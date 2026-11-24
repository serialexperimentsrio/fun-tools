use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use common::{At, paint};

/// Find files and directories by name below a directory.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Part of the name to look for, case-insensitive. Use * and ? for a full-name pattern
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
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Kind {
    File,
    Dir,
}

struct Search {
    needle: String,
    pattern: Vec<char>,
    wildcard: bool,
    cli: Cli,
    out: BufWriter<io::StdoutLock<'static>>,
    color: bool,
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
            out: BufWriter::new(io::stdout().lock()),
            color: common::color(),
            ok: true,
            cli,
        };
        search.walk(&root, search.cli.depth.unwrap_or(usize::MAX))?;
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
            let is_dir = item.file_type().at(&item.path())?.is_dir();
            let path = item.path();

            let kind_ok = match self.cli.kind {
                Some(Kind::Dir) => is_dir,
                Some(Kind::File) => !is_dir,
                None => true,
            };
            if kind_ok && self.matches(&name) {
                let shown = path.strip_prefix("./").unwrap_or(&path).display().to_string();
                let shown = if is_dir { paint(&shown, common::BLUE, self.color) } else { shown };
                writeln!(self.out, "{shown}")?;
            }

            if is_dir {
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
            wildcard(&self.pattern, &name.chars().collect::<Vec<_>>())
        } else {
            name.contains(&self.needle)
        }
    }
}

/// Matches a whole name against a pattern with `*` (any run) and `?` (one char).
/// Backtracks to the last `*` on a mismatch, so it's linear for typical patterns.
fn wildcard(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
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
    let m = |p: &str, n: &str| wildcard(&p.chars().collect::<Vec<_>>(), &n.chars().collect::<Vec<_>>());
    assert!(m("*.rs", "main.rs"));
    assert!(!m("*.rs", "main.rsx"));
    assert!(m("ma?n.*", "main.rs"));
    assert!(m("*", ""));
    assert!(m("a*b*c", "aXXbYYc"));
    assert!(!m("a*b*c", "aXXbYY"));
    assert!(m("**x", "abx"));
}
