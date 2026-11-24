use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::Duration;

use arboard::{Clipboard, SetExtLinux};
use clap::Parser;
use common::{At, Place, selection};

/// Copy files, directories or text to the system clipboard, then paste with
/// `put`. Text can come from a pipe or from a file, whole or a range of lines.
/// With --to, files are copied straight into a directory instead.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Paths to copy. With none, copies text from stdin
    #[arg(conflicts_with = "selected")]
    paths: Vec<PathBuf>,

    /// Copy the selected paths (see `sel`)
    #[arg(short, long)]
    selected: bool,

    /// Cut instead of copy: the next `put` moves the paths
    #[arg(short = 'x', long)]
    cut: bool,

    /// Copy straight into this directory, leaving the clipboard alone
    #[arg(short, long, value_name = "DIR", conflicts_with = "cut")]
    to: Option<PathBuf>,

    /// With --to, replace anything already there
    #[arg(short, long, requires = "to")]
    force: bool,

    /// With --to, show what would be copied without touching anything
    #[arg(long, requires = "to")]
    dry_run: bool,

    /// Copy the file's text instead of the file itself
    #[arg(short, long, conflicts_with_all = ["selected", "cut", "to"])]
    content: bool,

    /// Copy only these lines of the file's text: 5, 5-10 or 5- for the rest
    #[arg(long, value_name = "RANGE", value_parser = parse_range, conflicts_with_all = ["selected", "cut", "to"])]
    lines: Option<Lines>,
}

/// An inclusive, 1-based line range. `last` is None for "to the end".
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lines {
    first: usize,
    last: Option<usize>,
}

fn parse_range(s: &str) -> Result<Lines, String> {
    let num = |t: &str| t.trim().parse::<usize>().ok().filter(|&n| n > 0).ok_or(format!("`{t}` isn't a line number"));
    match s.split_once(['-', ':']) {
        None => num(s).map(|n| Lines { first: n, last: Some(n) }),
        Some((first, "")) => Ok(Lines { first: num(first)?, last: None }),
        Some((first, last)) => {
            let (first, last) = (num(first)?, num(last)?);
            if last < first {
                return Err(format!("{s} ends before it starts"));
            }
            Ok(Lines { first, last: Some(last) })
        }
    }
}

/// The text of a file, or just some of its lines.
fn text_of(path: &Path, lines: Option<Lines>) -> common::Result<String> {
    let text =
        String::from_utf8(fs::read(path).at(path)?).map_err(|_| format!("{}: not a text file", path.display()))?;
    let Some(range) = lines else {
        return Ok(text);
    };
    let all: Vec<&str> = text.split_inclusive('\n').collect();
    if range.first > all.len() {
        return Err(format!("{}: only has {} lines", path.display(), all.len()).into());
    }
    let last = range.last.unwrap_or(all.len()).min(all.len());
    Ok(all[range.first - 1..last].concat())
}

/// Set in the environment of the background copy of `cop` that keeps the
/// clipboard contents alive.
const SERVE: &str = "FUN_TOOLS_CLIPBOARD_SERVE";

enum Payload {
    Files(Vec<PathBuf>),
    Text(String),
}

fn main() -> ExitCode {
    if env::var_os(SERVE).is_some() {
        return common::run(serve);
    }
    common::run(|| {
        let cli = Cli::parse();
        if cli.to.is_some() && cli.paths.is_empty() && !cli.selected {
            return Err("--to needs paths to copy".into());
        }
        let payload = if cli.selected {
            Payload::Files(selection::require()?)
        } else if cli.content || cli.lines.is_some() {
            let [path] = cli.paths.as_slice() else {
                return Err("--content and --lines take exactly one file".into());
            };
            Payload::Text(text_of(path, cli.lines)?)
        } else if !cli.paths.is_empty() {
            let mut full = Vec::new();
            for path in &cli.paths {
                fs::symlink_metadata(path).at(path)?;
                full.push(common::full_path(path).at(path)?);
            }
            Payload::Files(full)
        } else if !io::stdin().is_terminal() {
            if cli.cut {
                return Err("--cut only works with paths".into());
            }
            let mut text = String::new();
            io::stdin().read_to_string(&mut text).map_err(|e| match e.kind() {
                io::ErrorKind::InvalidData => "piped input isn't UTF-8 text; copy the file itself instead".to_string(),
                _ => e.to_string(),
            })?;
            Payload::Text(text)
        } else {
            return Err("nothing to copy: give some paths or pipe in text".into());
        };

        if let (Some(dir), Payload::Files(paths)) = (&cli.to, &payload) {
            return copy_to(dir, paths, &cli);
        }

        // Fail here, with a clear message, rather than in the background process.
        let mut clipboard = Clipboard::new().map_err(|e| format!("can't reach the clipboard: {e}"))?;

        let cut_file = common::data_dir()?.join("cut");
        match &payload {
            Payload::Files(paths) if cli.cut => common::write_paths(&cut_file, paths)?,
            _ => match fs::remove_file(&cut_file) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e).at(&cut_file),
                _ => {}
            },
        }

        hand_off(&payload, &mut clipboard)
    })
}

