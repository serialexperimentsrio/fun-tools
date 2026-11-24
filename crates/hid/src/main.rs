use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use common::paint;
use serde_json::{Map, Value};

/// Show information about this machine.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Only show these fields, comma-separated
    #[arg(short, long, value_enum, value_delimiter = ',', value_name = "LIST")]
    only: Vec<Field>,

    /// Print a JSON object instead
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Field {
    Host,
    Os,
    Kernel,
    Shell,
    Terminal,
    Cpu,
    Gpu,
    Ram,
    Resolution,
    Uptime,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut fields = if cli.only.is_empty() { Field::value_variants().to_vec() } else { cli.only };
        // `-o cpu,cpu` shows cpu once; the first mention sets the order.
        let mut seen = Vec::new();
        fields.retain(|f| {
            if seen.contains(f) {
                false
            } else {
                seen.push(*f);
                true
            }
        });
        let mut out = io::stdout().lock();

        if cli.json {
            let mut map = Map::new();
            for field in fields {
                map.insert(name(field), field.value().map_or(Value::Null, Value::String));
            }
            serde_json::to_writer_pretty(&mut out, &map)?;
            writeln!(out)?;
            return Ok(());
        }

        let color = common::color();
        for field in fields {
            let value = field.value().unwrap_or_else(|| paint("unknown", common::DIM, color));
            writeln!(out, "{}  {value}", paint(&format!("{:<10}", name(field)), common::BLUE, color))?;
        }
        Ok(())
    })
}

fn name(field: Field) -> String {
    field.to_possible_value().map(|v| v.get_name().to_string()).unwrap_or_default()
}

fn read(path: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.trim().to_string()).filter(|s| !s.is_empty())
}

fn is_wsl() -> bool {
    read("/proc/sys/kernel/osrelease").is_some_and(|r| r.to_lowercase().contains("microsoft"))
}

impl Field {
    fn value(self) -> Option<String> {
        match self {
            Field::Host => read("/proc/sys/kernel/hostname"),
            Field::Kernel => read("/proc/sys/kernel/osrelease"),
            Field::Os => {
                let release = fs::read_to_string("/etc/os-release").ok()?;
                let name = release
                    .lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())?;
                Some(if is_wsl() { format!("{name} (WSL)") } else { name })
            }
            Field::Shell => {
                let shell = env::var("SHELL").ok()?;
                Path::new(&shell).file_name().map(|n| n.to_string_lossy().into_owned())
            }
            Field::Terminal => env::var("TERM_PROGRAM")
                .ok()
                .or_else(|| env::var_os("WT_SESSION").map(|_| "Windows Terminal".to_string()))
                .or_else(|| env::var("TERM").ok()),
            Field::Cpu => {
                let info = fs::read_to_string("/proc/cpuinfo").ok()?;
                let model = info.lines().find_map(|l| l.strip_prefix("model name")?.split_once(':'))?.1;
                let threads = info.lines().filter(|l| l.starts_with("processor")).count();
                let model = model.split_whitespace().collect::<Vec<_>>().join(" ");
                Some(format!("{model} ({threads} threads)"))
            }
            Field::Gpu => gpus(),
            Field::Ram => {
                let mem = common::meminfo().ok()?;
                let total = *mem.get("MemTotal")?;
                let used = total - mem.get("MemAvailable").copied().unwrap_or(0);
                Some(format!(
                    "{} / {} ({}%)",
                    common::human_size(used),
                    common::human_size(total),
                    used * 100 / total.max(1)
                ))
            }
            Field::Resolution => resolutions(),
            Field::Uptime => {
                let secs = read("/proc/uptime")?.split_whitespace().next()?.parse::<f64>().ok()? as u64;
                let (days, hours, mins) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
                Some(match (days, hours) {
                    (0, 0) => format!("{mins}m"),
                    (0, _) => format!("{hours}h {mins}m"),
                    _ => format!("{days}d {hours}h {mins}m"),
                })
            }
        }
    }
}

/// Graphics cards from /sys/class/drm, named through pci.ids when it's installed.
fn gpus() -> Option<String> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // card0, card1... but not the connectors like card0-HDMI-A-1.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        let id = |file: &str| {
            fs::read_to_string(device.join(file)).ok().map(|s| s.trim().trim_start_matches("0x").to_lowercase())
        };
        if let (Some(vendor), Some(model)) = (id("vendor"), id("device")) {
            found.push(pci_name(&vendor, &model).unwrap_or_else(|| format!("PCI {vendor}:{model}")));
        }
    }
    // WSL doesn't expose the card itself, only a paravirtual device for the host's GPU.
    if found.is_empty() && Path::new("/dev/dxg").exists() {
        found.push("host GPU through WSL (/dev/dxg)".to_string());
    }
    (!found.is_empty()).then(|| found.join(", "))
}

fn pci_name(vendor: &str, device: &str) -> Option<String> {
    let db = ["/usr/share/misc/pci.ids", "/usr/share/hwdata/pci.ids"].iter().find_map(|p| fs::read_to_string(p).ok());
    let Some(db) = db else {
        let known = match vendor {
            "10de" => "NVIDIA",
            "1002" => "AMD",
            "8086" => "Intel",
            "1af4" => "Virtio",
            _ => return None,
        };
        return Some(format!("{known} {device}"));
    };

    // Vendors sit at the start of a line, their devices follow indented by one tab.
    let mut vendor_name = None;
    for line in db.lines() {
        match vendor_name {
            None => vendor_name = line.strip_prefix(vendor).filter(|r| r.starts_with(' ')).map(str::trim),
            Some(vendor_name) => {
                if !line.starts_with('\t') && !line.starts_with('#') && !line.is_empty() {
                    return Some(vendor_name.to_string());
                }
                if let Some(model) = line.strip_prefix('\t').and_then(|l| l.strip_prefix(device)) {
                    return Some(format!("{vendor_name} {}", model.trim()));
                }
            }
        }
    }
    vendor_name.map(String::from)
}

/// Current mode of every connected display, from the kernel's DRM connectors.
fn resolutions() -> Option<String> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let path = entry.path();
        if fs::read_to_string(path.join("status")).is_ok_and(|s| s.trim() == "connected") {
            if let Some(mode) =
                fs::read_to_string(path.join("modes")).ok().and_then(|m| m.lines().next().map(String::from))
            {
                found.push(mode);
            }
        }
    }
    (!found.is_empty()).then(|| found.join(", "))
}
