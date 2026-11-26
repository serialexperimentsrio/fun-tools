use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use common::At;

/// Edit a text file in the terminal. Type to edit, and move around with the
/// arrow keys, Home/End and Page Up/Down. Ctrl+S saves, Ctrl+Q quits.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The file to edit. If it doesn't exist yet, it's created on the first save
    file: PathBuf,
}

/// How many columns a tab takes on screen.
const TAB: usize = 4;

fn main() -> ExitCode {
    common::run(|| {
        let cli = Cli::parse();
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("needs a terminal to run in".into());
        }
        let mut editor = Editor::open(&cli.file)?;
        let _terminal = Terminal::enter()?;
        editor.run()
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
    /// A key or sequence ted doesn't use, like Ctrl+Right.
    Ignored,
}

struct Editor {
    path: PathBuf,
    lines: Vec<Vec<char>>,
    /// The file had Windows line endings, so it's saved with them again.
    crlf: bool,
    /// The cursor, as a line and a character within that line.
    cy: usize,
    cx: usize,
    /// The first line and screen column on show.
    row_off: usize,
    col_off: usize,
    dirty: bool,
    /// Set by Ctrl+Q with unsaved changes; a second Ctrl+Q in a row quits anyway.
    quit_armed: bool,
    message: String,
}

impl Editor {
    fn open(path: &Path) -> common::Result<Editor> {
        let (lines, crlf) = match fs::read(path) {
            Ok(bytes) => parse(bytes).ok_or_else(|| format!("{}: not a text file", path.display()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (vec![Vec::new()], false),
            Err(e) => return Err(e).at(path),
        };
        Ok(Editor::new(path.to_path_buf(), lines, crlf))
    }

    fn new(path: PathBuf, lines: Vec<Vec<char>>, crlf: bool) -> Editor {
        Editor {
            path,
            lines,
            crlf,
            cy: 0,
            cx: 0,
            row_off: 0,
            col_off: 0,
            dirty: false,
            quit_armed: false,
            message: "Ctrl+S save · Ctrl+Q quit".to_string(),
        }
    }

    fn run(&mut self) -> common::Result {
        let mut out = io::stdout().lock();
        let mut input = io::stdin().lock();
        let mut redraw = true;
        let mut last_size = (0, 0);
        loop {
            let (rows, cols) = window_size();
            // Two rows go to the status bar and the message line.
            let text_rows = rows.saturating_sub(2).max(1);
            // Reads time out every 0.1 s, which is also when a resized window gets redrawn.
            if redraw || (rows, cols) != last_size {
                self.draw(&mut out, text_rows, cols)?;
                last_size = (rows, cols);
            }
            redraw = match read_key(&mut input)? {
                Some(key) => {
                    if !self.handle(key, text_rows) {
                        return Ok(());
                    }
                    true
                }
                None => false,
            };
        }
    }

    /// Applies one key. False means it's time to quit.
    fn handle(&mut self, key: Key, page: usize) -> bool {
        let quit_armed = std::mem::take(&mut self.quit_armed);
        match key {
            Key::Ctrl(b'q') => {
                if self.dirty && !quit_armed {
                    self.quit_armed = true;
                    self.message = "Unsaved changes. Ctrl+Q again to quit without saving, or Ctrl+S to save".into();
                    return true;
                }
                return false;
            }
            Key::Ctrl(b's') => self.save(),
            Key::Char(c) => self.insert(c),
            Key::Enter => self.newline(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Ctrl(_) | Key::Esc | Key::Ignored => {}
            movement => self.move_cursor(movement, page),
        }
        true
    }

    fn save(&mut self) {
        let text = contents(&self.lines, self.crlf);
        self.message = match common::write_file(&self.path, text.as_bytes()).at(&self.path) {
            Ok(()) => {
                self.dirty = false;
                format!("Saved {} bytes", text.len())
            }
            Err(e) => format!("Can't save: {e}"),
        };
    }

    fn insert(&mut self, c: char) {
        self.lines[self.cy].insert(self.cx, c);
        self.cx += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let rest = self.lines[self.cy].split_off(self.cx);
        self.lines.insert(self.cy + 1, rest);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
            self.lines[self.cy].remove(self.cx);
        } else if self.cy > 0 {
            let line = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
            self.lines[self.cy].extend(line);
        } else {
            return;
        }
        self.dirty = true;
    }

    fn delete(&mut self) {
        if self.cx < self.lines[self.cy].len() {
            self.lines[self.cy].remove(self.cx);
        } else if self.cy + 1 < self.lines.len() {
            let next = self.lines.remove(self.cy + 1);
            self.lines[self.cy].extend(next);
        } else {
            return;
        }
        self.dirty = true;
    }

    // ponytail: moving up or down forgets the column you came from on a
    // shorter line; remember a "wanted column" if that gets annoying.
    fn move_cursor(&mut self, key: Key, page: usize) {
        let last = self.lines.len() - 1;
        match key {
            Key::Left if self.cx > 0 => self.cx -= 1,
            Key::Left if self.cy > 0 => {
                self.cy -= 1;
                self.cx = self.lines[self.cy].len();
            }
            Key::Right if self.cx < self.lines[self.cy].len() => self.cx += 1,
            Key::Right if self.cy < last => {
                self.cy += 1;
                self.cx = 0;
            }
            Key::Up => self.cy = self.cy.saturating_sub(1),
            Key::Down => self.cy = (self.cy + 1).min(last),
            Key::PageUp => self.cy = self.cy.saturating_sub(page),
            Key::PageDown => self.cy = (self.cy + page).min(last),
            Key::Home => self.cx = 0,
            Key::End => self.cx = self.lines[self.cy].len(),
            _ => {}
        }
        self.cx = self.cx.min(self.lines[self.cy].len());
    }

    /// Draws the whole screen in one write, so it doesn't flicker.
    fn draw(&mut self, out: &mut impl Write, rows: usize, cols: usize) -> io::Result<()> {
        let rx = render(&self.lines[self.cy][..self.cx]).len();
        // Scroll just enough to keep the cursor on screen.
        if self.cy < self.row_off {
            self.row_off = self.cy;
        }
        if self.cy >= self.row_off + rows {
            self.row_off = self.cy + 1 - rows;
        }
        if rx < self.col_off {
            self.col_off = rx;
        }
        if rx >= self.col_off + cols {
            self.col_off = rx + 1 - cols;
        }

        let mut frame = String::from("\x1b[?25l\x1b[H");
        for y in self.row_off..self.row_off + rows {
            match self.lines.get(y) {
                Some(line) => frame.extend(render(line).into_iter().skip(self.col_off).take(cols)),
                None => frame.push_str("\x1b[2m~\x1b[m"),
            }
            frame.push_str("\x1b[K\r\n");
        }

        let name = common::escaped(self.path.as_os_str());
        let left = format!(" {name}{}", if self.dirty { " (modified)" } else { "" });
        let right = format!("line {}/{}, col {} ", self.cy + 1, self.lines.len(), self.cx + 1);
        let gap = cols.saturating_sub(left.chars().count() + right.chars().count());
        let status: String = format!("{left}{}{right}", " ".repeat(gap)).chars().take(cols).collect();
        frame.push_str(&format!("\x1b[7m{status}\x1b[m\r\n\x1b[K"));
        frame.extend(self.message.chars().take(cols));
        frame.push_str(&format!("\x1b[{};{}H\x1b[?25h", self.cy - self.row_off + 1, rx - self.col_off + 1));
        out.write_all(frame.as_bytes())?;
        out.flush()
    }
}

/// Splits a file into lines. A file that ends in a newline gets an empty last
/// line, so saving puts that newline back. Windows line endings are noted and
/// stripped, and written back the same way. None for binary or non-UTF-8
/// files, which an editor this simple would mangle.
fn parse(bytes: Vec<u8>) -> Option<(Vec<Vec<char>>, bool)> {
    if bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let crlf = text.contains('\n') && text.split('\n').next().is_some_and(|l| l.ends_with('\r'));
    let lines = text
        .split('\n')
        .map(|l| if crlf { l.strip_suffix('\r').unwrap_or(l) } else { l })
        .map(|l| l.chars().collect())
        .collect();
    Some((lines, crlf))
}

/// The file's text again, ready to save.
fn contents(lines: &[Vec<char>], crlf: bool) -> String {
    let nl = if crlf { "\r\n" } else { "\n" };
    lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join(nl)
}

/// A line as it looks on screen: tabs spread out to the next stop, and
/// control characters shown as `?` so they can't drive the terminal.
// ponytail: every other character counts as one column, so the cursor
// drifts on lines with wide characters (CJK, emoji); measure widths if needed.
fn render(line: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(line.len());
    for &c in line {
        if c == '\t' {
            out.push(' ');
            while out.len() % TAB != 0 {
                out.push(' ');
            }
        } else if c.is_control() {
            out.push('?');
        } else {
            out.push(c);
        }
    }
    out
}

/// The next key, or None when nothing was pressed within the read timeout.
fn read_key(input: &mut impl Read) -> io::Result<Option<Key>> {
    let Some(byte) = read_byte(input)? else { return Ok(None) };
    Ok(Some(match byte {
        b'\r' | b'\n' => Key::Enter,
        127 | 8 => Key::Backspace,
        b'\t' => Key::Char('\t'),
        0x1b => escape(input)?,
        1..=26 => Key::Ctrl(b'a' + byte - 1),
        0..=31 => Key::Ignored,
        0x80.. => utf8(byte, input)?,
        _ => Key::Char(byte as char),
    }))
}

fn read_byte(input: &mut impl Read) -> io::Result<Option<u8>> {
    let mut byte = [0];
    match input.read(&mut byte) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(byte[0])),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
        Err(e) => Err(e),
    }
}

/// What follows an Esc byte: arrows and such arrive as `ESC [ A` or
/// `ESC [ 3 ~`. Esc on its own is followed by nothing.
fn escape(input: &mut impl Read) -> io::Result<Key> {
    let Some(first) = read_byte(input)? else { return Ok(Key::Esc) };
    let Some(second) = read_byte(input)? else { return Ok(Key::Esc) };
    Ok(match (first, second) {
        (b'[', b'A') => Key::Up,
        (b'[', b'B') => Key::Down,
        (b'[', b'C') => Key::Right,
        (b'[', b'D') => Key::Left,
        (b'[' | b'O', b'H') => Key::Home,
        (b'[' | b'O', b'F') => Key::End,
        (b'[', digit @ b'0'..=b'9') => {
            // Read to the end of the sequence, so one with modifiers
            // (`ESC [ 1 ; 5 C` for Ctrl+Right) doesn't leak into the text.
            let mut longer = false;
            let mut end = read_byte(input)?;
            while end.is_some_and(|c| c != b'~' && !c.is_ascii_alphabetic()) {
                longer = true;
                end = read_byte(input)?;
            }
            match (digit, longer, end) {
                (b'1' | b'7', false, Some(b'~')) => Key::Home,
                (b'4' | b'8', false, Some(b'~')) => Key::End,
                (b'3', false, Some(b'~')) => Key::Delete,
                (b'5', false, Some(b'~')) => Key::PageUp,
                (b'6', false, Some(b'~')) => Key::PageDown,
                _ => Key::Ignored,
            }
        }
        _ => Key::Ignored,
    })
}

/// The rest of a character typed as several UTF-8 bytes, like `é`.
fn utf8(first: u8, input: &mut impl Read) -> io::Result<Key> {
    let len = match first {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return Ok(Key::Ignored),
    };
    let mut bytes = vec![first];
    for _ in 1..len {
        match read_byte(input)? {
            Some(b) => bytes.push(b),
            None => return Ok(Key::Ignored),
        }
    }
    Ok(std::str::from_utf8(&bytes).ok().and_then(|s| s.chars().next()).map_or(Key::Ignored, Key::Char))
}

/// Raw mode and the alternate screen for as long as this lives, so the
/// terminal is put back however ted exits, a panic included.
struct Terminal {
    saved: libc::termios,
}

impl Terminal {
    fn enter() -> io::Result<Terminal> {
        let mut saved = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills `saved` when it succeeds, and it's only read after that.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let saved = unsafe { saved.assume_init() };
        let mut raw = saved;
        // Keys arrive one at a time and unechoed, with Ctrl+C, S, Q and Z as plain keys.
        raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cflag |= libc::CS8;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
        // A read waits at most 0.1 s, which is how a lone Esc is told apart
        // from the start of an arrow key's sequence.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        // SAFETY: raw is a complete termios, copied from the current settings.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let terminal = Terminal { saved };
        let mut out = io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[2J")?;
        out.flush()?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[?1049l");
        let _ = out.flush();
        // SAFETY: puts back the settings read in `enter`.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved) };
    }
}

/// Rows and columns of the terminal, or 24x80 if it won't say.
fn window_size() -> (usize, usize) {
    let mut size = MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: TIOCGWINSZ fills a winsize, and all zeroes is a valid one if it doesn't.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) } == 0;
    let size = unsafe { size.assume_init() };
    if ok && size.ws_row > 0 && size.ws_col > 0 { (size.ws_row.into(), size.ws_col.into()) } else { (24, 80) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let (lines, crlf) = parse(text.as_bytes().to_vec()).unwrap();
        Editor::new(PathBuf::from("test.txt"), lines, crlf)
    }

    fn text(e: &Editor) -> String {
        contents(&e.lines, e.crlf)
    }

    #[test]
    fn edits() {
        let mut e = editor("ab\ncd");
        e.move_cursor(Key::End, 1);
        e.insert('!');
        e.newline();
        e.insert('x');
        assert_eq!(text(&e), "ab!\nx\ncd");
        e.backspace();
        e.backspace();
        assert_eq!(text(&e), "ab!\ncd");
        e.move_cursor(Key::Down, 1);
        e.move_cursor(Key::Home, 1);
        e.backspace();
        assert_eq!(text(&e), "ab!cd");
        e.move_cursor(Key::End, 1);
        e.delete();
        assert_eq!(text(&e), "ab!cd", "delete at the very end does nothing");
        assert!(e.dirty);
    }

    #[test]
    fn keeps_line_endings() {
        for original in ["", "x\n", "a\nb", "a\r\nb\r\n"] {
            assert_eq!(text(&editor(original)), original);
        }
        assert!(parse(b"a\0b".to_vec()).is_none(), "binary");
        assert!(parse(b"\xff".to_vec()).is_none(), "not UTF-8");
    }

    #[test]
    fn asks_before_dropping_changes() {
        let mut e = editor("a");
        assert!(!e.handle(Key::Ctrl(b'q'), 1), "nothing to lose");
        e.insert('b');
        assert!(e.handle(Key::Ctrl(b'q'), 1), "first Ctrl+Q only warns");
        assert!(!e.handle(Key::Ctrl(b'q'), 1));
        e.handle(Key::Ctrl(b'q'), 1);
        e.handle(Key::Right, 1);
        assert!(e.handle(Key::Ctrl(b'q'), 1), "any other key in between resets it");
    }

    #[test]
    fn renders_tabs_and_controls() {
        assert_eq!(render(&"\tab\x1b".chars().collect::<Vec<_>>()).into_iter().collect::<String>(), "    ab?");
        assert_eq!(render(&['x', '\t']).len(), 4);
    }

    #[test]
    fn reads_keys() {
        let key = |bytes: &[u8]| read_key(&mut &bytes[..]).unwrap();
        assert_eq!(key(b"a"), Some(Key::Char('a')));
        assert_eq!(key("é".as_bytes()), Some(Key::Char('é')));
        assert_eq!(key(b"\x11"), Some(Key::Ctrl(b'q')));
        assert_eq!(key(b"\x1b[A"), Some(Key::Up));
        assert_eq!(key(b"\x1b[3~"), Some(Key::Delete));
        assert_eq!(key(b"\x1b[1;5C"), Some(Key::Ignored));
        assert_eq!(key(b"\x1b"), Some(Key::Esc));
        assert_eq!(key(b""), None);
    }
}
