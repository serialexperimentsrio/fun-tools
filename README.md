<h1 align="center">Fun Tools</h1>

<p align="center">
  <img src="Fun%20Tools.png" alt="Fun Tools logo">
</p>

**Fun Tools** is a set of small file, text and system tools for Linux, written in Rust. The long-term goal is a friendlier replacement for GNU coreutils: safe by default, consistent flags across every tool, readable output, and `--json` where it makes sense.

## Tools

| Tool | What it does |
|------|--------------|
| `cop` | Copy files, directories, piped text or lines of a file to the system clipboard |
| `put` | Paste from the clipboard: files into a directory, text to stdout, a new file, or into a file at a line |
| `sel` | Keep a selection of paths for `cop`, `mov` and `del` to act on |
| `mov` | Move or rename files and directories |
| `del` | Delete safely: everything goes to a dump and can be restored |
| `new` | Create files or directories, including missing parents |
| `dog` | Print files, optionally numbered or just the first/last lines |
| `eco` | Print text, or pass stdin through |
| `idx` | List a directory, optionally as a tree |
| `loc` | Find files and directories by name |
| `hid` | Show information about the machine |
| `usg` | Show CPU, memory, swap and disk usage, and the busiest processes |
| `dat` | Show the date or time |
| `mop` | Clear the screen |
| `ted` | Text editor (not written yet) |

Every tool has `--help`.

## Examples

```sh
sel add *.log old/           # pick some paths
del add -s                   # dump them all
del ls                       # see what's in the dump
del restore 3                # changed my mind about one

cop report.pdf && cd ~/docs && put      # copy here, paste there
cop report.pdf --to ~/docs               # or in one go
cop -x notes/ && cd ~/archive && put     # cut and paste a directory
git diff | cop                           # copy text, paste anywhere

cop --lines 10-20 main.rs                # copy lines 10 to 20 of a file
put --at 5 other.rs                      # paste them in at line 5
put --at end log.txt                     # or at the end (or --at start)

idx -lr --depth 2            # two levels deep, with sizes and dates
loc '*.rs' src -t file       # every Rust file under src
usg -p 5                     # usage plus the five busiest processes
hid -o os,cpu,ram --json
```

## How the flags work

A tool that does one thing has no subcommands: `dog file.txt`. A tool that does several things takes the action as a subcommand: `sel add`, `del restore`. Flags only ever change how the action behaves, and a letter means the same thing in every tool that has it:

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

Nothing is overwritten without `-f`. Colors are used on a terminal only and can be turned off with `NO_COLOR=1`.

State (the selection and the dump) lives in `$XDG_DATA_HOME/fun-tools`, which is usually `~/.local/share/fun-tools`.

## Building

You need Rust and a C toolchain for linking (`build-essential` on Debian and Ubuntu).

```sh
cargo build --release
cargo test

# install one tool, or all of them
cargo install --path crates/dog
for t in cop dat del dog eco hid idx loc mop mov new put sel usg; do cargo install --path crates/$t; done
```

`cop` and `put` need a graphical session (X11 or Wayland). Under WSL that's WSLg, and copied text also shows up in the Windows clipboard.
