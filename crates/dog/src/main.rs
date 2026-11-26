use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
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
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
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
                open(path, output, terminal && !cli.binary)
                    .and_then(|f| print(f, &mut out, &cli, &mut line_no).at(path))
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
}
