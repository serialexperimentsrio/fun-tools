//! Code shared by the tools: error reporting, the state directory,
//! the selection list, colors and file moves that work across filesystems.

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

/// Runs a tool and turns the outcome into an exit code, printing the error
/// as `tool: message`. A closed pipe (`dog big.log | head`) counts as success.
pub fn run(main: impl FnOnce() -> Result) -> ExitCode {
    let Err(err) = main() else {
        return ExitCode::SUCCESS;
    };
    if broken_pipe(&*err) {
        return ExitCode::SUCCESS;
    }
    if !err.is::<Reported>() {
        warn(err);
    }
    ExitCode::FAILURE
}

/// True when stdout's reader went away, e.g. `| head` finished.
pub fn broken_pipe(err: &(dyn Error + 'static)) -> bool {
    err.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
}

/// Prints `tool: message` to stderr without stopping.
pub fn warn(msg: impl fmt::Display) {
    eprintln!("{}: {}", tool_name(), tidy(&msg.to_string()));
}

/// Error for "something already went wrong and was printed with `warn`".
#[derive(Debug)]
pub struct Reported;

impl fmt::Display for Reported {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("one or more operations failed")
    }
}

impl Error for Reported {}

/// `Ok` when everything worked, otherwise a quiet failure.
pub fn finish(ok: bool) -> Result {
    if ok { Ok(()) } else { Err(Reported.into()) }
}

fn tool_name() -> String {
    env::args_os()
        .next()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Drops the " (os error 2)" tail that std puts on every io::Error.
fn tidy(msg: &str) -> &str {
    match msg.rfind(" (os error ") {
        Some(i) if msg.ends_with(')') => &msg[..i],
        _ => msg,
    }
}

/// Adds the path to an io error, so messages read `foo.txt: Permission denied`.
pub trait At<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> At<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|e| {
            // Left alone so `run` can still tell a closed pipe from a real failure.
            if e.kind() == io::ErrorKind::BrokenPipe {
                return e.into();
            }
            format!("{}: {}", path.display(), tidy(&e.to_string())).into()
        })
    }
}

/// `$XDG_DATA_HOME/fun-tools`, falling back to `~/.local/share/fun-tools`.
/// Created on first use.
pub fn data_dir() -> Result<PathBuf> {
    let base = match env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => {
            let home = env::var_os("HOME").ok_or("HOME is not set")?;
            PathBuf::from(home).join(".local/share")
        }
    };
    let dir = base.join("fun-tools");
    fs::create_dir_all(&dir).at(&dir)?;
    Ok(dir)
}

/// Absolute path with `.` and `..` resolved. Unlike `canonicalize`, a symlink
/// at the end stays a symlink, so selecting a link selects the link itself.
pub fn full_path(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => Ok(fs::canonicalize(parent)?.join(name)),
        _ => fs::canonicalize(abs),
    }
}

/// Reads a list of paths stored one per line. A missing file is an empty list.
pub fn read_paths(file: &Path) -> Result<Vec<PathBuf>> {
    match fs::read(file) {
        // Newline-separated, so a file name that contains a newline won't
        // survive the round trip. NUL separators would fix that if it matters.
        Ok(bytes) => Ok(bytes
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| PathBuf::from(OsStr::from_bytes(line)))
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).at(file),
    }
}

pub fn write_paths(file: &Path, paths: &[PathBuf]) -> Result {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_os_str().as_bytes());
        bytes.push(b'\n');
    }
    fs::write(file, bytes).at(file)
}

/// The list of paths picked with `sel`.
pub mod selection {
    use super::*;

    fn file() -> Result<PathBuf> {
        Ok(data_dir()?.join("selection"))
    }

    pub fn load() -> Result<Vec<PathBuf>> {
        read_paths(&file()?)
    }

    pub fn save(paths: &[PathBuf]) -> Result {
        write_paths(&file()?, paths)
    }

    /// Loads the selection and fails if it's empty, for tools run with `-s`.
    pub fn require() -> Result<Vec<PathBuf>> {
        let paths = load()?;
        if paths.is_empty() {
            return Err("nothing is selected (pick paths with `sel <path>...`)".into());
        }
        Ok(paths)
    }

