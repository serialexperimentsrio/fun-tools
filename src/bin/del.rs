use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;
use clap::{Parser, Subcommand};
use common::{At, selection};

/// Delete files and directories safely. They go to a trash folder on the
/// same drive, made the first time something is trashed there, and can be
/// restored until they're purged: `~/.trash` for the drive your home is on,
/// and `.trash/<your uid>` at the top of any other drive. root always uses
/// the top of the drive, like `/.trash/0`.
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
        /// Items in a trash folder, e.g. ~/.trash/2026-11-26/notes.txt
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
    let home_trash = home_trash()?;
    let mut ok = true;
    let mut trashed = Vec::new();
    let mut out = io::stdout().lock();
    let terminal = out.is_terminal();

    for path in paths {
        match trash_one(path, home_trash.as_deref(), dry_run) {
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

fn trash_one(path: &Path, home_trash: Option<&Path>, dry_run: bool) -> common::Result {
    if is_dot(path) {
        return Err(format!("{}: refusing to delete `.` or `..`", path.display()).into());
    }
    fs::symlink_metadata(path).at(path)?;
    let full = common::full_path(path).at(path)?;
    let name = full.file_name().ok_or_else(|| format!("{}: can't delete that", path.display()))?;

    // Where it should go, then the home trash in case that doesn't work out:
    // the top of a drive may not be writable, or its .trash may belong to
    // someone else. The home trash always works, it just means a copy.
    let preferred = trash_for(&full, home_trash)?;
    let mut trashes = vec![preferred.clone()];
    if let Some(home) = home_trash.filter(|h| *h != preferred) {
        trashes.push(home.to_path_buf());
    }
    for trash in &trashes {
        // For a drive's trash, the shared .trash above your own folder counts too.
        let shared =
            if Some(trash.as_path()) == home_trash { trash.as_path() } else { trash.parent().unwrap_or(trash) };
        if shared.starts_with(&full) {
            return Err(format!("{}: that would trash the trash itself", path.display()).into());
        }
        if full.starts_with(trash) {
            return Err(format!("{}: already in the trash (see `del undel` and `del purge`)", path.display()).into());
        }
    }
    if dry_run {
        return Ok(());
    }

    let mut failed = None;
    for trash in &trashes {
        let shared = Some(trash.as_path()) != home_trash;
        match prepare(trash, shared).and_then(|()| claim(trash, name, &full)) {
            Ok((item, meta)) => {
                if let Err(e) = common::move_path(&full, &item) {
                    let _ = fs::remove_file(&meta);
                    return Err(e).at(path);
                }
                return Ok(());
            }
            Err(e) => failed = Some((trash, e)),
        }
    }
    let Some((trash, e)) = failed else { unreachable!("there's always at least one trash to try") };
    Err(e).at(trash)
}

/// The trash folder for something at `full`: `~/.trash` when it's on the
/// same drive as your home, otherwise `.trash/<uid>` at the top of its own
/// drive, so trashing is a rename rather than a copy. root has no home
/// trash and always uses the top of the drive.
fn trash_for(full: &Path, home_trash: Option<&Path>) -> common::Result<PathBuf> {
    let dir = full.parent().unwrap_or(full);
    let dev = fs::metadata(dir).at(dir)?.dev();
    if let Some(home_trash) = home_trash {
        let home = home_trash.parent().unwrap_or(home_trash);
        if fs::metadata(home).at(home)?.dev() == dev {
            return Ok(home_trash.to_path_buf());
        }
    }
    // Climb while still on the same drive; where that stops is its top.
    let mut top = dir;
    while let Some(up) = top.parent() {
        if !fs::metadata(up).is_ok_and(|m| m.dev() == dev) {
            break;
        }
        top = up;
    }
    Ok(top.join(".trash").join(uid().to_string()))
}

/// Makes the trash folder and its .meta if they're not there yet, private
/// to you. A drive's trash sits in a shared `.trash` that works like /tmp:
/// anyone can add their own folder, nobody can touch anyone else's. Your
/// folder must be a real folder you own, so nobody can set one up to catch
/// what you delete.
fn prepare(trash: &Path, shared: bool) -> io::Result<()> {
    let private = || DirBuilder::new().mode(0o700).create(trash).or_else(exists_ok);
    if shared {
        let parent = trash.parent().unwrap_or(trash);
        match fs::create_dir(parent) {
            // umask would strip the sticky, world-writable bits, so set them after.
            // Drives that can't store permissions (some /mnt setups) just ignore it.
            Ok(()) => drop(fs::set_permissions(parent, fs::Permissions::from_mode(0o1777))),
            Err(e) => exists_ok(e)?,
        }
        if !fs::symlink_metadata(parent)?.is_dir() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the shared .trash isn't a real folder"));
        }
        private()?;
        let own = fs::symlink_metadata(trash)?;
        if !own.is_dir() || own.uid() != uid() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "that trash folder isn't yours"));
        }
    } else {
        private()?;
    }
    DirBuilder::new().mode(0o700).create(trash.join(META)).or_else(exists_ok)
}

fn exists_ok(e: io::Error) -> io::Result<()> {
    if e.kind() == io::ErrorKind::AlreadyExists { Ok(()) } else { Err(e) }
}

