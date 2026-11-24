use std::io::{self, Write};
use std::process::ExitCode;

use chrono::format::{Item, StrftimeItems};
use chrono::{Local, Utc};
use clap::Parser;

/// Show the date or time.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Only the date
    #[arg(short, long)]
    date: bool,

    /// Only the time
    #[arg(short, long)]
    time: bool,

    /// Use UTC instead of local time
    #[arg(short, long)]
    utc: bool,

    /// Custom strftime format, e.g. "%Y-%m-%d %H:%M"
    #[arg(long, value_name = "FMT", conflicts_with_all = ["date", "time"])]
    format: Option<String>,

    /// Seconds since the Unix epoch
    #[arg(long, conflicts_with_all = ["date", "time", "format", "utc"])]
    unix: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let now = Utc::now();
        let mut out = io::stdout().lock();

        if cli.unix {
            writeln!(out, "{}", now.timestamp())?;
            return Ok(());
        }

        let fmt = match (&cli.format, cli.date, cli.time) {
            (Some(fmt), _, _) => fmt.as_str(),
            (None, true, false) => "%Y-%m-%d",
            (None, false, true) => "%H:%M:%S",
            _ => "%a %Y-%m-%d %H:%M:%S %Z",
        };
        // chrono panics on a bad format at print time, so check it first.
        let items: Vec<Item> = StrftimeItems::new(fmt).collect();
        if items.contains(&Item::Error) {
            return Err(format!("invalid format: {fmt}").into());
        }

        if cli.utc {
            writeln!(out, "{}", now.format_with_items(items.iter()))?;
        } else {
            writeln!(out, "{}", now.with_timezone(&Local).format_with_items(items.iter()))?;
        }
        Ok(())
    })
}
