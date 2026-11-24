use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::Duration;

use arboard::{Clipboard, SetExtLinux};
use clap::Parser;
use common::{At, selection};

/// Copy files, directories or piped text to the system clipboard.
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
        let payload = if cli.selected {
            Payload::Files(selection::require()?)
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
            io::stdin().read_to_string(&mut text)?;
            Payload::Text(text)
        } else {
            return Err("nothing to copy: give some paths or pipe in text".into());
        };

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
