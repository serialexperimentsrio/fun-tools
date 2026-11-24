use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use arboard::{Clipboard, Error as ClipError};
use clap::Parser;
use common::{At, Place};

/// Paste from the system clipboard. Copied files and directories land in
/// DEST; copied text goes to stdout, or into DEST if one is given.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Directory to paste files into (defaults to the current one), or the
    /// file to write text into
    dest: Option<PathBuf>,

    /// Replace anything already there
    #[arg(short, long)]
    force: bool,

    /// Show what would be pasted without touching anything
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut clipboard = Clipboard::new().map_err(|e| format!("can't reach the clipboard: {e}"))?;

        if let Ok(paths) = clipboard.get().file_list() {
            if !paths.is_empty() {
                return paste_files(&cli, &paths, &mut clipboard);
            }
        }
        match clipboard.get_text() {
            Ok(text) => paste_text(&cli, &text),
            Err(ClipError::ContentNotAvailable) => Err("the clipboard is empty".into()),
            Err(e) => Err(e.into()),
        }
    })
}

fn paste_text(cli: &Cli, text: &str) -> common::Result {
    let Some(dest) = &cli.dest else {
        io::stdout().lock().write_all(text.as_bytes())?;
        return Ok(());
    };
    if cli.dry_run {
        println!("would write {} bytes to {}", text.len(), dest.display());
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if cli.force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = match options.open(dest) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(format!("{}: already exists (use -f to replace it)", dest.display()).into());
        }
        other => other.at(dest)?,
    };
    file.write_all(text.as_bytes()).at(dest)
}

fn paste_files(cli: &Cli, paths: &[PathBuf], clipboard: &mut Clipboard) -> common::Result {
    let dest = cli.dest.clone().unwrap_or_else(|| PathBuf::from("."));
    if !dest.is_dir() {
        return Err(format!("{}: not a directory", dest.display()).into());
    }

    // `cop -x` leaves the list it cut behind; if it still matches what's on
    // the clipboard, this paste is a move.
    let cut_file = common::data_dir()?.join("cut");
    let how = Place { cut: common::read_paths(&cut_file)? == paths, force: cli.force, dry_run: cli.dry_run };

    let mut ok = true;
    for src in paths {
        if let Err(e) = common::place_into(src, &dest, &how) {
            common::warn(e);
            ok = false;
        }
    }

    // A cut can only be pasted once.
    if how.cut && ok && !cli.dry_run {
        fs::remove_file(&cut_file).at(&cut_file)?;
        clipboard.clear()?;
    }
    common::finish(ok)
}