fn copy_to(dir: &Path, paths: &[PathBuf], cli: &Cli) -> common::Result {
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", dir.display()).into());
    }
    let how = Place { cut: false, force: cli.force, dry_run: cli.dry_run };
    let mut ok = true;
    for src in paths {
        if let Err(e) = common::place_into(src, dir, &how) {
            common::warn(e);
            ok = false;
        }
    }
    common::finish(ok)
}

/// X11 and Wayland don't store clipboard contents anywhere: the program that
/// copied them has to stay around and hand them out. So we start a detached
/// copy of ourselves to do that, the same trick `xclip` and `wl-copy` use.
/// It exits on its own once something else takes over the clipboard.
fn hand_off(payload: &Payload, clipboard: &mut Clipboard) -> common::Result {
    let mut command = Command::new(env::current_exe()?);
    command.env(SERVE, "1").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    // Own session, so closing the terminal doesn't take the clipboard with it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn()?;

    let mut stdin = child.stdin.take().ok_or("couldn't talk to the clipboard process")?;
    match payload {
        Payload::Files(paths) => {
            stdin.write_all(b"f")?;
            for path in paths {
                stdin.write_all(path.as_os_str().as_bytes())?;
                stdin.write_all(b"\n")?;
            }
        }
        Payload::Text(text) => {
            stdin.write_all(b"t")?;
            stdin.write_all(text.as_bytes())?;
        }
    }
    drop(stdin);

    // Wait until the new contents are actually there, so a `put` right after
    // this doesn't paste whatever was on the clipboard before.
    for _ in 0..80 {
        if child.try_wait()?.is_some() {
            return Err("the clipboard process quit early".into());
        }
        let ready = match payload {
            Payload::Files(paths) => clipboard.get().file_list().is_ok_and(|got| got == *paths),
            Payload::Text(text) => clipboard.get_text().is_ok_and(|got| got == *text),
        };
        if ready {
            return Ok(());
        }
        sleep(Duration::from_millis(25));
    }
    Ok(())
}

/// Runs in the background process: takes the payload from stdin, puts it on
/// the clipboard and blocks until it gets replaced.
fn serve() -> common::Result {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let mut clipboard = Clipboard::new()?;
    match input.split_first() {
        Some((b'f', list)) => {
            let paths: Vec<PathBuf> = list
                .split(|&b| b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| PathBuf::from(OsStr::from_bytes(line)))
                .collect();
            clipboard.set().wait().file_list(&paths)?;
        }
        Some((b't', text)) => clipboard.set().wait().text(String::from_utf8_lossy(text))?,
        _ => return Err("bad payload".into()),
    }
    Ok(())
}

#[test]
fn ranges() {
    assert_eq!(parse_range("5"), Ok(Lines { first: 5, last: Some(5) }));
    assert_eq!(parse_range("5-10"), Ok(Lines { first: 5, last: Some(10) }));
    assert_eq!(parse_range("5:10"), Ok(Lines { first: 5, last: Some(10) }));
    assert_eq!(parse_range("5-"), Ok(Lines { first: 5, last: None }));
    assert!(parse_range("0").is_err());
    assert!(parse_range("10-5").is_err());
    assert!(parse_range("x").is_err());
}
