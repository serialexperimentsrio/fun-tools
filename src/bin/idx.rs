use std::cmp::Reverse;
use std::collections::HashMap;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::{self, Metadata};
use std::io::{self, BufWriter, IsTerminal, Write};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, TimeZone};
use clap::{Parser, ValueEnum};
use common::{At, paint};
use serde_json::{Value, json};

/// List what's in a directory, directories first, or with --meta show
/// everything about one file or directory.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Directory to list, or with --meta the file or directory to describe
    /// (defaults to the current one)
    path: Option<PathBuf>,

    /// Show everything about PATH itself: its type, size (a directory's
    /// total), permissions, owner, all its times, inode and links
    #[arg(short, long, conflicts_with_all = ["long", "recursive", "depth", "sort"])]
    meta: bool,

    /// Include hidden entries
    #[arg(short, long)]
    all: bool,

    /// Show permissions, owner, size and modification time
    #[arg(short, long)]
    long: bool,

    /// Walk into subdirectories and draw a tree
    #[arg(short, long, visible_alias = "tree")]
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

    /// Also walk into other drives mounted inside the tree (like /mnt/c from /)
    #[arg(long)]
    cross_drives: bool,
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
    terminal: bool,
    json: Vec<Value>,
    /// The drive the listing starts on; a tree doesn't leave it unless --cross-drives.
    root_dev: u64,
    /// User names already looked up, by user id.
    owners: HashMap<u32, String>,
    ok: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
        let meta = fs::symlink_metadata(&root).at(&root)?;
        if cli.meta {
            return describe(&root, &meta, &cli);
        }

        let mut lister = Lister {
            cli: &cli,
            out: BufWriter::new(io::stdout().lock()),
            color: common::color(),
            terminal: io::stdout().is_terminal(),
            json: Vec::new(),
            root_dev: meta.dev(),
            owners: HashMap::new(),
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
            // A drive mounted inside the tree is listed but not walked into.
            let same_drive = self.cli.cross_drives || entry.meta.dev() == self.root_dev;
            if tree && entry.meta.is_dir() && same_drive {
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
            let modified = entry.modified().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let owner = self.owner(meta.uid());
            self.json.push(json!({
                "path": entry.path.to_string_lossy(),
                "name": entry.name.to_string_lossy(),
                "type": common::kind(meta.file_type()),
                "size": meta.len(),
                "modified": modified,
                "mode": format!("{:o}", mode & 0o7777),
                "owner": owner,
            }));
            return Ok(());
        }

        let mut name = self.name_bytes(&entry.name);
        let code = if meta.is_dir() {
            name.push(b'/');
            common::BLUE
        } else if meta.is_symlink() {
            common::CYAN
        } else if mode & 0o111 != 0 {
            common::GREEN
        } else {
            ""
        };
        let mut line = if code.is_empty() || !self.color {
            name
        } else {
            paint(&String::from_utf8_lossy(&name), code, true).into_bytes()
        };
        if meta.is_symlink() {
            if let Ok(target) = fs::read_link(&entry.path) {
                line.extend_from_slice(b" -> ");
                line.extend_from_slice(&self.name_bytes(target.as_os_str()));
            }
        }

        if self.cli.long {
            let size = if meta.is_dir() { "-".to_string() } else { common::human_size(meta.len()) };
            let time = DateTime::<Local>::from(entry.modified()).format("%Y-%m-%d %H:%M");
            let owner = self.owner(meta.uid());
            let info = format!("{} {owner:<8} {size:>9}  {time}", permissions(meta));
            write!(self.out, "{}  ", paint(&info, common::DIM, self.color))?;
        }
        self.out.write_all(prefix.as_bytes())?;
        self.out.write_all(&line)?;
        writeln!(self.out)?;
        Ok(())
    }

    /// Escaped for a terminal, exact bytes for a pipe, so a script reading
    /// `idx` output gets names it can actually open.
    fn name_bytes(&self, name: &OsStr) -> Vec<u8> {
        if self.terminal { common::escaped(name).into_bytes() } else { name.as_bytes().to_vec() }
    }

    fn owner(&mut self, uid: u32) -> String {
        self.owners.entry(uid).or_insert_with(|| user_name(uid).unwrap_or_else(|| uid.to_string())).clone()
    }
}