/// Picks the item's place in the trash: a folder for the day it was trashed,
/// then its own name, like `2026-11-26/notes.txt`. A second `notes.txt` the
/// same day gets `(2)` added. The name is claimed by creating its record in
/// `.meta/<day>/`; create_new keeps a second `del` running at the same time
/// from taking it too.
fn claim(trash: &Path, name: &OsStr, origin: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let now = Local::now();
    let mut record = format!("{}\n", now.timestamp()).into_bytes();
    record.extend_from_slice(origin.as_os_str().as_bytes());
    let day = now.format("%Y-%m-%d").to_string();
    for dir in [trash.join(&day), trash.join(META).join(&day)] {
        DirBuilder::new().mode(0o700).create(dir).or_else(exists_ok)?;
    }
    for n in 1.. {
        let candidate = Path::new(&day).join(item_name(name, n));
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

/// The longest name a Linux filesystem takes, in bytes.
const NAME_MAX: usize = 255;

/// `notes.txt` for 1, `notes (2).txt` for 2, `build (3)` for a name without
/// an extension. A name with no room left for the number is shortened,
/// keeping its extension; `.meta` still has the full original path.
fn item_name(name: &OsStr, n: u32) -> OsString {
    if n == 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    // A very long "extension" is really part of the name.
    let ext = path.extension().filter(|e| e.len() <= 32);
    let stem = if ext.is_some() { path.file_stem().unwrap_or(name) } else { name };
    let suffix = format!(" ({n})");
    let ext_len = ext.map_or(0, |e| e.len() + 1);
    let room = NAME_MAX.saturating_sub(suffix.len() + ext_len);

    let mut out = OsStr::from_bytes(cut(stem.as_bytes(), room)).to_owned();
    out.push(suffix);
    if let Some(ext) = ext {
        out.push(".");
        out.push(ext);
    }
    out
}

/// The first `max` bytes, backing off so a UTF-8 character isn't split in half.
fn cut(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let mut end = max;
    while end > 0 && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    &bytes[..end]
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
    // `<trash>/<day>/<name>`, so the trash is two levels up.
    let item = (|| {
        let day_dir = full.parent()?;
        let rel = Path::new(day_dir.file_name()?).join(full.file_name()?);
        read_item(day_dir.parent()?, &rel)
    })()
    .ok_or_else(|| {
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
    fs::remove_file(&item.meta).at(&item.meta)?;
    tidy_days(&item);
    Ok(())
}

/// Removes the item's day folders once they're empty, so the trash doesn't
/// fill up with empty dates. remove_dir refuses a folder with anything in it.
fn tidy_days(item: &Item) {
    for dir in [item.path.parent(), item.meta.parent()].into_iter().flatten() {
        let _ = fs::remove_dir(dir);
    }
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
            Ok(()) => {
                removed += 1;
                tidy_days(&item);
            }
            Err(e) => {
                common::warn(e);
                ok = false;
            }
        }
    }
    (removed, ok)
}

/// Every item in every trash folder of yours: `~/.trash` and `.trash/<uid>`
/// at the top of each local drive that has one. Network drives are skipped,
/// since one that stopped answering would hang every `del`.
fn all_items() -> common::Result<Vec<Item>> {
    let mut trashes: Vec<PathBuf> = home_trash()?.into_iter().collect();
    for (mount, _) in common::local_mounts() {
        let trash = mount.join(".trash").join(uid().to_string());
        // A bind mount (like /run/shm for /dev/shm) shows the same trash twice.
        if trash.join(META).is_dir() && !trashes.iter().any(|t| common::same_file(t, &trash)) {
            trashes.push(trash);
        }
    }
    let mut items = Vec::new();
    for trash in &trashes {
        let Ok(days) = fs::read_dir(trash.join(META)) else { continue };
        for day in days.flatten() {
            let Ok(records) = fs::read_dir(day.path()) else { continue };
            let day = day.file_name();
            items.extend(records.flatten().filter_map(|r| read_item(trash, &Path::new(&day).join(r.file_name()))));
        }
    }
    Ok(items)
}

/// Reads the record for `<trash>/<day>/<name>` from `<trash>/.meta/<day>/<name>`,
/// with `rel` being `<day>/<name>`. None when there's no such record or it
/// doesn't look like one, so stray files are left alone.
fn read_item(trash: &Path, rel: &Path) -> Option<Item> {
    let meta = trash.join(META).join(rel);
    let record = fs::read(&meta).ok()?;
    let split = record.iter().position(|&b| b == b'\n')?;
    let when = std::str::from_utf8(&record[..split]).ok()?.parse().ok()?;
    let origin = PathBuf::from(OsStr::from_bytes(&record[split + 1..]));
    Some(Item { path: trash.join(rel), meta, origin, when })
}

/// `~/.trash`, with symlinks in the home path resolved so it compares
/// cleanly with the resolved paths being deleted. None for root, whose
/// deletes always go to the top of their own drive (`/.trash/0` and so on),
/// where they're easy to find, rather than into /root.
fn home_trash() -> common::Result<Option<PathBuf>> {
    if uid() == 0 {
        return Ok(None);
    }
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    Ok(Some(fs::canonicalize(&home).unwrap_or(home).join(".trash")))
}

fn uid() -> u32 {
    // SAFETY: getuid takes no arguments, touches no memory and can't fail.
    unsafe { libc::getuid() }
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
    fn names_items() {
        let n = |name: &str, i| item_name(OsStr::new(name), i).into_string().unwrap();
        assert_eq!(n("notes.txt", 1), "notes.txt");
        assert_eq!(n("notes.txt", 2), "notes (2).txt");
        assert_eq!(n("build", 3), "build (3)");
        assert_eq!(n(".bashrc", 2), ".bashrc (2)");
    }

    #[test]
    fn shortens_long_names() {
        let long = format!("{}.txt", "é".repeat(125)); // 254 bytes, no room for " (2)"
        let name = item_name(OsStr::new(&long), 2);
        assert!(name.len() <= NAME_MAX, "{}", name.len());
        let name = name.into_string().expect("no character cut in half");
        assert!(name.ends_with(" (2).txt"));
    }
}
