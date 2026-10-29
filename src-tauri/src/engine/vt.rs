//! A minimal VT100/xterm screen model (plan C1a-20).
//!
//! Ported from the C0 spike (`vt.rs`, itself a port of `vt.py`). It knows
//! enough of xterm to render Claude Code's fullscreen and inline TUI into
//! text: cursor moves, erase, scroll regions, insert/delete lines and chars,
//! the alt screen. SGR, DCS and most private modes are parsed and ignored.
//! OSC 0/2 set `title` (the D24.4 ready signal). `queries` collects the
//! terminal queries the app sent; `query_reply` gives the answers.
//!
//! Changes from the spike: `resize`, a cap on string sequences, and at
//! least one row and one column, so no input can make it panic.

use std::collections::BTreeSet;

/// Longest OSC/DCS/APC body kept. Longer bodies (images, say) are cut;
/// their content is never needed.
const SEQ_MAX: usize = 4096;

/// Private modes `modes` keeps at most; later ones set are not recorded.
const MODES_MAX: usize = 64;

#[derive(Clone, Copy, PartialEq, Debug)]
enum St {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Str,
    StrEsc,
    Charset,
}

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    main: Vec<Vec<char>>,
    alt: Vec<Vec<char>>,
    pub in_alt: bool,
    pub x: usize,
    pub y: usize,
    top: usize,
    bot: usize,
    saved: (usize, usize),
    st: St,
    seq: String,
    /// Raw query strings that need a reply, e.g. "CSIc", "CSI>q", "OSC11;?".
    pub queries: Vec<String>,
    /// Last OSC 0 / OSC 2 window title.
    pub title: String,
    /// Bumped on every title change, so callers can see a repeat of the same title.
    pub title_seq: u64,
    /// Private modes set (`CSI ? N h`), at most `MODES_MAX`.
    pub modes: BTreeSet<u32>,
    utf8_pending: Vec<u8>,
}

/// Display width of one char. No unicode-width crate (dep budget): the
/// common wide ranges (CJK, Hangul, full-width forms, most emoji) are 2.
pub fn cwidth(c: char) -> usize {
    let u = c as u32;
    if u == 0 {
        return 0;
    }
    // Combining marks and zero-width joiners / variation selectors.
    if (0x0300..=0x036F).contains(&u)
        || (0x200B..=0x200F).contains(&u)
        || (0xFE00..=0xFE0F).contains(&u)
        || u == 0x20E3
    {
        return 0;
    }
    let wide = (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0x303E).contains(&u)
        || (0x3041..=0x33FF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x4E00..=0x9FFF).contains(&u)
        || (0xA000..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1F64F).contains(&u)
        || (0x1F900..=0x1F9FF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u);
    if wide {
        2
    } else {
        1
    }
}

/// The reply to one terminal query from `Screen::queries`, or `None` when
/// the engine stays silent. `cursor` is (row, col), zero-based. These are
/// the queries CC sends at startup (D24 REPORT Q1.6); without the replies it
/// waits for a timeout.
pub fn query_reply(q: &str, cursor: (usize, usize)) -> Option<String> {
    match q {
        "CSI6n" => Some(format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1)),
        "CSI5n" => Some("\x1b[0n".into()),
        "CSIc" | "CSI0c" => Some("\x1b[?62;22c".into()),
        "CSI?u" => None, // no kitty keyboard protocol
        _ if q.starts_with("CSI>") && q.ends_with('c') => Some("\x1b[>0;10;1c".into()),
        _ if q.starts_with("CSI>") && q.ends_with('q') => Some("\x1bP>|c9watch(1)\x1b\\".into()),
        _ if q.starts_with("OSC11") => Some("\x1b]11;rgb:0000/0000/0000\x1b\\".into()),
        _ if q.starts_with("OSC10") => Some("\x1b]10;rgb:ffff/ffff/ffff\x1b\\".into()),
        _ => None,
    }
}

