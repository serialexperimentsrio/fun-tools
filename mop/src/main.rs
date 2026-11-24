use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

/// Clear the screen.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Clear only what's visible and leave the scrollback alone
    #[arg(short, long)]
    keep_scrollback: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        // Home the cursor, clear the screen, then (unless asked not to) the scrollback.
        let seq = if cli.keep_scrollback { "\x1b[H\x1b[2J" } else { "\x1b[H\x1b[2J\x1b[3J" };
        let mut out = io::stdout().lock();
        out.write_all(seq.as_bytes())?;
        Ok(out.flush()?)
    })
}
