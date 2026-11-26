use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;
use clap::{Parser, Subcommand};
use common::{At, selection};

/// Delete files and directories safely. They go to a `.trash` folder on the
/// same drive (`~/.trash` for the drive your home is on), made the first time
/// something is trashed there, and can be restored until they're purged.
///
/// Anything trashed more than 30 days ago is purged the next time `del`
/// deletes something. Set FUN_TOOLS_TRASH_DAYS to change that, or to 0 to
/// keep everything. To delete a file named like a subcommand, write it as a
/// path or after `--`: `del ./purge`, `del -- "$file"`.
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
    /// Put trashed items back where they came from
    #[command(visible_alias = "restore")]
    Undel {
        /// Items in a trash folder, e.g. ~/.trash/2026-11-26_10-12-03_notes.txt
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Replace whatever has since taken the original path
        #[arg(short, long)]
        force: bool,
    },
    /// Delete trashed items for good, from every trash folder
    Purge {
        /// Only items trashed more than DAYS days ago
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,

        /// Show what would be deleted without touching anything
        #[arg(long)]
        dry_run: bool,

        /// Don't ask for confirmation (needed when there's no terminal to ask on)
        #[arg(short, long)]
        yes: bool,
    },
}

/// Where each trash folder keeps what it knows about its items.
const META: &str = ".meta";

/// One trashed item: `<trash>/<name>` is the file or directory itself and
/// `<trash>/.meta/<name>` holds when it was trashed, then where it came from.
struct Item {
    path: PathBuf,
    meta: PathBuf,
    origin: PathBuf,
    when: u64,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        match cli.action {
            Some(Action::Undel { paths, force }) => restore(&paths, force),
            Some(Action::Purge { older_than, dry_run, yes }) => {
                if fs::symlink_metadata("purge").is_ok() {
                    common::warn("running the purge subcommand; to trash the file named purge, use `del ./purge`");
                }
                purge(older_than, dry_run, yes)
            }
            None => {
                let paths = if cli.selected { selection::require()? } else { cli.paths };
                if !cli.dry_run {
                    expire()?;
                }
                add(&paths, cli.selected, cli.dry_run)
            }
        }
    })
}

fn add(paths: &[PathBuf], selected: bool, dry_run: bool) -> common::Result {
    let home_trash = home()?.join(".trash");
    let mut ok = true;
    let mut trashed = Vec::new();
    let mut out = io::stdout().lock();
    let terminal = out.is_terminal();

    for path in paths {
        match trash_one(path, &home_trash, dry_run) {
            Ok(()) if dry_run => {
                write!(out, "would trash ")?;
                common::write_path(&mut out, path, terminal)?;
                writeln!(out)?;
            }
            Ok(()) => trashed.push(path.clone()),
            Err(e) => {
                common::warn(e);
                ok = false;
            }
        }
    }

    if selected && !dry_run {
        selection::forget(&trashed)?;
    }
    common::finish(ok)
}

fn trash_one(path: &Path, home_trash: &Path, dry_run: bool) -> common::Result {
    if is_dot(path) {
        return Err(format!("{}: refusing to delete `.` or `..`", path.display()).into());
    }
    fs::symlink_metadata(path).at(path)?;
    let full = common::full_path(path).at(path)?;
    let name = full.file_name().ok_or_else(|| format!("{}: can't delete that", path.display()))?;
    let trash = trash_for(&full, home_trash)?;
    for t in [&trash, home_trash] {
        if t.starts_with(&full) {
            return Err(format!("{}: that would trash the trash itself", path.display()).into());
        }
        if full.starts_with(t) {
            return Err(format!("{}: already in the trash (see `del undel` and `del purge`)", path.display()).into());
        }
    }
    if dry_run {
        return Ok(());
    }

    // The top of a drive isn't always writable; the home trash always works,
    // it just means copying across drives.
    let trash = match make_trash(&trash) {
        Ok(()) => trash,
        Err(_) => {
            make_trash(home_trash).at(home_trash)?;
            home_trash.to_path_buf()
        }
    };
    let (item, meta) = claim(&trash, name, &full).at(&trash)?;
    if let Err(e) = common::move_path(&full, &item) {
        let _ = fs::remove_file(&meta);
        return Err(e).at(path);
    }
    Ok(())
}