impl Screen {
    /// A blank screen. Zero sizes count as 1.
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Screen {
            cols,
            rows,
            main: blank(cols, rows),
            alt: blank(cols, rows),
            in_alt: false,
            x: 0,
            y: 0,
            top: 0,
            bot: rows - 1,
            saved: (0, 0),
            st: St::Ground,
            seq: String::new(),
            queries: Vec::new(),
            title: String::new(),
            title_seq: 0,
            modes: Default::default(),
            utf8_pending: Vec::new(),
        }
    }

    fn buf(&mut self) -> &mut Vec<Vec<char>> {
        if self.in_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn buf_ref(&self) -> &Vec<Vec<char>> {
        if self.in_alt {
            &self.alt
        } else {
            &self.main
        }
    }

    /// The visible screen as text, trailing spaces trimmed per row.
    pub fn text(&self) -> String {
        self.buf_ref()
            .iter()
            .map(|r| {
                let s: String = r.iter().filter(|&&c| c != '\0').collect();
                s.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Change the size, like a terminal on SIGWINCH: rows are cut at the
    /// bottom, or at the top when the cursor row would fall off; columns are
    /// cut on the right. The scroll region resets.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let shift = (self.y + 1).saturating_sub(rows);
        for b in [&mut self.main, &mut self.alt] {
            b.drain(..shift.min(b.len()));
            b.truncate(rows);
            while b.len() < rows {
                b.push(vec![' '; cols]);
            }
            for r in b.iter_mut() {
                r.resize(cols, ' ');
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.y -= shift;
        self.top = 0;
        self.bot = rows - 1;
        self.clamp();
    }

    /// Feed raw PTY bytes. Keeps a partial UTF-8 sequence for the next call.
    pub fn feed_bytes(&mut self, b: &[u8]) {
        let mut data = std::mem::take(&mut self.utf8_pending);
        data.extend_from_slice(b);
        let mut start = 0;
        loop {
            match std::str::from_utf8(&data[start..]) {
                Ok(s) => {
                    let s = s.to_string();
                    self.feed(&s);
                    break;
                }
                Err(e) => {
                    let good = start + e.valid_up_to();
                    let s = String::from_utf8_lossy(&data[start..good]).into_owned();
                    self.feed(&s);
                    match e.error_len() {
                        Some(n) => {
                            self.feed("\u{FFFD}");
                            start = good + n;
                        }
                        None => {
                            self.utf8_pending = data[good..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bot, cols) = (self.top, self.bot, self.cols);
        for _ in 0..n.min(self.rows) {
            let b = self.buf();
            b.remove(top);
            b.insert(bot, vec![' '; cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bot, cols) = (self.top, self.bot, self.cols);
        for _ in 0..n.min(self.rows) {
            let b = self.buf();
            b.remove(bot);
            b.insert(top, vec![' '; cols]);
        }
    }

    fn lf(&mut self) {
        if self.y == self.bot {
            self.scroll_up(1);
        } else if self.y < self.rows - 1 {
            self.y += 1;
        }
    }

    fn clamp(&mut self) {
        self.x = self.x.min(self.cols - 1);
        self.y = self.y.min(self.rows - 1);
    }

    fn put(&mut self, c: char) {
        let w = cwidth(c);
        if w == 0 {
            return;
        }
        if self.x + w > self.cols {
            self.x = 0;
            self.lf();
        }
        let (x, y, cols) = (self.x, self.y, self.cols);
        let b = self.buf();
        b[y][x] = c;
        if w == 2 && x + 1 < cols {
            b[y][x + 1] = '\0';
        }
        self.x += w; // x == cols means "pending wrap"
    }

    fn push_seq(&mut self, c: char) {
        if self.seq.len() < SEQ_MAX {
            self.seq.push(c);
        }
    }

    pub fn feed(&mut self, data: &str) {
        for c in data.chars() {
            match self.st {
                St::Ground => match c {
                    '\x1b' => {
                        self.st = St::Esc;
                        self.seq.clear();
                    }
                    '\r' => self.x = 0,
                    '\n' | '\x0b' | '\x0c' => self.lf(),
                    '\x08' => self.x = self.x.saturating_sub(1),
                    '\t' => self.x = ((self.x / 8 + 1) * 8).min(self.cols - 1),
                    c if (c as u32) < 32 || c == '\x7f' => {}
                    c => self.put(c),
                },
                St::Esc => {
                    self.st = St::Ground;
                    match c {
                        '[' => {
                            self.st = St::Csi;
                            self.seq.clear();
                        }
                        ']' => {
                            self.st = St::Osc;
                            self.seq.clear();
                        }
                        'P' | '_' | '^' | 'X' => {
                            self.st = St::Str;
                            self.seq.clear();
                        }
                        '(' | ')' | '*' | '+' | '#' | '%' => self.st = St::Charset,
                        '7' => self.saved = (self.x, self.y),
                        '8' => {
                            (self.x, self.y) = self.saved;
                            self.clamp();
                        }
                        'M' => {
                            if self.y == self.top {
                                self.scroll_down(1);
                            } else {
                                self.y = self.y.saturating_sub(1);
                            }
                        }
                        'D' => self.lf(),
                        'E' => {
                            self.x = 0;
                            self.lf();
                        }
                        'c' => {
                            let (c, r) = (self.cols, self.rows);
                            *self.buf() = blank(c, r);
                            self.x = 0;
                            self.y = 0;
                        }
                        _ => {}
                    }
                }
                St::Charset => self.st = St::Ground,
                St::Osc => match c {
                    '\x07' => {
                        let s = std::mem::take(&mut self.seq);
                        self.osc(&s);
                        self.st = St::Ground;
                    }
                    '\x1b' => self.st = St::OscEsc,
                    c => self.push_seq(c),
                },
                St::OscEsc => {
                    let s = std::mem::take(&mut self.seq);
                    self.osc(&s);
                    self.st = St::Ground;
                }
                St::Str => match c {
                    '\x1b' => self.st = St::StrEsc,
                    '\x07' => self.st = St::Ground,
                    c => self.push_seq(c),
                },
                St::StrEsc => {
                    if self.seq.starts_with("+q") || self.seq.starts_with("$q") {
                        self.queries.push(format!("DCS{}", self.seq));
                    }
                    self.st = St::Ground;
                }
                St::Csi => {
                    if ('\x40'..='\x7e').contains(&c) {
                        let s = std::mem::take(&mut self.seq);
                        self.csi(&s, c);
                        self.st = St::Ground;
                    } else {
                        self.push_seq(c);
                    }
                }
            }
        }
    }

    fn osc(&mut self, s: &str) {
        if let Some(t) = s.strip_prefix("0;").or_else(|| s.strip_prefix("2;")) {
            self.title = t.to_string();
            self.title_seq += 1;
        } else if s.ends_with(";?") {
            self.queries.push(format!("OSC{s}"));
        }
    }

    fn csi(&mut self, p: &str, f: char) {
        let mut p = p;
        let mut privc = "";
        if let Some(c) = p.chars().next() {
            if "?>=<".contains(c) {
                privc = &p[..1];
                p = &p[1..];
            }
        }
        let mut inter = String::new();
        while let Some(c) = p.chars().last() {
            if " $\"'!".contains(c) {
                inter.insert(0, c);
                p = &p[..p.len() - 1];
            } else {
                break;
            }
        }
        let args: Vec<usize> = if p.is_empty() {
            vec![]
        } else {
            p.split(';')
                .map(|a| a.split(':').next().unwrap_or("").parse().unwrap_or(0))
                .collect()
        };
        let arg = |i: usize| args.get(i).copied().unwrap_or(0);
        let a1 = if arg(0) == 0 { 1 } else { arg(0) };

        if privc == "?" && (f == 'h' || f == 'l') {
            for &m in &args {
                if m == 1049 || m == 47 || m == 1047 {
                    if f == 'h' {
                        if m == 1049 {
                            self.saved = (self.x, self.y);
                        }
                        self.alt = blank(self.cols, self.rows);
                        self.in_alt = true;
                    } else {
                        self.in_alt = false;
                        if m == 1049 {
                            (self.x, self.y) = self.saved;
                        }
                    }
                }
                if f == 'h' {
                    // Bounded: a flood of distinct modes must not grow it.
                    if self.modes.len() < MODES_MAX {
                        self.modes.insert(m as u32);
                    }
                } else {
                    self.modes.remove(&(m as u32));
                }
            }
            self.clamp();
            return;
        }
        let is_query = ("?>=".contains(privc) && !privc.is_empty() && "cqnu".contains(f))
            || (privc.is_empty() && f == 'c')
            || (f == 'n' && (args.first() == Some(&5) || args.first() == Some(&6)))
            || (privc.is_empty() && f == 't' && matches!(args.first(), Some(14 | 16 | 18)));
        if is_query {
            self.queries.push(format!("CSI{privc}{p}{inter}{f}"));
            return;
        }
        if !privc.is_empty() || !inter.is_empty() {
            return;
        }
        let (cols, rows) = (self.cols, self.rows);
        match f {
            'H' | 'f' => {
                let r = if arg(0) == 0 { 1 } else { arg(0) };
                let c = if arg(1) == 0 { 1 } else { arg(1) };
                self.y = r - 1;
                self.x = c - 1;
            }
            'A' => {
                let floor = if self.y >= self.top { self.top } else { 0 };
                self.y = self.y.saturating_sub(a1).max(floor);
            }
            'B' => {
                let ceil = if self.y <= self.bot {
                    self.bot
                } else {
                    rows - 1
                };
                self.y = self.y.saturating_add(a1).min(ceil);
            }
            'C' => self.x = self.x.saturating_add(a1),
            'D' => self.x = self.x.min(cols - 1).saturating_sub(a1),
            'E' => {
                self.y = self.y.saturating_add(a1);
                self.x = 0;
            }
            'F' => {
                self.y = self.y.saturating_sub(a1);
                self.x = 0;
            }
            'G' | '`' => self.x = a1 - 1,
            'd' => self.y = a1 - 1,
            'J' => {
                let (x, y) = (self.x.min(cols - 1), self.y);
                let b = self.buf();
                match arg(0) {
                    0 => {
                        for i in x..cols {
                            b[y][i] = ' ';
                        }
                        for r in (y + 1)..rows {
                            b[r] = vec![' '; cols];
                        }
                    }
                    1 => {
                        for r in 0..y {
                            b[r] = vec![' '; cols];
                        }
                        for i in 0..=x {
                            b[y][i] = ' ';
                        }
                    }
                    _ => {
                        for r in 0..rows {
                            b[r] = vec![' '; cols];
                        }
                    }
                }
            }
            'K' => {
                let (x, y) = (self.x.min(cols - 1), self.y);
                let row = &mut self.buf()[y];
                match arg(0) {
                    0 => (x..cols).for_each(|i| row[i] = ' '),
                    1 => (0..=x).for_each(|i| row[i] = ' '),
                    _ => *row = vec![' '; cols],
                }
            }
            'X' => {
                let (x, y) = (self.x.min(cols - 1), self.y);
                let row = &mut self.buf()[y];
                for i in x..cols.min(x.saturating_add(a1)) {
                    row[i] = ' ';
                }
            }
            '@' => {
                let (x, y) = (self.x.min(cols - 1), self.y);
                let row = &mut self.buf()[y];
                for _ in 0..a1.min(cols - x) {
                    row.insert(x, ' ');
                    row.pop();
                }
            }
            'P' => {
                let (x, y) = (self.x.min(cols - 1), self.y);
                let row = &mut self.buf()[y];
                for _ in 0..a1.min(cols - x) {
                    row.remove(x);
                    row.push(' ');
                }
            }
            'L' => {
                if self.top <= self.y && self.y <= self.bot {
                    let (y, bot) = (self.y, self.bot);
                    let b = self.buf();
                    for _ in 0..a1.min(bot - y + 1) {
                        b.remove(bot);
                        b.insert(y, vec![' '; cols]);
                    }
                }
            }
            'M' => {
                if self.top <= self.y && self.y <= self.bot {
                    let (y, bot) = (self.y, self.bot);
                    let b = self.buf();
                    for _ in 0..a1.min(bot - y + 1) {
                        b.remove(y);
                        b.insert(bot, vec![' '; cols]);
                    }
                }
            }
            'S' => self.scroll_up(a1),
            'T' => self.scroll_down(a1),
            'r' => {
                let t = if arg(0) == 0 { 1 } else { arg(0) };
                let b = if arg(1) == 0 { rows } else { arg(1) };
                self.top = t - 1;
                self.bot = b.min(rows) - 1;
                if self.top >= self.bot {
                    self.top = 0;
                    self.bot = rows - 1;
                }
                self.x = 0;
                self.y = 0;
            }
            's' => self.saved = (self.x, self.y),
            'u' => (self.x, self.y) = self.saved,
            _ => {}
        }
        self.clamp();
    }
}

fn blank(cols: usize, rows: usize) -> Vec<Vec<char>> {
    vec![vec![' '; cols]; rows]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_and_wraps() {
        let mut s = Screen::new(5, 3);
        s.feed("abcdefg");
        assert_eq!(s.text(), "abcde\nfg\n");
    }

    #[test]
    fn cursor_moves_and_erase() {
        let mut s = Screen::new(10, 3);
        s.feed("hello\x1b[2;3Hxy\x1b[1;1H\x1b[K");
        assert_eq!(s.text(), "\n  xy\n");
        s.feed("\x1b[2J");
        assert_eq!(s.text(), "\n\n");
    }

    #[test]
    fn osc0_title_bel_and_st() {
        let mut s = Screen::new(10, 2);
        s.feed("\x1b]0;\u{2733} Claude Code\x07");
        assert_eq!(s.title, "\u{2733} Claude Code");
        s.feed("\x1b]0;\u{25d0} working\x1b\\");
        assert_eq!(s.title, "\u{25d0} working");
        assert_eq!(s.title_seq, 2);
        assert!(s.queries.is_empty());
    }

    #[test]
    fn title_split_across_utf8_chunks() {
        // e2 9c b3 = U+2733; split the glyph across two reads.
        let mut s = Screen::new(10, 2);
        s.feed_bytes(b"\x1b]0;\xe2\x9c");
        s.feed_bytes(b"\xb3 Claude Code\x07");
        assert_eq!(s.title, "\u{2733} Claude Code");
    }

    #[test]
    fn alt_screen_switch() {
        let mut s = Screen::new(10, 2);
        s.feed("main");
        s.feed("\x1b[?1049h");
        assert!(s.in_alt);
        assert_eq!(s.text(), "\n");
        s.feed("alt");
        s.feed("\x1b[?1049l");
        assert!(!s.in_alt);
        assert_eq!(s.text(), "main\n");
    }

    #[test]
    fn collects_queries() {
        let mut s = Screen::new(10, 2);
        s.feed("\x1b[c\x1b[>0q\x1b[?u\x1b[6n\x1b]11;?\x07\x1b[16t");
        assert_eq!(
            s.queries,
            vec!["CSIc", "CSI>0q", "CSI?u", "CSI6n", "OSC11;?", "CSI16t"]
        );
    }

    #[test]
    fn scroll_region() {
        let mut s = Screen::new(4, 4);
        s.feed("a\r\nb\r\nc\r\nd");
        s.feed("\x1b[2;3r\x1b[3;1H\n");
        // Rows 2..3 scroll; row 1 and 4 stay.
        assert_eq!(s.text(), "a\nc\n\nd");
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let mut s = Screen::new(6, 1);
        s.feed("中文x");
        assert_eq!(s.text(), "中文x");
        assert_eq!(s.x, 5);
    }

    // Moved here from the spike's `detect.rs`: answering terminal queries is
    // the terminal's job, not the CC adapter's.
    #[test]
    fn query_replies() {
        assert_eq!(query_reply("CSI6n", (2, 4)).unwrap(), "\x1b[3;5R");
        assert_eq!(query_reply("CSIc", (0, 0)).unwrap(), "\x1b[?62;22c");
        assert!(query_reply("CSI>0q", (0, 0))
            .unwrap()
            .starts_with("\x1bP>|"));
        assert_eq!(query_reply("CSI?u", (0, 0)), None);
        assert_eq!(query_reply("CSI16t", (0, 0)), None);
    }

    #[test]
    fn resize_keeps_the_cursor_row_and_never_panics() {
        let mut s = Screen::new(6, 4);
        s.feed("a\r\nb\r\nc\r\nd");
        s.resize(3, 2);
        // The cursor was on the last row, so the top rows go.
        assert_eq!(s.text(), "c\nd");
        assert_eq!((s.y, s.x), (1, 1));
        s.resize(5, 3);
        assert_eq!(s.text(), "c\nd\n");
        s.resize(0, 0);
        assert_eq!((s.cols, s.rows), (1, 1));
        // Huge counts and sizes are clamped, not trusted.
        s.feed("\x1b[99999;99999H\x1b[99999L\x1b[99999M\x1b[99999@\x1b[99999S\x1b[99999Xz");
        let mut long = String::from("\x1b]0;");
        long.push_str(&"x".repeat(SEQ_MAX * 2));
        long.push('\x07');
        s.feed(&long);
        assert_eq!(s.title.len(), SEQ_MAX - 2);
    }

    #[test]
    fn private_modes_are_bounded() {
        let mut s = Screen::new(20, 5);
        let mut flood = String::new();
        for m in 2000..4000 {
            flood.push_str(&format!("\x1b[?{}h", m));
        }
        s.feed_bytes(flood.as_bytes());
        assert_eq!(s.modes.len(), MODES_MAX);
        // The alt screen still works past the cap.
        s.feed_bytes(b"\x1b[?1049h");
        assert!(s.in_alt);
        s.feed_bytes(b"\x1b[?2000l");
        assert_eq!(s.modes.len(), MODES_MAX - 1);
    }
}
