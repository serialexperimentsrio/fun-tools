use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use common::paint;
use serde_json::{Map, Value, json};

/// Show how much CPU, memory and disk is in use.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Only show these resources, comma-separated
    #[arg(short, long, value_enum, value_delimiter = ',', value_name = "LIST")]
    only: Vec<Resource>,

    /// Refresh every SECS seconds until Ctrl+C
    #[arg(short, long, value_name = "SECS")]
    watch: Option<f64>,

    /// Also list the N busiest processes
    #[arg(short, long, value_name = "N")]
    processes: Option<usize>,

    /// Print JSON instead (one object per line with --watch)
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Resource {
    Cpu,
    Ram,
    Swap,
    Disk,
}

/// CPU usage is a difference between two readings, taken this far apart.
const SAMPLE: Duration = Duration::from_millis(250);

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let Some(secs) = cli.watch else {
            return report(&cli);
        };
        let wait = Duration::try_from_secs_f64(secs).map_err(|_| "--watch needs a positive number")?;
        loop {
            // Clearing only makes sense on a terminal; in a log it's noise.
            if !cli.json && io::stdout().is_terminal() {
                print!("\x1b[H\x1b[2J");
            }
            report(&cli)?;
            sleep(wait.saturating_sub(SAMPLE));
        }
    })
}

fn report(cli: &Cli) -> common::Result {
    let wants = |r| cli.only.is_empty() || cli.only.contains(&r);
    let need_cpu = wants(Resource::Cpu) || cli.processes.is_some();

    let before = need_cpu.then(|| Ticks::read(cli.processes.is_some())).transpose()?;
    if before.is_some() {
        sleep(SAMPLE);
    }
    let after = need_cpu.then(|| Ticks::read(cli.processes.is_some())).transpose()?;
    let mem = common::meminfo()?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());

    // Rows for the screen, and the same numbers in the shape `--json` prints.
    let mut rows = Vec::new();
    let mut obj = Map::new();
    if let (Some(a), Some(b)) = (&before, &after) {
        if wants(Resource::Cpu) {
            let busy = (b.total - a.total) - (b.idle - a.idle);
            let total = (b.total - a.total).max(1);
            rows.push(Row::new("cpu", busy, total, format!("{threads} threads")));
            obj.insert(
                "cpu".into(),
                json!({ "percent": tenths(busy as f64 / total as f64 * 100.0), "threads": threads }),
            );
        }
    }
    let get = |key: &str| mem.get(key).copied().unwrap_or(0);
    if wants(Resource::Ram) {
        let total = get("MemTotal");
        let used = total - get("MemAvailable");
        rows.push(Row::sized("ram", used, total));
        obj.insert("ram".into(), json!({ "used": used, "total": total }));
    }
    if wants(Resource::Swap) {
        let total = get("SwapTotal");
        if total > 0 {
            let used = total - get("SwapFree");
            rows.push(Row::sized("swap", used, total));
            obj.insert("swap".into(), json!({ "used": used, "total": total }));
        } else {
            obj.insert("swap".into(), Value::Null);
        }
    }
    if wants(Resource::Disk) {
        let mut list = Vec::new();
        for (mount, used, total) in disks()? {
            rows.push(Row::sized(&format!("disk {}", mount.display()), used, total));
            list.push(json!({ "mount": mount.to_string_lossy(), "used": used, "total": total }));
        }
        obj.insert("disks".into(), list.into());
    }

    let mut busiest = Vec::new();
    if let (Some(n), Some(a), Some(b)) = (cli.processes, &before, &after) {
        let elapsed = (b.total - a.total).max(1) as f64;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        for (pid, now) in &b.procs {
            let then = a.procs.get(pid).map_or(now.ticks, |p| p.ticks);
            // Ticks in /proc/stat add up across all CPUs, so one busy core is 100%.
            let cpu = (now.ticks - then) as f64 / elapsed * threads as f64 * 100.0;
            busiest.push((*pid, cpu, now.rss_pages * page, now.name.clone()));
        }
        busiest.sort_by(|x, y| y.1.total_cmp(&x.1).then(y.2.cmp(&x.2)));
        busiest.truncate(n);
    }

    let mut out = io::stdout().lock();
    if cli.json {
        if cli.processes.is_some() {
            let list: Vec<Value> = busiest
                .iter()
                .map(|(pid, cpu, mem, name)| json!({ "pid": pid, "cpu": tenths(*cpu), "memory": mem, "name": name }))
                .collect();
            obj.insert("processes".into(), list.into());
        }
        if cli.watch.is_some() {
            serde_json::to_writer(&mut out, &obj)?;
        } else {
            serde_json::to_writer_pretty(&mut out, &obj)?;
        }
        writeln!(out)?;
        return Ok(out.flush()?);
    }

    let color = common::color();
    let width = rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
    for row in &rows {
        let pct = row.used as f64 / row.total.max(1) as f64 * 100.0;
        let label = paint(&format!("{:<width$}", row.label), common::BLUE, color);
        writeln!(out, "{label}  {}  {pct:>3.0}%  {}", bar(pct, color), paint(&row.detail, common::DIM, color))?;
    }
    if !busiest.is_empty() {
        writeln!(out)?;
        writeln!(out, "{}", paint(&format!("{:>7}  {:>6}  {:>9}  name", "pid", "cpu%", "memory"), common::DIM, color))?;
        for (pid, cpu, mem, name) in &busiest {
            writeln!(out, "{pid:>7}  {cpu:>6.1}  {:>9}  {name}", common::human_size(*mem))?;
        }
    }
    Ok(out.flush()?)
}

