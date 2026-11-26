use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

/// Print text. To pass a file or stdin through, use `dog`.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Text to print, joined by spaces
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    text: Vec<String>,

    /// Don't print the trailing newline
    #[arg(long)]
    no_newline: bool,

    /// Interpret backslash escapes: \n \t \r \\ \0 \e
    #[arg(short, long)]
    escapes: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut out = io::stdout().lock();
        let mut text = cli.text.join(" ");
        if cli.escapes {
            text = unescape(&text);
        }
        if !cli.no_newline {
            text.push('\n');
        }
        out.write_all(text.as_bytes())?;
        Ok(())
    })
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            // Unknown escapes stay as written, like `\q`.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[test]
fn escapes() {
    assert_eq!(unescape(r"a\tb\nc"), "a\tb\nc");
    assert_eq!(unescape(r"\\n"), "\\n");
    assert_eq!(unescape(r"\q and \"), "\\q and \\");
}
