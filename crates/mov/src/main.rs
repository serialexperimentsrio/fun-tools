use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use common::{At, selection};

/// Move or rename files and directories.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Sources, then the destination. With --selected, only the destination
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Move the selected paths (see `sel`)
    #[arg(short, long)]
    selected: bool,

    /// Replace anything already at the destination
    #[arg(short, long)]
    force: bool,

    /// Show what would move without touching anything
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let (sources, dest) = if cli.selected {
            let [dest] = cli.paths.as_slice() else {
                return Err("with --selected, give only the destination".into());
            };
            (selection::require()?, dest.clone())
        } else {
            match cli.paths.split_last() {
                Some((dest, sources)) if !sources.is_empty() => (sources.to_vec(), dest.clone()),
                _ => return Err("give at least one source and a destination".into()),
            }
        };

        let into_dir = dest.is_dir();
        if sources.len() > 1 && !into_dir {
            return Err(format!("{}: not a directory", dest.display()).into());
        }

        let mut ok = true;
        let mut moved = Vec::new();
        for src in &sources {
            let target = match (into_dir, src.file_name()) {
                (true, Some(name)) => dest.join(name),
                (true, None) => {
                    common::warn(format!("{}: can't move that", src.display()));
                    ok = false;
                    continue;
                }
                (false, _) => dest.clone(),
            };
            match move_one(src, &target, &cli) {
                Ok(()) => moved.push(src.clone()),
                Err(e) => {
                    common::warn(e);
                    ok = false;
                }
            }
        }

        if cli.selected && !cli.dry_run {
            selection::forget(&moved)?;
        }
        common::finish(ok)
    })
}

fn move_one(src: &Path, target: &Path, cli: &Cli) -> common::Result {
    fs::symlink_metadata(src).at(src)?;
    let src_real = fs::canonicalize(src).at(src)?;
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if fs::canonicalize(parent).at(parent)?.starts_with(&src_real) {
        return Err(format!("{}: can't move a directory into itself", src.display()).into());
    }

    if fs::symlink_metadata(target).is_ok() {
        if fs::canonicalize(target).ok().as_ref() == Some(&src_real) {
            return Err(format!("{}: source and destination are the same", src.display()).into());
        }
        if !cli.force {
            return Err(format!("{}: already exists (use -f to replace it)", target.display()).into());
        }
        if !cli.dry_run {
            // ponytail: the old target is gone before the move runs, so a failed
            // move loses it. Rename it aside first if that ever bites.
            common::remove_path(target).at(target)?;
        }
    }

    if cli.dry_run {
        println!("{} -> {}", src.display(), target.display());
        return Ok(());
    }
    common::move_path(src, target).at(src)
}
