use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use common::At;

/// Output file content.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Files to print. Reads stdin when left out, or for `-`
    paths: Vec<PathBuf>,

    /// Number every line
    #[arg(short, long)]
    number: bool,

    /// Only print the first N lines of each file
    #[arg(long, value_name = "N", conflicts_with = "tail")]
    head: Option<usize>,

    /// Only print the last N lines of each file
    #[arg(long, value_name = "N")]
    tail: Option<usize>,

    /// Print binary files to a terminal too, instead of skipping them
    #[arg(long)]
    binary: bool,

    /// Keep printing lines as they're added to the file, until Ctrl+C.
    /// Works on one file, and picks up a log that's rotated or emptied
    #[arg(long, conflicts_with = "head")]
    track: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        if cli.track && (cli.paths.len() != 1 || cli.paths[0] == Path::new("-")) {
            Cli::command().error(ErrorKind::ArgumentConflict, "--track follows exactly one file, not stdin").exit();
        }
        let mut out = io::BufWriter::new(io::stdout().lock());
        let mut line_no = 0;

        if cli.paths.is_empty() {
            print(io::stdin().lock(), &mut out, &cli, &mut line_no)?;
            return Ok(out.flush()?);
        }

        let terminal = io::stdout().is_terminal();
        // What stdout points at, if it's a regular file, to catch `dog a >> a`.
        let output = fs::metadata("/proc/self/fd/1").ok().filter(|m| m.is_file()).map(|m| (m.dev(), m.ino()));

        let mut ok = true;
        for path in &cli.paths {
            let result = if path == Path::new("-") {
                print(io::stdin().lock(), &mut out, &cli, &mut line_no).at(path)
            } else {
                open(path, output, terminal && !cli.binary).and_then(|mut f| {
                    let end = match cli.tail {
                        // Without line numbers there's no need to read a big log
                        // from the start; its end can be found from the back.
                        Some(n) if !cli.number && f.get_ref().metadata().is_ok_and(|m| m.is_file()) => {
                            tail_of(f.get_mut(), n, &mut out).at(path)?
                        }
                        _ => {
                            print(&mut f, &mut out, &cli, &mut line_no).at(path)?;
                            f.stream_position().at(path)?
                        }
                    };
                    if cli.track {
                        track(path, end, &mut out, &cli, &mut line_no).at(path)?;
                    }
                    Ok(())
                })
            };
            if let Err(e) = result {
                if common::broken_pipe(&*e) {
                    return Err(e);
                }
                out.flush()?;
                common::warn(e);
                ok = false;
            }
        }
        out.flush()?;
        common::finish(ok)
    })
}

/// Writes the last `n` lines of a regular file, reading backwards from the
/// end in chunks until it has them. Returns where the file ended.
fn tail_of(file: &mut File, n: usize, out: &mut impl Write) -> io::Result<u64> {
    const CHUNK: u64 = 64 * 1024;
    let len = file.metadata()?.len();
    if n == 0 {
        return Ok(len);
    }
    let mut pos = len;
    let mut buf = Vec::new();
    while pos > 0 {
        let step = CHUNK.min(pos);
        pos -= step;
        file.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0; step as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        // A newline at the very end closes the last line rather than starting another.
        let body = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if body.iter().filter(|&&b| b == b'\n').count() >= n {
            break;
        }
    }
    let lines: Vec<&[u8]> = buf.split_inclusive(|&b| b == b'\n').collect();
    for line in &lines[lines.len().saturating_sub(n)..] {
        out.write_all(line)?;
    }
    Ok(len)
}

/// Keeps printing what gets added to the file after `pos`, until Ctrl+C.
/// A log that's rotated (a new file at the same path) is picked up from its
/// start, and one that's emptied is read again from the top.
fn track(path: &Path, mut pos: u64, out: &mut impl Write, cli: &Cli, line_no: &mut usize) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut inode = file.metadata()?.ino();
    // Half a line written so far, held back until the rest of it arrives.
    let mut pending = Vec::new();
    loop {
        out.flush()?;
        sleep(Duration::from_millis(250));
        if let Ok(meta) = fs::metadata(path) {
            if meta.ino() != inode {
                file = File::open(path)?;
                inode = meta.ino();
                pos = 0;
                pending.clear();
            }
        }
        let len = file.metadata()?.len();
        if len < pos {
            common::warn(format!("{}: file was emptied, reading it from the top", path.display()));
            pos = 0;
            pending.clear();
        }
        if len == pos {
            continue;
        }
        file.seek(SeekFrom::Start(pos))?;
        pos += (&mut file).take(len - pos).read_to_end(&mut pending)? as u64;
        let Some(end) = pending.iter().rposition(|&b| b == b'\n') else { continue };
        for line in pending.drain(..=end).collect::<Vec<_>>().split_inclusive(|&b| b == b'\n') {
            *line_no += 1;
            if cli.number {
                write!(out, "{:>6}  ", *line_no)?;
            }
            out.write_all(line)?;
        }
    }
}