    /// Drops paths that were moved or deleted.
    pub fn forget(gone: &[PathBuf]) -> Result {
        let mut paths = load()?;
        paths.retain(|p| !gone.contains(p));
        save(&paths)
    }
}

/// Renames `from` to `to`, copying and deleting when they sit on
/// different filesystems. If the copy fails halfway, the partial copy is
/// removed and the source is left as it was.
pub fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(e) = copy_path(from, to) {
                let _ = remove_path(to);
                return Err(e);
            }
            remove_path(from)
        }
        other => other,
    }
}

/// Copies a file, symlink or whole directory tree, keeping permissions and
/// modification times. Symlinks are copied as links, not followed. FIFOs,
/// sockets and device files are refused: reading a FIFO would block forever.
pub fn copy_path(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if kind.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        // Both last: adding children changes the time, and a read-only
        // directory would refuse them.
        keep_modified(to, &meta)?;
        fs::set_permissions(to, meta.permissions())
    } else if kind.is_file() {
        fs::copy(from, to)?;
        keep_modified(to, &meta)
    } else {
        Err(io::Error::new(io::ErrorKind::Unsupported, "can't copy a special file (FIFO, socket or device)"))
    }
}

fn keep_modified(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    match meta.modified() {
        Ok(time) => fs::File::open(path)?.set_modified(time),
        Err(_) => Ok(()),
    }
}

/// Whether two paths are the same file on disk (same device and inode),
/// following symlinks. Catches hard links and, on case-insensitive drives
/// like WSL's /mnt/c, the same name in a different case.
pub fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Runs `make`, which creates `target`, after moving anything already at
/// `target` out of the way. The old one is deleted only once `make` worked,
/// and put back if it failed, so a failed replace never loses data.
pub fn replace(target: &Path, make: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    if fs::symlink_metadata(target).is_err() {
        return make();
    }
    let name = target.file_name().unwrap_or(target.as_os_str()).to_string_lossy();
    // The pid keeps this from colliding with a leftover from a crashed run.
    let aside = target.with_file_name(format!(".{name}.fun-tools-old-{}", std::process::id()));
    fs::rename(target, &aside)?;
    match make() {
        Ok(()) => remove_path(&aside),
        Err(e) => {
            let _ = fs::rename(&aside, target);
            Err(e)
        }
    }
}

/// Deletes a file, symlink or directory tree. Read-only directories inside
/// the tree (common in build caches) are made writable first, since a
/// directory has to be writable for its contents to be removed.
pub fn remove_path(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return fs::remove_file(path);
    }
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            make_writable(path)?;
            fs::remove_dir_all(path)
        }
        other => other,
    }
}

fn make_writable(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::symlink_metadata(dir)?.permissions();
    perms.set_mode(perms.mode() | 0o700);
    fs::set_permissions(dir, perms)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}

/// Options for [`place_into`].
pub struct Place {
    /// Move instead of copy.
    pub cut: bool,
    /// Replace what's already at the target.
    pub force: bool,
    /// Print what would happen and stop there.
    pub dry_run: bool,
}

/// Copies or moves `src` into the directory `dir`, keeping its name. Used by
/// `put` and `cop --to`, so pasting behaves the same either way.
pub fn place_into(src: &Path, dir: &Path, how: &Place) -> Result {
    let name = src.file_name().ok_or_else(|| format!("{}: can't copy that", src.display()))?;
    fs::symlink_metadata(src).at(src)?;
    let src_real = fs::canonicalize(src).at(src)?;
    if fs::canonicalize(dir).at(dir)?.starts_with(&src_real) {
        return Err(format!("{}: can't put a directory inside itself", src.display()).into());
    }

    let target = dir.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        // Without this, -f would delete the source before copying it onto itself.
        if same_file(&target, src) {
            return Err(format!("{}: already there", target.display()).into());
        }
        if !how.force {
            return Err(format!("{}: already exists (use -f to replace it)", target.display()).into());
        }
    }

    if how.dry_run {
        let verb = if how.cut { "move" } else { "copy" };
        println!("would {verb} {} -> {}", src.display(), target.display());
        return Ok(());
    }
    replace(&target, || if how.cut { move_path(src, &target) } else { copy_path(src, &target) }).at(src)
}

