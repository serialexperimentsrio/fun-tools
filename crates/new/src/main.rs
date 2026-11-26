use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Parser;
use common::At;

/// Create new files or directories. Missing parent directories are created
/// too. A file that already exists is left as it is, with its access and
/// modification times set to now.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// What to create. A trailing `/` makes it a directory
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Create directories instead of files
    #[arg(short, long)]
    dir: bool,

    /// Empty a file that already exists instead of just updating its times
    #[arg(short, long)]
    force: bool,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut ok = true;
        for path in &cli.paths {
            let wants_dir = cli.dir || path.as_os_str().as_encoded_bytes().ends_with(b"/");
            let result = if wants_dir { make_dir(path) } else { make_file(path, cli.force) };
            if let Err(e) = result {
                common::warn(e);
                ok = false;
            }
        }
        common::finish(ok)
    })
}

fn make_dir(path: &Path) -> common::Result {
    if path.is_file() {
        return Err(format!("{}: a file with that name exists", path.display()).into());
    }
    if let Some(blocker) = path.ancestors().skip(1).find(|a| a.is_file()) {
        return Err(format!("{}: {} is a file, not a directory", path.display(), blocker.display()).into());
    }
    fs::create_dir_all(path).at(path)
}

fn make_file(path: &Path, force: bool) -> common::Result {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        // create_dir_all only says "File exists" when part of the way is a file.
        if let Some(blocker) = parent.ancestors().find(|a| a.is_file()) {
            return Err(format!("{}: {} is a file, not a directory", path.display(), blocker.display()).into());
        }
        fs::create_dir_all(parent).at(parent)?;
    }
    if path.is_dir() {
        return Err(format!("{}: a directory with that name exists", path.display()).into());
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        // Already there: just mark it as touched now, like `touch` would.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let now = SystemTime::now();
            File::open(path).and_then(|f| f.set_times(FileTimes::new().set_accessed(now).set_modified(now))).at(path)
        }
        Err(e) => Err(e).at(path),
    }
}
