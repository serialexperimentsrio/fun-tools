use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{self, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use common::{At, paint, selection};

/// Keep a list of selected files and directories for `cop`, `mov` and `del`
/// to act on. `sel a.txt b/` adds to it. To select a file named like one of
/// the subcommands, write it as a path: `sel ./idx`.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    action: Option<Action>,

    /// Files and directories to add to the selection
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Action {
    /// Take paths out of the selection, or empty it when no paths are given.
    /// The files themselves stay put
    Mop { paths: Vec<PathBuf> },
    /// List the selection
    #[command(visible_alias = "index")]
    Idx,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut selected = selection::load()?;
        let mut ok = true;

        match cli.action {
            None => {
                for path in cli.paths {
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
            Some(Action::Mop { paths }) if paths.is_empty() => selection::save(&[])?,
            Some(Action::Mop { paths }) => {
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
            Some(Action::Idx) => {
                let color = common::color();
                let mut out = io::stdout().lock();
                let terminal = out.is_terminal();
                for path in &selected {
                    if !terminal {
                        // Exact paths only, so scripts can use them as they are.
                        common::write_path(&mut out, path, false)?;
                        writeln!(out)?;
                        continue;
                    }
                    let shown = common::escaped(path.as_os_str());
                    // Something moved or deleted it since it was selected.
                    if fs::symlink_metadata(path).is_err() {
                        writeln!(out, "{}", paint(&format!("{shown} (missing)"), common::RED, color))?;
                    } else {
                        writeln!(out, "{shown}")?;
                    }
                }
            }
        }
        common::finish(ok)
    })
}
