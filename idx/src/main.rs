use std::cmp::Reverse;
use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use clap::{Parser, ValueEnum};
use common::{At, paint};
use serde_json::{Value, json};

/// List what's in a directory. Directories come first.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Directory to list (defaults to the current one)
    path: Option<PathBuf>,

    /// Include hidden entries
    #[arg(short, long)]
    all: bool,

    /// Show permissions, size and modification time
    #[arg(short, long)]
    long: bool,

    /// Walk into subdirectories and draw a tree
    #[arg(short, long)]
    recursive: bool,

    /// How deep to walk. Implies --recursive
    #[arg(long, value_name = "N")]
    depth: Option<usize>,

    /// Sort order inside each directory
    #[arg(long, value_enum, default_value_t = Sort::Name, value_name = "KEY")]
    sort: Sort,

    /// Print a JSON array instead
    #[arg(long, conflicts_with = "long")]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Sort {
    Name,
    /// Largest first
    Size,
    /// Newest first
    Time,
}

struct Entry {
    name: OsString,
    path: PathBuf,
    meta: Metadata,
}

impl Entry {
    fn modified(&self) -> SystemTime {
        self.meta.modified().unwrap_or(UNIX_EPOCH)
    }
}

struct Lister<'a> {
    cli: &'a Cli,
    out: BufWriter<io::StdoutLock<'static>>,
    color: bool,
    json: Vec<Value>,
    ok: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
        let meta = fs::symlink_metadata(&root).at(&root)?;

        let mut lister = Lister {
            cli: &cli,
            out: BufWriter::new(io::stdout().lock()),
            color: common::color(),
            json: Vec::new(),
            ok: true,
        };

        if meta.is_dir() {
            let max_depth = match (cli.depth, cli.recursive) {
                (Some(n), _) => n,
                (None, true) => usize::MAX,
                (None, false) => 1,
            };
            lister.list(&root, max_depth, "")?;
        } else {
            let name = root.file_name().map(OsString::from).unwrap_or_else(|| root.clone().into());
            lister.show(&Entry { name, path: root.clone(), meta }, "")?;
        }

        if cli.json {
            serde_json::to_writer_pretty(&mut lister.out, &lister.json)?;
            writeln!(lister.out)?;
        }
        lister.out.flush()?;
        common::finish(lister.ok)
    })
}

impl Lister<'_> {
    fn list(&mut self, dir: &Path, depth_left: usize, prefix: &str) -> common::Result {
        if depth_left == 0 {
            return Ok(());
        }
        let entries = self.read(dir)?;
        let tree = self.cli.recursive || self.cli.depth.is_some();
        let count = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            let last = i + 1 == count;
            let branch = match (tree, last) {
                (false, _) => "",
                (true, false) => "├── ",
                (true, true) => "└── ",
            };
            self.show(entry, &format!("{prefix}{branch}"))?;
            if tree && entry.meta.is_dir() {
                let next = format!("{prefix}{}", if last { "    " } else { "│   " });
                // A subdirectory we can't open shouldn't stop the rest of the tree.
                if let Err(e) = self.list(&entry.path, depth_left - 1, &next) {
                    if common::broken_pipe(&*e) {
                        return Err(e);
                    }
                    self.out.flush()?;
                    common::warn(e);
                    self.ok = false;
                }
            }
        }
        Ok(())
    }

    fn read(&self, dir: &Path) -> common::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(dir).at(dir)? {
            let item = item.at(dir)?;
            let name = item.file_name();
            if !self.cli.all && name.as_encoded_bytes().starts_with(b".") {
                continue;
            }
            let path = item.path();
            // symlink_metadata so links show up as links and the tree never follows them.
            let meta = fs::symlink_metadata(&path).at(&path)?;
            entries.push(Entry { name, path, meta });
        }

        entries.sort_by_cached_key(|e| e.name.to_string_lossy().to_lowercase());
        match self.cli.sort {
            Sort::Name => {}
            Sort::Size => entries.sort_by_key(|e| Reverse(e.meta.len())),
            Sort::Time => entries.sort_by_key(|e| Reverse(e.modified())),
        }
        // Stable sort, so the order above survives within each group.
        entries.sort_by_key(|e| !e.meta.is_dir());
        Ok(entries)
    }

    fn show(&mut self, entry: &Entry, prefix: &str) -> common::Result {
        let meta = &entry.meta;
        let mode = meta.permissions().mode();

        if self.cli.json {
            let kind = if meta.is_dir() {
                "dir"
            } else if meta.is_symlink() {
                "link"
            } else {
                "file"
            };
            let modified = entry.modified().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            self.json.push(json!({
                "path": entry.path.to_string_lossy(),
                "name": entry.name.to_string_lossy(),
                "type": kind,
                "size": meta.len(),
                "modified": modified,
                "mode": format!("{:o}", mode & 0o7777),
            }));
            return Ok(());
        }

        let mut name = entry.name.to_string_lossy().into_owned();
        let code = if meta.is_dir() {
            name.push('/');
            common::BLUE
        } else if meta.is_symlink() {
            common::CYAN
        } else if mode & 0o111 != 0 {
            common::GREEN
        } else {
            ""
        };
        let mut line = if code.is_empty() { name } else { paint(&name, code, self.color) };
        if meta.is_symlink() {
            if let Ok(target) = fs::read_link(&entry.path) {
                line.push_str(&format!(" -> {}", target.display()));
            }
        }

        if self.cli.long {
            let size = if meta.is_dir() { "-".to_string() } else { common::human_size(meta.len()) };
            let time = DateTime::<Local>::from(entry.modified()).format("%Y-%m-%d %H:%M");
            let info = format!("{} {size:>9}  {time}", permissions(meta));
            write!(self.out, "{}  ", paint(&info, common::DIM, self.color))?;
        }
        writeln!(self.out, "{prefix}{line}")?;
        Ok(())
    }
}

/// `drwxr-xr-x` style string.
fn permissions(meta: &Metadata) -> String {
    let kind = if meta.is_dir() {
        'd'
    } else if meta.is_symlink() {
        'l'
    } else {
        '-'
    };
    let mode = meta.permissions().mode();
    let mut s = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}
