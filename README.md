<p align="center">
  <img src="Fun%20Tools.png" alt="Fun Tools logo" width="600">
</p>

<h1 align="center">Fun Tools</h1>

<p align="center">
  <a href="https://github.com/serialexperimentsrio/fun-tools/actions/workflows/ci.yml"><img src="https://github.com/serialexperimentsrio/fun-tools/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust" alt="Rust 1.85+">
  <img src="https://img.shields.io/badge/platform-Linux-lightgrey?logo=linux&logoColor=white" alt="Platform: Linux">
</p>

**Fun Tools** is a set of small file, text and system tools for Linux, written in Rust. The long-term goal is a friendlier replacement for GNU coreutils: safe by default, consistent flags across every tool, readable output, and `--json` where it makes sense.

## Tools

| Tool | What it does |
|------|--------------|
| `cop` | Copy files, directories, piped text or lines of a file to the system clipboard |
| `put` | Paste from the clipboard: files into a directory, text to stdout, a new file, or into a file at a line |
| `sel` | Keep a selection of paths for `cop`, `mov` and `del` to act on: `sel <paths>` adds, `sel mop` takes out (all of it with no paths), `sel idx` lists |
| `mov` | Move or rename files and directories |
| `del` | Delete safely: `del file` goes to a dump; `del idx`, `del restore`, `del empty` manage it |
| `new` | Create files or directories, including missing parents |
| `dog` | Print files, optionally numbered or just the first/last lines |
| `eco` | Print text, or pass stdin through |
| `nav` | Change directory by path, by part of a name, or back to the previous one |
| `idx` | List a directory, optionally as a tree |
| `loc` | Find files and directories by name |
| `hid` | Show information about the machine |
| `usg` | Show CPU, memory, swap and disk usage, and the busiest processes |
| `dat` | Show the date or time |
| `mop` | Clear the screen |
| `ted` | Text editor (not written yet) |

Every tool except `ted` has `--help`. Wherever a subcommand is called `idx`, `index` works too.

## Examples

```sh
sel *.log old/               # pick some paths
sel mop old/                 # changed my mind about one (the folder stays put)
sel idx                      # what's selected
sel mop                      # start over: empty the selection
del -s                       # dump everything selected

del notes.txt build/         # delete straight away (to the dump)
del idx                      # see what's in the dump
del restore 3                # put one back
del empty --older-than 30    # purge for good (asks first)
del ./idx                    # a file named like a subcommand

cop report.pdf && nav ~/docs && put     # copy here, paste there
cop report.pdf --to ~/docs               # or in one go
cop -x notes/ && nav ~/archive && put    # cut and paste a directory
git diff | cop                           # copy text, paste anywhere

cop --lines 10-20 main.rs                # copy lines 10 to 20 of a file
put --at 5 other.rs                      # paste them in at line 5
put --at end log.txt                     # or at the end (or --at start)

nav web                      # jump to the nearest directory named like `web`
nav -u 2                     # two levels up
nav -                        # back to where you were

idx -lr --depth 2            # two levels deep, with sizes and dates
loc '*.rs' src -t file       # every Rust file under src
usg -p 5                     # usage plus the five busiest processes
hid -o os,cpu,ram --json
```

## How the flags work

A tool that does one thing has no subcommands: `dog file.txt`. A tool that does several things takes the action as a subcommand: `del restore`, `sel mop`. In `del` and `sel` the main action needs no subcommand: `del file` deletes and `sel file` selects, while the subcommands look after the dump and the selection. A file named like a subcommand is written as a path, e.g. `del ./idx`.

Flags change how the action behaves. These shared ones mean the same thing in every tool that has them:

| Flag | Meaning |
|------|---------|
| `-a, --all` | Include hidden entries |
| `-f, --force` | Replace what's already there |
| `-s, --selected` | Act on the selection instead of path arguments |
| `-o, --only` | Show only these fields or resources |
| `-r, --recursive` | Walk into subdirectories |
| `--depth` | Limit how far down to go |
| `--dry-run` | Show what would happen without changing anything |
| `--json` | Machine-readable output |

Other short flags belong to one tool and are listed in its `--help`. Nothing is overwritten without `-f`. Colors are used on a terminal only and can be turned off with `NO_COLOR=1`.

State (the selection, the dump and a pending cut) lives in `$XDG_DATA_HOME/fun-tools`, which is usually `~/.local/share/fun-tools`.

## Building

You need Rust and a C toolchain for linking (`build-essential` on Debian and Ubuntu).

```sh
cargo build --release
cargo test

# install one tool, or all of them
cargo install --path crates/dog
for t in cop dat del dog eco hid idx loc mop mov nav new put sel usg; do cargo install --path crates/$t; done
```

`nav` needs one line in your shell's startup file, because a program can't change the directory of the shell that started it. `nav` works out where to go and a small shell function does the `cd`:

```sh
eval "$(nav --init bash)"     # ~/.bashrc
eval "$(nav --init zsh)"      # ~/.zshrc
nav --init fish | source     # ~/.config/fish/config.fish
```

`cop` and `put` use the system clipboard, so they need a graphical session (X11 or Wayland); `cop --to` doesn't. Under WSL that's WSLg, and copied text also shows up in the Windows clipboard.

## License

Copyright 2026 Rio <engineer@disr.it>

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
