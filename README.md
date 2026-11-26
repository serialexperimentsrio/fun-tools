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

**Fun Tools** is a set of small command-line tools for Linux, written in Rust. They handle the everyday file, text and system jobs that GNU coreutils does, with a few ideas of their own: deleting goes to a trash you can undo, copying can go through the system clipboard, and the same flag means the same thing in every tool.

It's a work in progress. The long-term goal is a friendlier replacement for coreutils: safe defaults, readable output, and `--json` wherever a script might want it.

## Tools

| Tool | What it does |
|------|--------------|
| `cop` | Copy files, folders, piped text or lines of a file to the clipboard, or straight to another folder or name |
| `put` | Paste from the clipboard: files into a folder, text to the terminal, into a new file, or into a file at a given line |
| `sel` | Keep a list of selected paths for `cop`, `mov` and `del` to work on |
| `mov` | Move or rename files and folders |
| `del` | Delete to a trash you can restore from |
| `new` | Create files or folders, along with any missing parents |
| `dog` | Print files, numbered if you like, or just the head or tail |
| `eco` | Print text |
| `nav` | Change directory by path, by part of a folder's name, or back to where you were |
| `idx` | List a folder, with details or as a tree |
| `loc` | Find files and folders by name or pattern |
| `hid` | Show what machine you're on: OS, CPU, memory, GPU and so on |
| `usg` | Show CPU, memory, swap and disk usage, and the busiest processes |
| `dat` | Show the date and time |
| `mop` | Clear the screen |
| `ted` | A text editor, not written yet |

Every tool except `ted` has `--help`.

## A quick tour

Copy and paste work the way they do in a file manager:

```sh
cop report.pdf && nav ~/docs && put      # copy here, paste there
cop -x notes/ && nav ~/archive && put    # cut and paste a folder
cop report.pdf --to ~/docs               # or skip the clipboard
cop report.pdf --to report-old.pdf       # and give it a new name
git diff | cop                           # copy text from a pipe
cop --lines 10-20 main.rs                # or a few lines of a file
put --at 5 other.rs                      # and paste them in at line 5
```

Pick paths once, then act on all of them:

```sh
sel *.log old/               # select some paths
sel mop old/                 # take one back out (the folder itself stays put)
sel idx                      # see what's selected
mov -s ~/archive/            # move them all
sel mop                      # or just start over
```

Delete without worrying about it:

```sh
del notes.txt build/             # off to the trash
idx ~/.trash                     # see what's in there
del undel ~/.trash/*notes.txt    # put it back where it was
del purge --older-than 7         # gone for good (asks first)
```

Get around and look around:

```sh
nav web                      # jump to the nearest folder named like "web"
nav -u 2                     # two levels up
nav -                        # back to where you were
idx -l --tree --depth 2      # a two-level tree with sizes and dates
loc '*.rs' src -t file       # every Rust file under src
dog --tail 50 app.log        # the end of a log
```

Check on the machine:

```sh
usg -p 5                     # usage plus the five busiest processes
usg --watch 2                # the same, refreshed every two seconds
hid -o os,cpu,ram            # just a few facts
dat -o time                  # just the time
hid --json                   # everything, for a script
```

## The trash

`del` never erases anything straight away. Files go to a trash folder on the same drive: `~/.trash` for anything on the drive your home is on, and `.trash` at the top of any other drive, like `/mnt/d/.trash`. That keeps deleting a quick rename, even for big folders. A trash folder only appears once you've deleted something on that drive.

Each item is named after the moment it was deleted, so you can tell at a glance when something went in, and when it's due to go:

```
2026-11-26_10-47-23_build/
2026-11-26_10-12-03_notes.txt
```

A hidden `.meta` folder next to them remembers where each one came from, so `del undel` puts it back in the same place under its own name (`del restore` works too). If something else has taken that spot in the meantime, it stops and tells you to use `-f`.

Anything that has been in the trash for more than 30 days is purged the next time you delete something. Set `FUN_TOOLS_TRASH_DAYS` to change that, or to `0` to keep everything until you run `del purge` yourself. `del purge` asks before it deletes anything, and in a script, where there's nobody to ask, it needs `--yes`.

The selection and a pending cut are kept separately, in `$XDG_DATA_HOME/fun-tools` (usually `~/.local/share/fun-tools`).

## How the flags work

A tool that does one thing has no subcommands: `dog file.txt`. A tool that does several things takes the action as a subcommand, like `del purge` or `sel mop`. In `del` and `sel` the everyday action needs no subcommand at all: `del file` deletes and `sel file` selects. Wherever a subcommand is called `idx`, `index` works too.

If a file happens to share its name with a subcommand, write it as a path (`del ./purge`) or put it after `--` (`del -- purge`). Scripts should always use `--` for names they didn't pick themselves: `del -- "$file"`.

Flags change how the action behaves. The shared ones mean the same thing in every tool that has them:

| Flag | Meaning |
|------|---------|
| `-a, --all` | Include hidden entries |
| `-f, --force` | Replace what's already there |
| `-s, --selected` | Act on the selection instead of path arguments |
| `-o, --only` | Show only these fields, parts or resources |
| `-r, --recursive` | Walk into subfolders (`--tree` in `idx`) |
| `--depth` | Limit how far down to go |
| `--dry-run` | Show what would happen without changing anything |
| `--json` | Machine-readable output |

Every other short flag belongs to a single tool, so a letter never means two different things. Nothing gets overwritten unless you pass `-f`. Colors only show up on a terminal, and `NO_COLOR=1` turns them off.

`idx`, `loc`, `sel idx`, `hid` and `usg` can all print JSON, and it has the same shape everywhere: lists are arrays of objects, sizes are in bytes, times are Unix seconds, and CPU is a percentage. An entry's `type` is `file`, `dir` or `link`, and anything that couldn't be found is `null`.

## Installing

You need Rust and a C toolchain for linking (`build-essential` on Debian and Ubuntu).

```sh
cargo build --release
cargo test

# install one tool, or all of them
cargo install --path crates/dog
for t in cop dat del dog eco hid idx loc mop mov nav new put sel usg; do cargo install --path crates/$t; done
```

`nav` needs one line in your shell's startup file. A program can't change the directory of the shell that started it, so `nav` works out where to go and a small shell function does the actual `cd`:

```sh
eval "$(nav --init bash)"     # ~/.bashrc
eval "$(nav --init zsh)"      # ~/.zshrc
nav --init fish | source      # ~/.config/fish/config.fish
```

`cop` and `put` talk to the system clipboard, so they need a graphical session (X11 or Wayland). `cop --to` leaves the clipboard alone and works anywhere. Under WSL the session comes from WSLg, and anything you copy shows up in the Windows clipboard too.

## License

Copyright 2026 Rio <engineer@disr.it>

Licensed under the MIT License. See [LICENSE](LICENSE) for details.
