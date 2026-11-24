use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use common::{At, paint, selection};

/// Delete files and directories safely. They go to a dump first and can be
/// restored until the dump is emptied. To delete a file named like one of
/// the subcommands, write it as a path: `del ./ls`.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    action: Option<Action>,

    /// Files and directories to delete
    #[arg(required_unless_present = "selected", conflicts_with = "selected")]
    paths: Vec<PathBuf>,

    /// Delete the selected paths (see `sel`)
    #[arg(short, long)]
    selected: bool,

    /// Show what would be deleted without touching anything
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum Action {
    /// List what's in the dump
    Ls,
    /// Put dumped entries back where they came from
    Restore {
        /// Entry ids, as shown by `del ls`
        #[arg(required = true)]
        ids: Vec<u64>,

        /// Replace whatever has since taken the original path
        #[arg(short, long)]
        force: bool,
    },
    /// Delete dumped entries for good
    Empty {
        /// Only entries dumped more than DAYS days ago
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,

        /// Show what would be deleted without touching anything
        #[arg(long)]
        dry_run: bool,

        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
}

/// One dumped path. On disk it's `dump/<id>/data` (the file or directory
/// itself) next to `dump/<id>/origin` (when it was dumped and where from).
struct Entry {
    id: u64,
    dir: PathBuf,
    origin: PathBuf,
    when: u64,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let dump = common::data_dir()?.join("dump");
        fs::create_dir_all(&dump).at(&dump)?;

        match cli.action {
            Some(Action::Ls) => list(&dump),
            Some(Action::Restore { ids, force }) => restore(&dump, &ids, force),
            Some(Action::Empty { older_than, dry_run, yes }) => empty(&dump, older_than, dry_run, yes),
            None => {
                let paths = if cli.selected { selection::require()? } else { cli.paths };
                add(&dump, &paths, cli.selected, cli.dry_run)
            }
        }
    })
}

fn add(dump: &Path, paths: &[PathBuf], selected: bool, dry_run: bool) -> common::Result {
    let mut next_id = entries(dump)?.last().map_or(1, |e| e.id + 1);
    let mut ok = true;
    let mut dumped = Vec::new();
    let mut out = io::stdout().lock();

    for path in paths {
        let result = (|| -> common::Result<u64> {
            fs::symlink_metadata(path).at(path)?;
            let full = common::full_path(path).at(path)?;
            if dump.starts_with(&full) {
                return Err(format!("{}: that would dump the dump itself", path.display()).into());
            }
            if dry_run {
                return Ok(0);
            }

            // create_dir fails if the id is taken, which also covers two `del` runs at once.
            let dir = loop {
                let dir = dump.join(next_id.to_string());
                next_id += 1;
                match fs::create_dir(&dir) {
                    Ok(()) => break dir,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e).at(&dir),
                }
            };
            let mut origin = format!("{}\n", now()).into_bytes();
            origin.extend_from_slice(full.as_os_str().as_bytes());
            fs::write(dir.join("origin"), origin).at(&dir)?;

            if let Err(e) = common::move_path(&full, &dir.join("data")) {
                let _ = fs::remove_dir_all(&dir);
                return Err(e).at(path);
            }
            Ok(next_id - 1)
        })();

        match result {
            Ok(_) if dry_run => writeln!(out, "would dump {}", path.display())?,
            Ok(id) => {
                writeln!(out, "{id:>4}  {}", path.display())?;
                dumped.push(path.clone());
            }
            Err(e) => {
                common::warn(e);
                ok = false;
            }
        }
    }

    if selected && !dry_run {
        selection::forget(&dumped)?;
    }
    common::finish(ok)
}

fn list(dump: &Path) -> common::Result {
    let color = common::color();
    let mut out = io::stdout().lock();
    for entry in entries(dump)? {
        let when = Local.timestamp_opt(entry.when as i64, 0).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string());
        let when = paint(&when.unwrap_or_default(), common::DIM, color);
        writeln!(out, "{:>4}  {when}  {}", entry.id, entry.origin.display())?;
    }
    Ok(())
}

fn restore(dump: &Path, ids: &[u64], force: bool) -> common::Result {
    let all = entries(dump)?;
    let mut ok = true;
    for id in ids {
        let result = (|| -> common::Result {
            let entry = all.iter().find(|e| e.id == *id).ok_or_else(|| format!("{id}: no such entry"))?;
            let origin = &entry.origin;
            if !force && fs::symlink_metadata(origin).is_ok() {
                return Err(format!("{}: already exists (use -f to replace it)", origin.display()).into());
            }
            if let Some(parent) = origin.parent() {
                fs::create_dir_all(parent).at(parent)?;
            }
            common::replace(origin, || common::move_path(&entry.dir.join("data"), origin)).at(origin)?;
            fs::remove_dir_all(&entry.dir).at(&entry.dir)
        })();
        if let Err(e) = result {
            common::warn(e);
            ok = false;
        }
    }
    common::finish(ok)
}

fn empty(dump: &Path, older_than: Option<u64>, dry_run: bool, yes: bool) -> common::Result {
    let cutoff = older_than.map_or(u64::MAX, |days| now().saturating_sub(days * 86_400));
    let doomed: Vec<Entry> = entries(dump)?.into_iter().filter(|e| e.when <= cutoff).collect();

    // This is the one step that can't be undone, so ask when someone is at the keyboard.
    if !dry_run && !yes && !doomed.is_empty() && io::stdin().is_terminal() {
        print!("Permanently delete {} dumped {}? [y/N] ", doomed.len(), plural(doomed.len()));
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(());
        }
    }

    let mut ok = true;
    let mut removed = 0;
    for entry in doomed {
        if dry_run {
            println!("would delete {:>4}  {}", entry.id, entry.origin.display());
            continue;
        }
        match fs::remove_dir_all(&entry.dir).at(&entry.dir) {
            Ok(()) => removed += 1,
            Err(e) => {
                common::warn(e);
                ok = false;
            }
        }
    }
    if !dry_run {
        println!("deleted {removed} {}", plural(removed));
    }
    common::finish(ok)
}

/// Every entry in the dump, oldest id first. Anything that doesn't look
/// like an entry is skipped rather than treated as an error.
fn entries(dump: &Path) -> common::Result<Vec<Entry>> {
    let mut found = Vec::new();
    for item in fs::read_dir(dump).at(dump)? {
        let dir = item.at(dump)?.path();
        let Some(id) = dir.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(origin) = fs::read(dir.join("origin")) else {
            continue;
        };
        let Some(split) = origin.iter().position(|&b| b == b'\n') else {
            continue;
        };
        let when = std::str::from_utf8(&origin[..split]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let origin = PathBuf::from(OsStr::from_bytes(&origin[split + 1..]));
        found.push(Entry { id, dir, origin, when });
    }
    found.sort_by_key(|e| e.id);
    Ok(found)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "entry" } else { "entries" }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
