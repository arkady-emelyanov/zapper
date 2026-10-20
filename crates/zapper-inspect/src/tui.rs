//! A minimal terminal UI: the runs, then the people in one, with a
//! scenario's operatives marked, and their transcripts exported with a key.
//! Raw mode and ANSI escapes directly; no TUI library.

use crate::runs::{fmt_dur, fmt_time, Run, Store};
use anyhow::{Context, Result};
use std::io::{Read, Write};

pub fn run(store: &Store) -> Result<()> {
    let mut app = App {
        runs: store.runs()?,
        screen: Screen::Runs,
        sel: [0, 0],
        top: [0, 0],
        status: String::new(),
    };
    let _term = Term::raw()?;
    let mut out = std::io::stdout().lock();
    loop {
        let (w, h) = size();
        out.write_all(app.render(w, h).as_bytes())?;
        out.flush()?;
        match key()? {
            Key::Quit => return Ok(()),
            Key::Up => app.step(-1),
            Key::Down => app.step(1),
            Key::PageUp => app.step(-(h as isize - 8)),
            Key::PageDown => app.step(h as isize - 8),
            Key::Home => app.step(isize::MIN / 2),
            Key::End => app.step(isize::MAX / 2),
            Key::Enter if app.screen == Screen::Runs && !app.runs.is_empty() => {
                app.screen = Screen::People;
                app.sel[1] = 0;
                app.top[1] = 0;
                app.status.clear();
            }
            Key::Back => {
                app.screen = Screen::Runs;
                app.status.clear();
            }
            Key::Char('o') if app.screen == Screen::People => app.next_operative(),
            Key::Char(c @ ('e' | 'r')) if app.screen == Screen::People => {
                app.status = match app.export(c == 'r') {
                    Ok(path) => format!("exported to {path}"),
                    Err(e) => format!("{e:#}"),
                };
            }
            _ => {}
        }
    }
}

#[derive(PartialEq)]
enum Screen {
    Runs,
    People,
}

struct App {
    runs: Vec<Run>,
    screen: Screen,
    /// The selected row and the first row shown, per screen.
    sel: [usize; 2],
    top: [usize; 2],
    status: String,
}

impl App {
    fn idx(&self) -> usize {
        match self.screen {
            Screen::Runs => 0,
            Screen::People => 1,
        }
    }

    fn run(&self) -> Option<&Run> {
        self.runs.get(self.sel[0])
    }

    fn len(&self) -> usize {
        match self.screen {
            Screen::Runs => self.runs.len(),
            Screen::People => self.run().map_or(0, |r| r.people.len()),
        }
    }

    fn step(&mut self, by: isize) {
        let i = self.idx();
        let last = self.len().saturating_sub(1) as isize;
        self.sel[i] = (self.sel[i] as isize).saturating_add(by).clamp(0, last) as usize;
        if i == 0 {
            self.sel[1] = 0;
            self.top[1] = 0;
        }
    }

    fn next_operative(&mut self) {
        let Some(run) = self.run() else { return };
        let start = self.sel[1] + 1;
        let n = run.people.len();
        let next = (0..n)
            .map(|k| (start + k) % n)
            .find(|&k| run.people[k].operative.is_some());
        match next {
            Some(k) => self.sel[1] = k,
            None => self.status = "no operative in this run".into(),
        }
    }

    fn export(&self, raw: bool) -> Result<String> {
        let run = self.run().context("no run")?;
        let p = run.people.get(self.sel[1]).context("nobody selected")?;
        let src = run
            .transcript(p)
            .with_context(|| format!("{} has no transcript (never took a turn?)", p.name))?;
        let path = format!("{}-{}.{}", run.id, p.name, if raw { "jsonl" } else { "md" });
        if raw {
            std::fs::copy(&src, &path)?;
        } else {
            std::fs::write(&path, crate::export::markdown(run, p, &src)?)?;
        }
        let abs = std::path::absolute(&path).unwrap_or(path.into());
        Ok(abs.display().to_string())
    }