struct Row {
    label: String,
    used: u64,
    total: u64,
    detail: String,
}

impl Row {
    fn new(label: &str, used: u64, total: u64, detail: String) -> Row {
        Row { label: label.to_string(), used, total, detail }
    }

    fn sized(label: &str, used: u64, total: u64) -> Row {
        let detail = format!("{} / {}", common::human_size(used), common::human_size(total));
        Row::new(label, used, total, detail)
    }
}

/// Rounds a percentage to one decimal for `--json`.
fn tenths(pct: f64) -> f64 {
    (pct * 10.0).round() / 10.0
}

fn bar(pct: f64, color: bool) -> String {
    const WIDTH: usize = 20;
    let filled = ((pct / 100.0 * WIDTH as f64).round() as usize).min(WIDTH);
    let code = if pct >= 85.0 {
        common::RED
    } else if pct >= 60.0 {
        common::YELLOW
    } else {
        common::GREEN
    };
    format!("{}{}", paint(&"█".repeat(filled), code, color), paint(&"░".repeat(WIDTH - filled), common::DIM, color))
}

/// One reading of the CPU counters, optionally with every process's own.
struct Ticks {
    total: u64,
    idle: u64,
    procs: HashMap<u32, Proc>,
}

struct Proc {
    name: String,
    ticks: u64,
    rss_pages: u64,
}

impl Ticks {
    fn read(with_procs: bool) -> common::Result<Ticks> {
        let stat = fs::read_to_string("/proc/stat")?;
        let cpu: Vec<u64> =
            stat.lines().next().unwrap_or_default().split_whitespace().skip(1).filter_map(|n| n.parse().ok()).collect();
        // user nice system idle iowait irq softirq steal; guest time is already in user.
        let total = cpu.iter().take(8).sum();
        let idle = cpu.get(3).copied().unwrap_or(0) + cpu.get(4).copied().unwrap_or(0);

        let mut procs = HashMap::new();
        if with_procs {
            for entry in fs::read_dir("/proc")?.flatten() {
                let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
                    continue;
                };
                // Processes can exit between listing and reading; just skip them.
                if let Some(proc) = read_proc(&entry.path()) {
                    procs.insert(pid, proc);
                }
            }
        }
        Ok(Ticks { total, idle, procs })
    }
}

fn read_proc(dir: &Path) -> Option<Proc> {
    let stat = fs::read_to_string(dir.join("stat")).ok()?;
    // The name is in parentheses and may itself contain spaces or parentheses.
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat[open + 1..close].to_string();
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // Counting from the state field: utime is 11, stime 12, rss 21.
    let num = |i: usize| rest.get(i).and_then(|n| n.parse::<u64>().ok());
    Some(Proc { name, ticks: num(11)? + num(12)?, rss_pages: num(21)? })
}

/// Real filesystems from /proc/mounts as (mount point, used bytes, total bytes).
fn disks() -> common::Result<Vec<(PathBuf, u64, u64)>> {
    const PSEUDO: &[&str] = &[
        "autofs",
        "binfmt_misc",
        "bpf",
        "cgroup",
        "cgroup2",
        "configfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "efivarfs",
        "fusectl",
        "hugetlbfs",
        "mqueue",
        "nsfs",
        "overlay",
        "proc",
        "pstore",
        "ramfs",
        "rpc_pipefs",
        "securityfs",
        "selinuxfs",
        "squashfs",
        "sysfs",
        "tmpfs",
        "tracefs",
    ];
    let mounts = fs::read_to_string("/proc/mounts")?;
    // Keyed by the numbers statvfs reports, which also catches the same
    // filesystem mounted twice (bind mounts, or WSL showing C: at both /mnt/c
    // and /usr/lib/wsl/drivers). The shortest mount point wins.
    // Two different filesystems with exactly the same size and usage would
    // collapse into one row, which is unlikely enough to live with.
    let mut found: HashMap<(u64, u64, u64), (PathBuf, u64, u64)> = HashMap::new();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (Some(_device), Some(mount), Some(kind)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if PSEUDO.contains(&kind) {
            continue;
        }
        let mount = common::mount_path(mount);
        // Single files can be mount points too (WSL's /init); they aren't disks.
        if !mount.is_dir() {
            continue;
        }
        let Some(st) = statvfs(&mount) else { continue };
        let block = st.f_frsize;
        let total = st.f_blocks * block;
        if total == 0 {
            continue;
        }
        let row = (mount, (st.f_blocks - st.f_bfree) * block, total);
        let key = (st.f_blocks, st.f_bfree, st.f_files);
        match found.get(&key) {
            Some((kept, _, _)) if kept.as_os_str().len() <= row.0.as_os_str().len() => {}
            _ => {
                found.insert(key, row);
            }
        }
    }
    let mut found: Vec<_> = found.into_values().collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn statvfs(path: &Path) -> Option<libc::statvfs> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: c_path is a valid C string and st is only read after statvfs fills it in.
    let ok = unsafe { libc::statvfs(c_path.as_ptr(), st.as_mut_ptr()) } == 0;
    ok.then(|| unsafe { st.assume_init() })
}
