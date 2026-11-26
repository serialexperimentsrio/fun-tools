use std::io::{self, Write};
use std::process::ExitCode;

use chrono::format::{Item, StrftimeItems};
use chrono::{Local, Utc};
use clap::{Parser, ValueEnum};

/// Show the date or time.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Only show these parts, comma-separated
    #[arg(short, long, value_enum, value_delimiter = ',', value_name = "LIST")]
    only: Vec<Part>,

    /// Use UTC instead of local time
    #[arg(long)]
    utc: bool,

    /// Custom strftime format, e.g. "%Y-%m-%d %H:%M"
    #[arg(long, value_name = "FMT", conflicts_with = "only")]
    format: Option<String>,

    /// Seconds since the Unix epoch
    #[arg(long, conflicts_with_all = ["only", "format", "utc"])]
    unix: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Part {
    Date,
    Time,
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

        let fmt = match (&cli.format, cli.only.contains(&Part::Date), cli.only.contains(&Part::Time)) {
            (Some(fmt), _, _) => fmt.as_str(),
            (None, true, false) => "%Y-%m-%d",
            (None, false, true) => "%H:%M:%S",
            (None, true, true) => "%Y-%m-%d %H:%M:%S",
            (None, false, false) => "%a %Y-%m-%d %H:%M:%S %Z",
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
