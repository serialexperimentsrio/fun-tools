use std::fs;
use std::io::{self, Write};
use std::path::{self, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use common::{At, paint, selection};

/// Keep a list of selected files and directories for `cop`, `mov` and `del` to act on.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Add paths to the selection
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Remove paths from the selection
    Rm {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// List the selection
    Ls,
    /// Empty the selection
    Clear,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut selected = selection::load()?;
        let mut ok = true;

        match cli.action {
            Action::Add { paths } => {
                for path in paths {
                    match fs::symlink_metadata(&path).and_then(|_| common::full_path(&path)).at(&path) {
                        Ok(abs) if !selected.contains(&abs) => selected.push(abs),
                        Ok(_) => {}
                        Err(e) => {
                            common::warn(e);
                            ok = false;
                        }
                    }
                }
                selection::save(&selected)?;
            }
            Action::Rm { paths } => {
                for path in paths {
                    // The path may be gone already, so fall back to a plain absolute path.
                    let abs = common::full_path(&path).or_else(|_| path::absolute(&path)).at(&path)?;
                    let before = selected.len();
                    selected.retain(|p| *p != abs);
                    if selected.len() == before {
                        common::warn(format!("{}: not selected", path.display()));
                        ok = false;
                    }
                }
                selection::save(&selected)?;
            }
            Action::Ls => {
                let color = common::color();
                let mut out = io::stdout().lock();
                for path in &selected {
                    let shown = path.display().to_string();
                    // Something moved or deleted it since it was selected.
                    if fs::symlink_metadata(path).is_err() {
                        writeln!(out, "{}", paint(&format!("{shown} (missing)"), common::RED, color))?;
                    } else {
                        writeln!(out, "{shown}")?;
                    }
                }
            }
            Action::Clear => selection::save(&[])?,
        }
        common::finish(ok)
    })
}
