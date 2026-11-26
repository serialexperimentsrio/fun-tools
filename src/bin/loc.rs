use std::fs;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use common::{At, paint};
use serde_json::{Value, json};

/// Find files and directories by name below a directory.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Part of the name to look for, case-insensitive. Use * and ? for a full-name
    /// pattern. With a `/` in it, it's matched against the path below ROOT
    pattern: String,

    /// Where to start searching (defaults to the current directory)
    root: Option<PathBuf>,

    /// Include hidden entries and look inside hidden directories
    #[arg(short, long)]
    all: bool,

    /// Only match this kind of entry
    #[arg(short = 't', long = "type", value_enum, value_name = "KIND")]
    kind: Option<Kind>,

    /// How many directory levels to go down
    #[arg(long, value_name = "N")]
    depth: Option<usize>,

    /// Print a JSON array instead
    #[arg(long)]
    json: bool,

    /// Also search other drives mounted below ROOT (like /mnt/c from /)
    #[arg(long)]
    cross_drives: bool,
}

/// Named the same as the `type` field in `--json`.
#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Kind {
    File,
    Dir,
    Link,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Link => "link",
        }
    }
}

struct Search {
    needle: String,
    pattern: Vec<char>,
    wildcard: bool,
    /// A pattern with a `/` is matched against the path below ROOT, not just the name.
    by_path: bool,
    root: PathBuf,
    /// The drive ROOT is on; others are skipped unless --cross-drives.
    root_dev: u64,
    cli: Cli,
    out: BufWriter<io::StdoutLock<'static>>,
    color: bool,
    terminal: bool,
    json: Vec<Value>,
    ok: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let root = cli.root.clone().unwrap_or_else(|| PathBuf::from("."));
        let needle = cli.pattern.to_lowercase();
        let mut search = Search {
            pattern: needle.chars().collect(),
            needle,
            wildcard: cli.pattern.contains(['*', '?']),
            by_path: cli.pattern.contains('/'),
            root_dev: fs::metadata(&root).at(&root)?.dev(),
            root: root.clone(),
            out: BufWriter::new(io::stdout().lock()),
            color: common::color(),
            terminal: io::stdout().is_terminal(),
            json: Vec::new(),
            ok: true,
            cli,
        };
        search.walk(&root, search.cli.depth.unwrap_or(usize::MAX))?;
        if search.cli.json {
            serde_json::to_writer_pretty(&mut search.out, &search.json)?;
            writeln!(search.out)?;
        }
        search.out.flush()?;
        common::finish(search.ok)
    })
}

impl Search {
    fn walk(&mut self, dir: &Path, depth_left: usize) -> common::Result {
        if depth_left == 0 {
            return Ok(());
        }
        for item in fs::read_dir(dir).at(dir)? {
            let item = item.at(dir)?;
            let name = item.file_name().to_string_lossy().to_lowercase();
            if !self.cli.all && name.starts_with('.') {
                continue;
            }
            // file_type() doesn't follow symlinks, so linked directories aren't walked twice.
            let file_type = item.file_type().at(&item.path())?;
            let is_dir = file_type.is_dir();
            let kind = common::kind(file_type);
            let path = item.path();

            let kind_ok = self.cli.kind.is_none_or(|k| k.name() == kind);
            let subject = if self.by_path {
                path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().to_lowercase()
            } else {
                name
            };
            if kind_ok && self.matches(&subject) {
                let shown = path.strip_prefix("./").unwrap_or(&path);
                if self.cli.json {
                    self.json.push(json!({ "path": shown.to_string_lossy(), "type": kind }));
                } else {
                    if is_dir && self.color {
                        let text = common::escaped(shown.as_os_str());
                        write!(self.out, "{}", paint(&text, common::BLUE, true))?;
                    } else {
                        common::write_path(&mut self.out, shown, self.terminal)?;
                    }
                    writeln!(self.out)?;
                }
            }

            // Another drive mounted here (/mnt/c below /) can be huge or slow,
            // so stay on the one ROOT is on unless asked.
            let same_drive = || fs::symlink_metadata(&path).is_ok_and(|m| m.dev() == self.root_dev);
            if is_dir && (self.cli.cross_drives || same_drive()) {
                if let Err(e) = self.walk(&path, depth_left - 1) {
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

    fn matches(&self, name: &str) -> bool {
        if self.wildcard {
            wildcard(&self.pattern, &name.chars().collect::<Vec<_>>(), self.by_path)
        } else {
            name.contains(&self.needle)
        }
    }
}

/// Matches a whole name against a pattern with `*` (any run) and `?` (one char).
/// Backtracks to the last `*` on a mismatch, so it's linear for typical patterns.
/// With `within_dir`, neither one matches `/`, so `src/*.rs` stays in src.
fn wildcard(pattern: &[char], name: &[char], within_dir: bool) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if (c == '?' && !(within_dir && name[n] == '/')) || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((_, sn)) if within_dir && name[sn] == '/' => return false,
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[test]
fn wildcards() {
    let chars = |s: &str| s.chars().collect::<Vec<_>>();
    let m = |p: &str, n: &str| wildcard(&chars(p), &chars(n), false);
    let in_dir = |p: &str, n: &str| wildcard(&chars(p), &chars(n), true);
    assert!(m("*.rs", "main.rs"));
    assert!(!m("*.rs", "main.rsx"));
    assert!(m("ma?n.*", "main.rs"));
    assert!(m("*", ""));
    assert!(m("a*b*c", "aXXbYYc"));
    assert!(!m("a*b*c", "aXXbYY"));
    assert!(m("**x", "abx"));
    assert!(in_dir("src/*.rs", "src/main.rs"));
    assert!(!in_dir("src/*.rs", "src/deep/main.rs"));
    assert!(!in_dir("src?main.rs", "src/main.rs"));
    assert!(in_dir("*/*/main.rs", "src/deep/main.rs"));
}
