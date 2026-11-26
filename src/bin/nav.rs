use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

/// Change directory. Give a path, part of a directory name to look for below
/// the current one, or `-` for the previous directory.
///
/// A program can't change its shell's directory, so nav prints where to go and
/// a small shell function does the `cd`. Set it up once with
/// `eval "$(nav --init bash)"` in ~/.bashrc.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Where to go. Defaults to your home directory
    target: Option<PathBuf>,

    /// Go up N levels (from TARGET, if one is given)
    #[arg(short, long, value_name = "N")]
    up: Option<usize>,

    /// How many levels down to look when TARGET isn't a path
    #[arg(long, value_name = "N", default_value_t = 4)]
    depth: usize,

    /// Print the shell function that makes nav work, for your shell's rc file
    #[arg(long, value_name = "SHELL", exclusive = true)]
    init: Option<Shell>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
}

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        let mut out = io::stdout().lock();

        if let Some(shell) = cli.init {
            out.write_all(hook(shell).as_bytes())?;
            return Ok(());
        }

        let here = current_dir()?;
        let mut dir = match &cli.target {
            // `nav -u 2` climbs from here; a bare `nav` goes home.
            None if cli.up.is_some() => here,
            None => PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?),
            Some(t) if t == Path::new("-") => PathBuf::from(env::var_os("OLDPWD").ok_or("no previous directory yet")?),
            Some(t) => resolve(&here, t, cli.depth)?,
        };
        for _ in 0..cli.up.unwrap_or(0) {
            dir.pop();
        }

        out.write_all(dir.as_os_str().as_bytes())?;
        out.write_all(b"\n")?;
        Ok(())
    })
}

/// The directory the shell thinks it's in. $PWD keeps symlinked paths the way
/// the user typed them, like `cd` does; the real path is only a fallback.
fn current_dir() -> io::Result<PathBuf> {
    match env::var_os("PWD").map(PathBuf::from) {
        Some(pwd) if pwd.is_absolute() && pwd.is_dir() => Ok(pwd),
        _ => env::current_dir(),
    }
}

/// Turns TARGET into a directory: an existing path if there is one, otherwise
/// the closest directory below `here` whose name contains it.
fn resolve(here: &Path, target: &Path, depth: usize) -> common::Result<PathBuf> {
    let path = tidy(&here.join(target));
    if path.is_dir() {
        return Ok(path);
    }
    if path.exists() {
        return Err(format!("{}: not a directory", target.display()).into());
    }
    if target.components().count() > 1 {
        return Err(format!("{}: no such directory", target.display()).into());
    }
    let name = target.to_string_lossy().to_lowercase();
    search(here, &name, depth).ok_or_else(|| format!("no directory matching `{}` below here", target.display()).into())
}

/// Removes `.` and resolves `..` by dropping the previous component, the same
/// way `cd` treats paths.
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Looks one level at a time, so the shallowest match wins. Within a level an
/// exact name beats a partial one, then paths sort alphabetically. Hidden
/// directories are skipped unless the search itself starts with a dot.
fn search(root: &Path, needle: &str, max_depth: usize) -> Option<PathBuf> {
    let skip_hidden = !needle.starts_with('.');
    let mut level = vec![root.to_path_buf()];
    for _ in 0..max_depth {
        let mut hits = Vec::new();
        let mut next = Vec::new();
        for dir in &level {
            let Ok(entries) = fs::read_dir(dir) else { continue };
            for entry in entries.flatten() {
                // file_type() doesn't follow symlinks, so linked directories
                // can't send the search in circles.
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if skip_hidden && name.starts_with('.') {
                    continue;
                }
                if name.contains(needle) {
                    hits.push((name != needle, entry.path()));
                }
                next.push(entry.path());
            }
        }
        if let Some((_, best)) = hits.into_iter().min() {
            return Some(best);
        }
        level = next;
    }
    None
}

/// Shell glue. Anything nav prints that isn't a directory (help, version)
/// is passed through instead of being handed to `cd`.
fn hook(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => {
            r#"nav() {
    local out
    out="$(command nav "$@")" || return
    if [ -d "$out" ]; then
        cd -- "$out"
    else
        printf '%s\n' "$out"
    fi
}
"#
        }
        Shell::Fish => {
            r#"function nav
    set -l out (command nav $argv); or return
    if test (count $out) -eq 1; and test -d "$out"
        cd -- $out
    else
        printf '%s\n' $out
    end
end
"#
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidies_like_cd() {
        assert_eq!(tidy(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(tidy(Path::new("/../..")), Path::new("/"));
    }

    #[test]
    fn finds_the_closest_match() {
        let root = env::temp_dir().join(format!("nav-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["apps/web/src", "apps/website", "deep/a/b/web", ".hidden/web2", "lib/Web"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }

        // `web` exists at depth 2 in three places; the exact names win, alphabetically.
        assert_eq!(search(&root, "web", 4), Some(root.join("apps/web")));
        assert_eq!(search(&root, "site", 4), Some(root.join("apps/website")));
        assert_eq!(search(&root, "src", 4), Some(root.join("apps/web/src")));
        assert_eq!(search(&root, "web2", 4), None);
        assert_eq!(search(&root, ".hid", 4), Some(root.join(".hidden")));
        assert_eq!(search(&root, "src", 2), None);

        assert_eq!(resolve(&root, Path::new("apps/web/.."), 4).unwrap(), root.join("apps"));
        assert!(resolve(&root, Path::new("apps/nope"), 4).is_err());

        fs::remove_dir_all(&root).unwrap();
    }
}