/// `idx --meta`: everything about one file or directory.
fn describe(path: &Path, meta: &Metadata, cli: &Cli) -> common::Result {
    let secs = |time: io::Result<SystemTime>| {
        time.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)
    };
    let times = [
        ("modified", secs(meta.modified())),
        ("accessed", secs(meta.accessed())),
        ("changed", Some(meta.ctime())),
        // Not every filesystem records when a file was created.
        ("created", secs(meta.created())),
    ];
    let (size, files, complete) = if meta.is_dir() {
        let (bytes, files, complete) = total(path, meta.dev(), cli.cross_drives);
        (bytes, Some(files), complete)
    } else {
        (meta.len(), None, true)
    };
    let (owner, group) = (user_name(meta.uid()), group_name(meta.gid()));
    let target = if meta.is_symlink() { fs::read_link(path).ok() } else { None };
    // metadata() follows the link, so it fails when the target is gone.
    let target_exists = target.as_ref().map(|_| fs::metadata(path).is_ok());
    let mode = meta.permissions().mode() & 0o7777;
    let mut out = io::stdout().lock();

    if cli.json {
        let mut obj = json!({
            "path": path.to_string_lossy(),
            "type": common::kind(meta.file_type()),
            "size": size,
            "files": files,
            "complete": complete,
            "mode": format!("{mode:o}"),
            "owner": owner,
            "group": group,
            "uid": meta.uid(),
            "gid": meta.gid(),
            "inode": meta.ino(),
            "links": meta.nlink(),
            "target": target.as_ref().map(|t| t.to_string_lossy()),
            "target_exists": target_exists,
        });
        for (name, time) in times {
            obj[name] = json!(time);
        }
        serde_json::to_writer_pretty(&mut out, &obj)?;
        writeln!(out)?;
        return Ok(());
    }

    let mut rows = vec![("type", common::kind(meta.file_type()).to_string())];
    // The exact count only adds something once the short form rounds.
    let size_text = if size < 1024 {
        format!("{size} {}", if size == 1 { "byte" } else { "bytes" })
    } else {
        format!("{} ({size} bytes)", common::human_size(size))
    };
    rows.push(match files {
        Some(files) => {
            let partial = if complete { "" } else { ", some folders couldn't be read" };
            ("size", format!("{size_text} in {files} {}{partial}", if files == 1 { "file" } else { "files" }))
        }
        None => ("size", size_text),
    });
    rows.push(("mode", format!("{} ({mode:o})", permissions(meta))));
    let owner = owner.unwrap_or_else(|| meta.uid().to_string());
    let group = group.unwrap_or_else(|| meta.gid().to_string());
    rows.push(("owner", format!("{owner}:{group}")));
    for (name, time) in times {
        let shown = time.and_then(|s| Local.timestamp_opt(s, 0).single()).map(|t| t.format("%Y-%m-%d %H:%M:%S"));
        rows.push((name, shown.map_or("unknown".to_string(), |t| t.to_string())));
    }
    let links = meta.nlink();
    rows.push(("inode", format!("{}, {links} {}", meta.ino(), if links == 1 { "link" } else { "links" })));
    if let Some(target) = &target {
        let missing = if target_exists == Some(true) { "" } else { " (missing)" };
        rows.push(("target", format!("{}{missing}", common::escaped(target.as_os_str()))));
    }

    let color = common::color();
    writeln!(out, "{}", paint(&common::escaped(path.as_os_str()), common::BOLD, color))?;
    for (label, value) in rows {
        writeln!(out, "  {}  {value}", paint(&format!("{label:<8}"), common::BLUE, color))?;
    }
    Ok(())
}

/// Bytes and files below `dir`, without following links and staying on its
/// drive unless `cross` is set. The flag is false when some folder couldn't
/// be read, which makes the total a lower bound.
// ponytail: a file with several hard links inside the tree counts once per
// name; track (device, inode) pairs if exact totals matter.
fn total(dir: &Path, dev: u64, cross: bool) -> (u64, u64, bool) {
    let (mut bytes, mut files, mut complete) = (0, 0, true);
    let mut todo = vec![dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(list) = fs::read_dir(&dir) else {
            complete = false;
            continue;
        };
        for entry in list.flatten() {
            // DirEntry::metadata doesn't follow symlinks.
            let Ok(meta) = entry.metadata() else {
                complete = false;
                continue;
            };
            if meta.is_dir() {
                if cross || meta.dev() == dev {
                    todo.push(entry.path());
                }
            } else {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files, complete)
}

/// The name for a user id, through the system's user database (so LDAP and
/// the like work too). None when there's no such user.
fn user_name(uid: u32) -> Option<String> {
    let mut entry = MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found = std::ptr::null_mut();
    // SAFETY: getpwuid_r fills `entry`, keeping its strings in `buf`, and
    // points `found` at it when there's a match.
    let rc = unsafe { libc::getpwuid_r(uid, entry.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return None;
    }
    // SAFETY: pw_name is a NUL-terminated string inside `buf`, which is still alive.
    Some(unsafe { CStr::from_ptr((*found).pw_name) }.to_string_lossy().into_owned())
}

/// The name for a group id, the same way as [`user_name`].
fn group_name(gid: u32) -> Option<String> {
    let mut entry = MaybeUninit::<libc::group>::uninit();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found = std::ptr::null_mut();
    // SAFETY: as in user_name, with getgrgid_r and a group entry.
    let rc = unsafe { libc::getgrgid_r(gid, entry.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return None;
    }
    // SAFETY: gr_name is a NUL-terminated string inside `buf`, which is still alive.
    Some(unsafe { CStr::from_ptr((*found).gr_name) }.to_string_lossy().into_owned())
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