/// The trash folder for something at `full`: `~/.trash` when it's on the
/// same drive as your home, otherwise `.trash` at the top of its own drive,
/// so trashing is a rename rather than a copy.
fn trash_for(full: &Path, home_trash: &Path) -> common::Result<PathBuf> {
    let dir = full.parent().unwrap_or(full);
    let dev = fs::metadata(dir).at(dir)?.dev();
    let home = home_trash.parent().unwrap_or(home_trash);
    if fs::metadata(home).at(home)?.dev() == dev {
        return Ok(home_trash.to_path_buf());
    }
    // Climb while still on the same drive; where that stops is its top.
    let mut top = dir;
    while let Some(up) = top.parent() {
        if !fs::metadata(up).is_ok_and(|m| m.dev() == dev) {
            break;
        }
        top = up;
    }
    Ok(top.join(".trash"))
}

/// Creates the trash folder and its .meta, private to you, if they're not there yet.
fn make_trash(trash: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(trash.join(META))
}

/// Picks the item's name in the trash: when it was trashed, then its own name,
/// like `2026-11-26_10-12-03_notes.txt`, so a listing sorts oldest first.
/// Two in the same second get `(2)` added. The name is claimed by creating
/// its .meta file; create_new keeps a second `del` at the same time from
/// taking it too.
fn claim(trash: &Path, name: &OsStr, origin: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let now = Local::now();
    let mut record = format!("{}\n", now.timestamp()).into_bytes();
    record.extend_from_slice(origin.as_os_str().as_bytes());
    // No colons: NTFS drives under /mnt can't store them.
    let mut stamped = OsString::from(now.format("%Y-%m-%d_%H-%M-%S_").to_string());
    stamped.push(name);
    for n in 1.. {
        let candidate = numbered(&stamped, n);
        let item = trash.join(&candidate);
        if fs::symlink_metadata(&item).is_ok() {
            continue;
        }
        let meta = trash.join(META).join(&candidate);
        match OpenOptions::new().write(true).create_new(true).open(&meta) {
            Ok(mut file) => {
                file.write_all(&record)?;
                return Ok((item, meta));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("ran out of numbers")
}

/// `notes.txt` for 1, `notes (2).txt` for 2, `build (3)` for a name without
/// an extension.
fn numbered(name: &OsStr, n: u32) -> OsString {
    if n == 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    let mut out = path.file_stem().unwrap_or(name).to_owned();
    out.push(format!(" ({n})"));
    if let Some(ext) = path.extension() {
        out.push(".");
        out.push(ext);
    }
    out
}

fn restore(paths: &[PathBuf], force: bool) -> common::Result {
    let mut ok = true;
    for path in paths {
        if let Err(e) = restore_one(path, force) {
            common::warn(e);
            ok = false;
        }
    }
    common::finish(ok)
}

fn restore_one(path: &Path, force: bool) -> common::Result {
    let full = common::full_path(path).at(path)?;
    let item =
        full.parent().zip(full.file_name()).and_then(|(trash, name)| read_item(trash, name)).ok_or_else(|| {
            format!("{}: not a trashed item (give its path in a trash folder; see `idx ~/.trash`)", path.display())
        })?;
    fs::symlink_metadata(&item.path).at(path)?;

    let origin = &item.origin;
    if !force && fs::symlink_metadata(origin).is_ok() {
        return Err(format!("{}: already exists (use -f to replace it)", origin.display()).into());
    }
    if let Some(parent) = origin.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    common::replace(origin, || common::move_path(&item.path, origin)).at(origin)?;
    fs::remove_file(&item.meta).at(&item.meta)
}

fn purge(older_than: Option<u64>, dry_run: bool, yes: bool) -> common::Result {
    let cutoff = older_than.map_or(u64::MAX, |days| now().saturating_sub(days.saturating_mul(86_400)));
    let doomed: Vec<Item> = all_items()?.into_iter().filter(|i| i.when <= cutoff).collect();

    if dry_run {
        let mut out = io::stdout().lock();
        let terminal = out.is_terminal();
        for item in &doomed {
            write!(out, "would delete ")?;
            common::write_path(&mut out, &item.path, terminal)?;
            writeln!(out)?;
        }
        return Ok(());
    }

    // This is the one step that can't be undone. A script has to say so
    // with --yes, since there's nobody to ask.
    if !yes && !doomed.is_empty() {
        if !io::stdin().is_terminal() {
            return Err("purge needs --yes when there's no terminal to ask on".into());
        }
        eprint!("Permanently delete {} trashed {}? [y/N] ", doomed.len(), plural(doomed.len()));
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(());
        }
    }

    let (removed, ok) = remove_items(doomed);
    println!("deleted {removed} {}", plural(removed));
    common::finish(ok)
}

/// Purges what was trashed more than FUN_TOOLS_TRASH_DAYS (default 30) days
/// ago. Runs before each delete, so the trash can't grow forever.
fn expire() -> common::Result {
    let days: u64 = match env::var("FUN_TOOLS_TRASH_DAYS") {
        Ok(v) => v.trim().parse().map_err(|_| format!("FUN_TOOLS_TRASH_DAYS: `{v}` isn't a number of days"))?,
        Err(_) => 30,
    };
    if days == 0 {
        return Ok(());
    }
    let cutoff = now().saturating_sub(days.saturating_mul(86_400));
    let old: Vec<Item> = all_items()?.into_iter().filter(|i| i.when < cutoff).collect();
    if old.is_empty() {
        return Ok(());
    }
    // Failures are reported as they happen and shouldn't stop the delete.
    let (removed, _) = remove_items(old);
    common::warn(format!("purged {removed} {} trashed more than {days} days ago", plural(removed)));
    Ok(())
}

/// Deletes items and their .meta files. Returns how many went and whether all did.
fn remove_items(items: Vec<Item>) -> (usize, bool) {
    let (mut removed, mut ok) = (0, true);
    for item in items {
        // Data already gone (removed by hand) still leaves a .meta to clean up.
        let data = match common::remove_path(&item.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
        match data.and_then(|()| fs::remove_file(&item.meta)).at(&item.path) {
            Ok(()) => removed += 1,
            Err(e) => {
                common::warn(e);
                ok = false;
            }
        }
    }
    (removed, ok)
}

/// Every item in every trash folder: `~/.trash` and `.trash` at the top of
/// each mounted drive that has one.
fn all_items() -> common::Result<Vec<Item>> {
    let mut trashes = vec![home()?.join(".trash")];
    for mount in common::mount_points() {
        let trash = mount.join(".trash");
        // A bind mount (like /run/shm for /dev/shm) shows the same trash twice.
        if trash.join(META).is_dir() && !trashes.iter().any(|t| common::same_file(t, &trash)) {
            trashes.push(trash);
        }
    }
    let mut items = Vec::new();
    for trash in &trashes {
        let Ok(list) = fs::read_dir(trash.join(META)) else { continue };
        items.extend(list.flatten().filter_map(|e| read_item(trash, &e.file_name())));
    }
    Ok(items)
}

/// Reads `<trash>/.meta/<name>`. None when there's no such record or it
/// doesn't look like one, so stray files are left alone.
fn read_item(trash: &Path, name: &OsStr) -> Option<Item> {
    let meta = trash.join(META).join(name);
    let record = fs::read(&meta).ok()?;
    let split = record.iter().position(|&b| b == b'\n')?;
    let when = std::str::from_utf8(&record[..split]).ok()?.parse().ok()?;
    let origin = PathBuf::from(OsStr::from_bytes(&record[split + 1..]));
    Some(Item { path: trash.join(name), meta, origin, when })
}

/// Your home directory with symlinks resolved, so it compares cleanly with
/// the resolved paths being deleted.
fn home() -> common::Result<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    Ok(fs::canonicalize(&home).unwrap_or(home))
}

/// True for `.`, `..` and anything ending in them, like `src/..`. Deleting
/// those would take the current or parent directory with it.
fn is_dot(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    let trimmed = bytes.strip_suffix(b"/").unwrap_or(bytes);
    let last = trimmed.rsplit(|&b| b == b'/').next().unwrap_or(trimmed);
    last == b"." || last == b".."
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots() {
        for p in [".", "..", "./", "../", "a/.", "a/..", "/tmp/.."] {
            assert!(is_dot(Path::new(p)), "{p}");
        }
        for p in ["a", ".hidden", "..x", "a/.b", "/"] {
            assert!(!is_dot(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn numbers_clashing_names() {
        let n = |name: &str, i| numbered(OsStr::new(name), i).into_string().unwrap();
        assert_eq!(n("notes.txt", 1), "notes.txt");
        assert_eq!(n("notes.txt", 2), "notes (2).txt");
        assert_eq!(n("build", 3), "build (3)");
        assert_eq!(n(".bashrc", 2), ".bashrc (2)");
    }
}