/// A name made safe for a terminal: control characters are shown escaped, so
/// a file called `\e[2J` can't clear the screen, and invalid UTF-8 is
/// replaced. Use it only for terminal output; pipes should get the real bytes.
pub fn escaped(name: &OsStr) -> String {
    let text = name.to_string_lossy();
    if !text.chars().any(char::is_control) {
        return text.into_owned();
    }
    text.chars().map(|c| if c.is_control() { c.escape_default().to_string() } else { c.to_string() }).collect()
}

/// Writes a path for the reader: escaped on a terminal, raw bytes otherwise.
pub fn write_path(out: &mut impl io::Write, path: &Path, terminal: bool) -> io::Result<()> {
    if terminal {
        out.write_all(escaped(path.as_os_str()).as_bytes())
    } else {
        out.write_all(path.as_os_str().as_bytes())
    }
}

/// Whether to print colors: only on a terminal, and never with NO_COLOR set.
pub fn color() -> bool {
    io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

pub const BOLD: &str = "1";
pub const DIM: &str = "2";
pub const RED: &str = "31";
pub const GREEN: &str = "32";
pub const YELLOW: &str = "33";
pub const BLUE: &str = "1;34";
pub const CYAN: &str = "36";

/// Wraps text in an ANSI color when `on` is set.
pub fn paint(text: &str, code: &str, on: bool) -> String {
    if on { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
}

/// `/proc/meminfo` as a map of field name to bytes.
pub fn meminfo() -> Result<std::collections::HashMap<String, u64>> {
    let path = Path::new("/proc/meminfo");
    let text = fs::read_to_string(path).at(path)?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some((key.to_string(), kib * 1024))
        })
        .collect())
}

/// 1536 -> "1.5 KiB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 << 30), "5.0 GiB");
    }

    #[test]
    fn strips_os_error() {
        assert_eq!(tidy("No such file or directory (os error 2)"), "No such file or directory");
        assert_eq!(tidy("plain message"), "plain message");
    }

    #[test]
    fn copies_and_moves_trees() {
        let root = env::temp_dir().join(format!("fun-tools-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/sub")).unwrap();
        fs::write(root.join("a/sub/file"), "hi").unwrap();
        std::os::unix::fs::symlink("sub/file", root.join("a/link")).unwrap();

        copy_path(&root.join("a"), &root.join("b")).unwrap();
        assert_eq!(fs::read_to_string(root.join("b/sub/file")).unwrap(), "hi");
        assert_eq!(fs::read_link(root.join("b/link")).unwrap(), Path::new("sub/file"));

        move_path(&root.join("b"), &root.join("c")).unwrap();
        assert!(!root.join("b").exists());
        assert!(root.join("c/sub/file").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(escaped(OsStr::new("plain.txt")), "plain.txt");
        assert_eq!(escaped(OsStr::new("evil\x1b[2J\nname")), "evil\\u{1b}[2J\\nname");
        assert_eq!(escaped(OsStr::from_bytes(b"bad\xffbyte")), "bad\u{fffd}byte");
    }

    #[test]
    fn copies_keep_times_and_refuse_fifos() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!("fun-tools-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/ro")).unwrap();
        fs::write(root.join("src/f"), "x").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::open(root.join("src/f")).unwrap().set_modified(old).unwrap();

        copy_path(&root.join("src"), &root.join("dst")).unwrap();
        assert_eq!(fs::metadata(root.join("dst/f")).unwrap().modified().unwrap(), old);

        let fifo = root.join("fifo");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success());
        let err = copy_path(&fifo, &root.join("fifo2")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        // A read-only directory inside a tree must not stop removal.
        fs::write(root.join("src/ro/inner"), "x").unwrap();
        fs::set_permissions(root.join("src/ro"), fs::Permissions::from_mode(0o500)).unwrap();
        remove_path(&root.join("src")).unwrap();
        assert!(!root.join("src").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replace_restores_on_failure() {
        let dir = env::temp_dir().join(format!("fun-tools-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("t");
        fs::write(&target, "old").unwrap();

        let failed = replace(&target, || Err(io::Error::other("nope")));
        assert!(failed.is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");

        replace(&target, || fs::write(&target, "new")).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "nothing left over");

        fs::remove_dir_all(&dir).unwrap();
    }
}