fn open(path: &Path, output: Option<(u64, u64)>, skip_binary: bool) -> common::Result<BufReader<File>> {
    let file = File::open(path).at(path)?;
    let meta = file.metadata().at(path)?;
    if meta.is_dir() {
        return Err(format!("{}: is a directory", path.display()).into());
    }
    // Reading a file while appending it to itself never ends.
    if output == Some((meta.dev(), meta.ino())) && meta.len() > 0 {
        return Err(format!("{}: input file is the output file", path.display()).into());
    }
    let mut reader = BufReader::new(file);
    // Binary data on a terminal is garbage at best and can scramble its
    // state at worst. A zero byte early on is the usual tell.
    if skip_binary && reader.fill_buf().at(path)?.contains(&0) {
        return Err(format!("{}: binary file, not shown (pipe it or use --binary)", path.display()).into());
    }
    Ok(reader)
}

/// Writes one input to `out`. `line_no` carries on across files.
fn print(mut input: impl BufRead, out: &mut impl Write, cli: &Cli, line_no: &mut usize) -> io::Result<()> {
    if !cli.number && cli.head.is_none() && cli.tail.is_none() {
        io::copy(&mut input, out)?;
        return Ok(());
    }

    let emit = |n: usize, line: &[u8], out: &mut dyn Write| -> io::Result<()> {
        if cli.number {
            write!(out, "{n:>6}  ")?;
        }
        out.write_all(line)
    };

    // Kept with their real line numbers, so `-n --tail 5` shows where they are.
    let mut tail = VecDeque::new();
    let mut seen = 0;
    let mut line = Vec::new();
    // Bytes, not strings, so files that aren't valid UTF-8 still print.
    // The limit is checked before reading so a terminal isn't asked for one line too many.
    while cli.head != Some(seen) && input.read_until(b'\n', &mut line)? > 0 {
        seen += 1;
        *line_no += 1;
        match cli.tail {
            Some(0) => {}
            Some(n) => {
                if tail.len() == n {
                    tail.pop_front();
                }
                tail.push_back((*line_no, std::mem::take(&mut line)));
            }
            None => emit(*line_no, &line, out)?,
        }
        line.clear();
    }
    for (n, line) in tail {
        emit(n, &line, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], input: &str) -> String {
        let cli = Cli::parse_from(std::iter::once("dog").chain(args.iter().copied()));
        let mut out = Vec::new();
        print(input.as_bytes(), &mut out, &cli, &mut 0).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn slices_and_numbers() {
        let text = "a\nb\nc\nd";
        assert_eq!(run(&[], text), text);
        assert_eq!(run(&["--head", "2"], text), "a\nb\n");
        assert_eq!(run(&["--tail", "2"], text), "c\nd");
        assert_eq!(run(&["--tail", "0"], text), "");
        assert_eq!(run(&["-n", "--head", "1"], text), "     1  a\n");
        assert_eq!(run(&["-n", "--tail", "1"], text), "     4  d");
    }

    #[test]
    fn tails_big_files_from_the_back() {
        let path = std::env::temp_dir().join(format!("dog-tail-{}", std::process::id()));
        // Over 64 KiB, so the lines it wants span more than one chunk.
        let text: String = (1..=20_000).map(|i| format!("line {i}\n")).collect();
        let tail = |text: &str, n| {
            fs::write(&path, text).unwrap();
            let mut out = Vec::new();
            tail_of(&mut File::open(&path).unwrap(), n, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(tail(&text, 2), "line 19999\nline 20000\n");
        assert_eq!(tail(&text, 12_000).lines().next(), Some("line 8001"));
        assert_eq!(tail("a\nb", 1), "b");
        assert_eq!(tail("a\nb\n", 5), "a\nb\n");
        assert_eq!(tail("", 3), "");
        fs::remove_file(&path).unwrap();
    }
}
