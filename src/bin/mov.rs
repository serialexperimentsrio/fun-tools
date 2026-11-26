use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
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
        // `mov a.txt notes/` means "into notes", so a missing notes/ is an
        // error rather than a rename to a file called notes.
        let wants_dir = dest.as_os_str().as_bytes().ends_with(b"/");
        if !into_dir && (sources.len() > 1 || wants_dir) {
            let why = if dest.exists() { "not a directory" } else { "no such directory" };
            return Err(format!("{}: {why}", dest.display()).into());
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
    let src_meta = fs::symlink_metadata(src).at(src)?;
    // full_path rather than canonicalize: a symlink is moved as itself, so
    // moving a link into the directory it points at is fine.
    let src_full = common::full_path(src).at(src)?;
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if fs::canonicalize(parent).at(parent)?.starts_with(&src_full) {
        return Err(format!("{}: can't move a directory into itself", src.display()).into());
    }

    if let Ok(target_meta) = fs::symlink_metadata(target) {
        if target_meta.dev() == src_meta.dev() && target_meta.ino() == src_meta.ino() {
            // On a case-insensitive drive (WSL's /mnt/c) `File` *is* `file`,
            // so this is a plain rename. Otherwise it's a second hard link to
            // the same file, and rename() would silently do nothing.
            if !same_name_ignoring_case(src, target) {
                return Err(format!("{}: is the same file as {}", target.display(), src.display()).into());
            }
            if cli.dry_run {
                println!("{} -> {}", src.display(), target.display());
                return Ok(());
            }
            return fs::rename(src, target).at(src);
        }
        if !cli.force {
            return Err(format!("{}: already exists (use -f to replace it)", target.display()).into());
        }
    }

    if cli.dry_run {
        println!("{} -> {}", src.display(), target.display());
        return Ok(());
    }
    common::replace(target, || common::move_path(src, target)).at(src)
}

fn same_name_ignoring_case(a: &Path, b: &Path) -> bool {
    let lower = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    lower(a).is_some() && lower(a) == lower(b)
}