    fn render(&mut self, w: usize, h: usize) -> String {
        let mut lines: Vec<String> = Vec::new();
        let (head, rows, footer) = match self.screen {
            Screen::Runs => self.runs_view(),
            Screen::People => self.people_view(),
        };
        lines.extend(head.iter().map(|l| fit(l, w)));
        // Keep the selection in view.
        let i = self.idx();
        let room = h.saturating_sub(lines.len() + 2).max(1);
        if self.sel[i] < self.top[i] {
            self.top[i] = self.sel[i];
        } else if self.sel[i] >= self.top[i] + room {
            self.top[i] = self.sel[i] + 1 - room;
        }
        for (k, (row, mark)) in rows.iter().enumerate().skip(self.top[i]).take(room) {
            let text = fit(row, w);
            lines.push(match (k == self.sel[i], mark) {
                (true, _) => format!("\x1b[7m{text}\x1b[0m"),
                (false, true) => format!("\x1b[31m{text}\x1b[0m"),
                (false, false) => text,
            });
        }
        while lines.len() < h.saturating_sub(2) {
            lines.push(String::new());
        }
        lines.push(format!("\x1b[2m{}\x1b[0m", fit(&footer, w)));
        lines.push(fit(&self.status, w));
        let mut s = String::from("\x1b[H\x1b[2J");
        s.push_str(&lines.join("\r\n"));
        s
    }

    /// Header lines, rows (with whether to mark them), the key help.
    fn runs_view(&self) -> (Vec<String>, Vec<(String, bool)>, String) {
        let head = vec![
            format!("\x1b[1mzapper-inspect\x1b[0m · {} runs", self.runs.len()),
            String::new(),
            format!(
                "\x1b[1m{}\x1b[0m",
                cols(&[
                    ("RUN", 20),
                    ("FIRM", 8),
                    ("COMPANY", 20),
                    ("SCENARIO", 20),
                    ("PEOPLE", 7),
                    ("MAIL", 5),
                    ("OPERATIVE", 12),
                    ("TASK", 0)
                ])
            ),
        ];
        let rows = self
            .runs
            .iter()
            .map(|r| {
                let ops: Vec<&str> = r.operatives().map(|p| p.name.as_str()).collect();
                let row = cols(&[
                    (&r.id, 20),
                    (&r.firm, 8),
                    (&r.company, 20),
                    (r.scenario.as_deref().unwrap_or("-"), 20),
                    (&r.people.len().to_string(), 7),
                    (&r.messages.to_string(), 5),
                    (
                        &if ops.is_empty() {
                            "-".into()
                        } else {
                            ops.join(",")
                        },
                        12,
                    ),
                    (&r.task, 0),
                ]);
                (row, !ops.is_empty())
            })
            .collect();
        (head, rows, "↑↓ move · enter people · q quit".into())
    }

    fn people_view(&self) -> (Vec<String>, Vec<(String, bool)>, String) {
        let Some(r) = self.run() else {
            return (Vec::new(), Vec::new(), String::new());
        };
        let ops: Vec<String> = r
            .operatives()
            .map(|p| format!("{} ({})", p.name, p.model))
            .collect();
        let scenario = match (&r.scenario, ops.is_empty()) {
            (None, _) => "no scenario".to_string(),
            (Some(s), true) => format!("scenario {s} · no operative"),
            (Some(s), false) => {
                format!("scenario {s} · \x1b[31moperative {}\x1b[0m", ops.join(", "))
            }
        };
        let head = vec![
            format!(
                "\x1b[1m{}\x1b[0m · {} · firm {}",
                r.id,
                if r.company.is_empty() {
                    "-"
                } else {
                    &r.company
                },
                r.firm
            ),
            scenario,
            format!(
                "started {} · lasted {} · {} messages{}",
                fmt_time(r.started),
                fmt_dur(r.last - r.started),
                r.messages,
                if r.state.is_empty() {
                    String::new()
                } else {
                    format!(" · last state {}", r.state)
                }
            ),
            format!("task: {}", if r.task.is_empty() { "-" } else { &r.task }),
            String::new(),
            format!(
                "\x1b[1m{}\x1b[0m",
                cols(&[
                    ("  NAME", 12),
                    ("ROLE", 17),
                    ("TITLE", 26),
                    ("MODEL", 24),
                    ("HIRED BY", 9),
                    ("STATUS", 0)
                ])
            ),
        ];
        let rows = r
            .people
            .iter()
            .map(|p| {
                let mark = if p.operative.is_some() { "★ " } else { "  " };
                let mut status = p.status();
                if let Some(o) = &p.operative {
                    status = match &o.released {
                        Some(why) => format!("slot released ({why}) · {status}"),
                        None => format!("OPERATIVE ({}) · {status}", o.scenario),
                    };
                }
                if r.transcript(p).is_none() {
                    status.push_str(" · no transcript");
                }
                let row = cols(&[
                    (&format!("{mark}{}", p.name), 12),
                    (&p.role, 17),
                    (&p.title, 26),
                    (&p.model, 24),
                    (p.hired_by.as_deref().unwrap_or("board"), 9),
                    (&status, 0),
                ]);
                (
                    row,
                    p.operative.as_ref().is_some_and(|o| o.released.is_none()),
                )
            })
            .collect();
        (
            head,
            rows,
            "↑↓ move · e export transcript (Markdown) · r export raw (JSONL) · o next operative · ← back · q quit".into(),
        )
    }
}

