use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use arboard::{Clipboard, Error as ClipError};
use clap::Parser;
use common::{At, Place};

/// Paste from the system clipboard. Copied files and directories land in
/// DEST; copied text goes to stdout, into a new file DEST, or with --at into
/// an existing file at a given line.
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

    /// Insert the text into the file DEST, pushing what's there down:
    /// a line number, `start` or `end`
    #[arg(long, value_name = "LINE", value_parser = parse_pos, requires = "dest", conflicts_with = "force")]
    at: Option<Pos>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pos {
    Start,
    End,
    /// 1-based; the pasted text starts on this line.
    Line(usize),
}

fn parse_pos(s: &str) -> Result<Pos, String> {
    match s {
        "start" => Ok(Pos::Start),
        "end" => Ok(Pos::End),
        _ => s
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .map(Pos::Line)
            .ok_or(format!("expected a line number, `start` or `end`, not `{s}`")),
    }
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut clipboard = Clipboard::new().map_err(|e| format!("can't reach the clipboard: {e}"))?;

        if let Ok(paths) = clipboard.get().file_list() {
            if !paths.is_empty() {
                if cli.at.is_some() {
                    return Err("--at only works when the clipboard holds text".into());
                }
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
    if let Some(pos) = cli.at {
        return insert(dest, text, pos, cli.dry_run);
    }
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

fn insert(dest: &Path, text: &str, pos: Pos, dry_run: bool) -> common::Result {
    // Write through a symlink to the real file rather than replacing the link.
    let real = fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf());
    let old = match fs::read(&real) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| format!("{}: not a text file", dest.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).at(dest),
    };
    let (new, line) = splice(&old, text, pos).map_err(|e| format!("{}: {e}", dest.display()))?;
    if dry_run {
        println!("would insert {} lines into {} at line {line}", text.lines().count(), dest.display());
        return Ok(());
    }

    // Write a temporary file next to it and rename it over the original, so
    // a crash halfway through can't leave the file cut short.
    let name = real.file_name().ok_or_else(|| format!("{}: not a file", dest.display()))?;
    let tmp = real.with_file_name(format!(".{}.put-tmp", name.to_string_lossy()));
    fs::write(&tmp, new).at(&tmp)?;
    if let Ok(meta) = fs::metadata(&real) {
        fs::set_permissions(&tmp, meta.permissions()).at(&tmp)?;
    }
    fs::rename(&tmp, &real).at(&real)
}

/// Puts `text` into `old` at `pos`, keeping it on lines of its own. Returns
/// the new contents and the line the text starts on.
fn splice(old: &str, text: &str, pos: Pos) -> Result<(String, usize), String> {
    let lines: Vec<&str> = old.split_inclusive('\n').collect();
    let index = match pos {
        Pos::Start => 0,
        Pos::End => lines.len(),
        Pos::Line(n) if n <= lines.len() + 1 => n - 1,
        Pos::Line(n) => return Err(format!("only has {} lines, can't insert at line {n}", lines.len())),
    };
    let mut out = lines[..index].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(text);
    if index < lines.len() && !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&lines[index..].concat());
    Ok((out, index + 1))
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

#[test]
fn splicing() {
    let file = "one\ntwo\nthree\n";
    let at = |text, pos| splice(file, text, pos).unwrap().0;
    assert_eq!(at("new\n", Pos::Start), "new\none\ntwo\nthree\n");
    assert_eq!(at("new\n", Pos::End), "one\ntwo\nthree\nnew\n");
    assert_eq!(at("new\n", Pos::Line(2)), "one\nnew\ntwo\nthree\n");
    assert_eq!(at("new\n", Pos::Line(4)), "one\ntwo\nthree\nnew\n");
    // Text without a final newline still gets its own line.
    assert_eq!(at("new", Pos::Line(2)), "one\nnew\ntwo\nthree\n");
    assert_eq!(splice("no newline", "new", Pos::End).unwrap().0, "no newline\nnew");
    assert_eq!(splice("", "new\n", Pos::Line(1)).unwrap().0, "new\n");
    assert!(splice(file, "x", Pos::Line(9)).is_err());
}