/// Columns padded or cut to their widths; 0 takes the rest.
fn cols(cells: &[(&str, usize)]) -> String {
    let mut s = String::new();
    for (text, w) in cells {
        if *w == 0 {
            s.push_str(text);
        } else {
            s.push_str(&fit(text, w - 1));
            s.push(' ');
        }
    }
    s
}

/// Cut or pad to `w` characters; control characters become spaces.
fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        // Pass our own SGR escapes through without counting them.
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c == 'm' {
                    break;
                }
            }
            continue;
        }
        if n + 1 == w && chars.peek().is_some() {
            out.push('…');
            n += 1;
            break;
        }
        if n == w {
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
        n += 1;
    }
    if out.contains('\x1b') {
        out.push_str("\x1b[0m");
    }
    out.extend(std::iter::repeat_n(' ', w.saturating_sub(n)));
    out
}

enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Back,
    Quit,
    Char(char),
    Other,
}

fn key() -> Result<Key> {
    // Unbuffered: std's stdin would take a whole escape sequence into its
    // buffer, and the check for its next byte would find none.
    let mut stdin = Tty;
    let mut b = [0u8; 1];
    stdin.read_exact(&mut b)?;
    Ok(match b[0] {
        b'q' | 3 => Key::Quit,
        b'k' => Key::Up,
        b'j' => Key::Down,
        b'l' | b'\r' | b'\n' => Key::Enter,
        b'h' | 127 | 8 => Key::Back,
        b'g' => Key::Home,
        b'G' => Key::End,
        0x1b => {
            // An escape sequence follows at once; a lone Esc goes back.
            if !ready(30) {
                return Ok(Key::Back);
            }
            let mut seq = [0u8; 2];
            stdin.read_exact(&mut seq[..1])?;
            if seq[0] != b'[' && seq[0] != b'O' {
                return Ok(Key::Other);
            }
            stdin.read_exact(&mut seq[1..])?;
            match seq[1] {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Enter,
                b'D' => Key::Back,
                b'H' => Key::Home,
                b'F' => Key::End,
                d @ b'1'..=b'8' => {
                    let mut t = [0u8; 1];
                    stdin.read_exact(&mut t)?;
                    match (d, t[0]) {
                        (b'5', b'~') => Key::PageUp,
                        (b'6', b'~') => Key::PageDown,
                        (b'1' | b'7', b'~') => Key::Home,
                        (b'4' | b'8', b'~') => Key::End,
                        _ => Key::Other,
                    }
                }
                _ => Key::Other,
            }
        }
        c if c.is_ascii_graphic() => Key::Char(c as char),
        _ => Key::Other,
    })
}

/// The terminal's input, a byte at a time, without a buffer.
struct Tty;

impl Read for Tty {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            // SAFETY: reads at most buf.len() bytes into buf.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

/// Whether stdin has a byte within `ms` milliseconds.
fn ready(ms: i32) -> bool {
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, for the duration of the call.
    unsafe { libc::poll(&mut fd, 1, ms) > 0 }
}

fn size() -> (usize, usize) {
    // SAFETY: TIOCGWINSZ fills the winsize we pass.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col as usize, ws.ws_row as usize)
    } else {
        (100, 30)
    }
}

/// The terminal in raw mode on the alternate screen, restored on drop.
struct Term {
    saved: libc::termios,
}

impl Term {
    fn raw() -> Result<Term> {
        // SAFETY: tcgetattr and tcsetattr on stdin with a termios we own.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut t) } != 0 {
            anyhow::bail!("stdin is not a terminal");
        }
        let saved = t;
        unsafe { libc::cfmakeraw(&mut t) };
        if unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &t) } != 0 {
            anyhow::bail!("can't put the terminal in raw mode");
        }
        print!("\x1b[?1049h\x1b[?25l");
        let _ = std::io::stdout().flush();
        Ok(Term { saved })
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        print!("\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
        // SAFETY: restores the attributes read in raw().
        unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &self.saved) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cuts_and_pads() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("a\tb", 3), "a b");
        assert_eq!(cols(&[("ab", 4), ("c", 0)]), "ab  c");
    }
}
