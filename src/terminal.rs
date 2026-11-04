use std::sync::{Arc, Mutex};

const CLEAR_SCREEN: &[u8] = b"\x1b[2J";

/// In-memory virtual terminal emulator (VT100) model for screen state and scrollback history inspection.
#[derive(Clone)]
pub struct Terminal {
    state: Arc<Mutex<State>>,
    scrollback_len: usize,
}

struct State {
    parser: vt100::Parser,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        let r = if rows > 0 { rows } else { 24 };
        let c = if cols > 0 { cols } else { 80 };
        let parser = vt100::Parser::new(r, c, scrollback_len);
        Self {
            state: Arc::new(Mutex::new(State { parser })),
            scrollback_len,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        }
    }

    pub fn process(&self, data: &[u8]) {
        self.lock().parser.process(data);
    }

    pub fn set_size(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        self.lock().parser.screen_mut().set_size(rows, cols);
    }

    pub fn size(&self) -> (u16, u16) {
        self.lock().parser.screen().size()
    }

    /// True while a full-screen program (vim, less, top) has switched to the alternate screen.
    pub fn alternate_screen(&self) -> bool {
        self.lock().parser.screen().alternate_screen()
    }

    pub fn read(&self, lines: Option<usize>, all: bool, with_color: bool) -> String {
        let mut state = self.lock();
        let parser = &mut state.parser;
        let rows = parser.screen().size().0 as usize;
        let all_lines = collect_lines(parser, with_color);

        // Filter based on arguments
        let result_lines = if all {
            all_lines
        } else if let Some(n) = lines {
            let total = all_lines.len();
            let count = n.min(total);
            all_lines[total.saturating_sub(count)..].to_vec()
        } else {
            // Default: visible active screen
            let total = all_lines.len();
            let count = rows.min(total);
            all_lines[total.saturating_sub(count)..].to_vec()
        };

        // Trim trailing whitespace from each line
        let trimmed: Vec<String> = result_lines
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        trimmed.join("\n")
    }

    /// Returns the ANSI payload to cleanly repaint the terminal screen and restore cursor state.
    pub fn redraw_payload(&self) -> Vec<u8> {
        let state = self.lock();
        let parser = &state.parser;
        let mut buf = Vec::new();
        // 1. Reset color attributes and clear entire screen
        buf.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");

        // 2. Render screen buffer contents with formatting
        buf.extend_from_slice(&parser.screen().contents_formatted());

        // 3. Move cursor to actual position and set visibility
        let (row, col) = parser.screen().cursor_position();
        let cursor_seq = format!("\x1b[{};{}H", row + 1, col + 1);
        buf.extend_from_slice(cursor_seq.as_bytes());

        if parser.screen().hide_cursor() {
            buf.extend_from_slice(b"\x1b[?25l");
        } else {
            buf.extend_from_slice(b"\x1b[?25h");
        }

        buf
    }
}

/// The text a [`Terminal`] shows after a starting point: the cursor when created or at the previous
/// [`NewText::take`]. It is a second emulator that starts as a copy of the screen, so the shell's
/// line editor (redraws, autosuggestions) renders exactly as on the screen.
pub struct NewText {
    terminal: Terminal,
    parser: vt100::Parser,
    /// The starting point: a row index into [`collect_lines`] and a column.
    start_row: usize,
    start_col: usize,
    /// Lines taken out before the screen was cleared.
    earlier: Vec<String>,
    /// Some output was not seen, or lines were dropped from the scrollback.
    truncated: bool,
}

impl NewText {
    pub fn new(terminal: &Terminal) -> Self {
        let screen = terminal.lock().parser.screen().clone();
        Self::seeded(terminal.clone(), &screen)
    }

    fn seeded(terminal: Terminal, screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        let mut parser = vt100::Parser::new(rows, cols, terminal.scrollback_len);
        parser.process(&screen.state_formatted());
        let (row, col) = parser.screen().cursor_position();
        Self {
            terminal,
            parser,
            start_row: row as usize,
            start_col: col as usize,
            earlier: Vec::new(),
            truncated: false,
        }
    }

    /// Feeds output after it was written to the terminal.
    pub fn push(&mut self, output: &[u8]) {
        let size = self.terminal.size();
        if self.parser.screen().size() != size {
            self.parser.screen_mut().set_size(size.0, size.1);
        }
        // A clear of the main screen (`clear`, Ctrl-L) erases the rows after the starting point:
        // take the lines so far, and start again at the top of the cleared screen.
        let clear_at = output
            .windows(CLEAR_SCREEN.len())
            .rposition(|w| w == CLEAR_SCREEN);
        let (before, after) = output.split_at(clear_at.unwrap_or(output.len()));
        self.parser.process(before);
        if !after.is_empty() && !self.parser.screen().alternate_screen() {
            let lines = self.lines(false);
            self.earlier.extend(lines);
            self.start_row = scrollback_len(&mut self.parser);
            self.start_col = 0;
        }
        self.parser.process(after);
    }

    /// Records that some output was missed.
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// True if a line was completed since the starting point: the cursor moved to a later line
    /// other than by a long line wrapping. A line editor that steps down and back up (to clear
    /// below an autosuggestion) does not count.
    pub fn has_new_line(&mut self) -> bool {
        let scrolled = scrollback_len(&mut self.parser);
        if !self.earlier.is_empty() || self.scrollback_full(scrolled) {
            return true;
        }
        let screen = self.parser.screen();
        let cursor = scrolled + screen.cursor_position().0 as usize;
        (self.start_row..cursor)
            .any(|row| row < scrolled || !screen.row_wrapped((row - scrolled) as u16))
    }

    /// The non-empty lines since the starting point as plain text, then starts over at the cursor.
    /// The first line leaves out what was left of the starting point (the prompt), and the text
    /// ends at the cursor without a newline, so consecutive texts join as on the screen (`% ` and
    /// the command typed after it). Output that repaints in place (progress bars) keeps only its
    /// final state. At most the scrollback length of newest lines are kept, after an
    /// `[... earlier lines omitted]` marker.
    pub fn take(&mut self) -> String {
        let mut lines = std::mem::take(&mut self.earlier);
        lines.extend(self.lines(true));
        if self.truncated {
            lines.insert(0, "[... earlier lines omitted]".to_string());
        }
        let screen = self.parser.screen().clone();
        *self = Self::seeded(self.terminal.clone(), &screen);
        lines.join("\n")
    }

    /// True if the scrollback (holding `scrolled` rows) is full: rows were dropped from its top,
    /// so `start_row` no longer points at the starting point.
    fn scrollback_full(&self, scrolled: usize) -> bool {
        self.terminal.scrollback_len > 0 && scrolled == self.terminal.scrollback_len
    }

    /// The non-empty lines after the starting point. With `to_cursor`, the last line keeps its
    /// spaces up to the cursor (after a prompt `% `).
    fn lines(&mut self, to_cursor: bool) -> Vec<String> {
        let scrolled = scrollback_len(&mut self.parser);
        let (start, skip) = if self.scrollback_full(scrolled) {
            self.truncated = true;
            (0, 0)
        } else {
            (self.start_row, self.start_col)
        };
        let rows = collect_lines(&mut self.parser, false);
        let (cursor_row, cursor_col) = self.parser.screen().cursor_position();
        // The last row is the cursor row unless rows with text follow it.
        let ends_at_cursor = to_cursor && rows.len() == scrolled + cursor_row as usize + 1;
        let mut lines = Vec::new();
        for (i, row) in rows.iter().enumerate().skip(start) {
            let from = if i == start { skip } else { 0 };
            let mut line = row
                .chars()
                .skip(from)
                .collect::<String>()
                .trim_end()
                .to_string();
            if ends_at_cursor && i + 1 == rows.len() {
                let width = from + line.chars().count();
                line.extend(std::iter::repeat_n(
                    ' ',
                    (cursor_col as usize).saturating_sub(width),
                ));
                // The cursor line ends the text even when empty (the cursor is at the start of a
                // new line), so the text ends with a newline and the next text starts on that line.
                if i != start {
                    lines.push(line);
                    continue;
                }
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }
}

/// The number of rows in the scrollback. Setting the view offset past the end clamps it to that
/// number; the offset is reset to 0 (the live screen) afterwards.
fn scrollback_len(parser: &mut vt100::Parser) -> usize {
    parser.screen_mut().set_scrollback(usize::MAX);
    let len = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(0);
    len
}

/// All rendered lines: the scrollback (oldest first), then the visible rows up to the last row
/// with content or the cursor row, whichever is lower.
fn collect_lines(parser: &mut vt100::Parser, with_color: bool) -> Vec<String> {
    let total_scrollback = scrollback_len(parser);
    let (rows, cols) = parser.screen().size();
    let r_size = rows as usize;

    // 1. Collect scrollback rows in page chunks (from oldest to newest)
    let mut all_lines: Vec<String> = Vec::with_capacity(total_scrollback + r_size);
    let mut remaining = total_scrollback;
    while remaining > 0 && r_size > 0 {
        let chunk_size = remaining.min(r_size);
        parser.screen_mut().set_scrollback(remaining);
        if with_color {
            for row_bytes in parser.screen().rows_formatted(0, cols).take(chunk_size) {
                all_lines.push(String::from_utf8_lossy(&row_bytes).to_string());
            }
        } else {
            for row in parser.screen().rows(0, cols).take(chunk_size) {
                all_lines.push(row);
            }
        }
        remaining -= chunk_size;
    }

    // 2. Collect visible screen rows and determine the last active row
    parser.screen_mut().set_scrollback(0);
    let (cursor_row, _) = parser.screen().cursor_position();

    let visible_plain: Vec<String> = parser.screen().rows(0, cols).collect();
    let last_content_row = visible_plain.iter().rposition(|r| !r.trim().is_empty());
    let active_visible_rows = match last_content_row {
        Some(idx) => (idx + 1).max((cursor_row as usize) + 1),
        None => (cursor_row as usize) + 1,
    }
    .min(r_size);

    if with_color {
        for row_bytes in parser
            .screen()
            .rows_formatted(0, cols)
            .take(active_visible_rows)
        {
            all_lines.push(String::from_utf8_lossy(&row_bytes).to_string());
        }
    } else {
        all_lines.extend(visible_plain.into_iter().take(active_visible_rows));
    }
    all_lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_read_basic() {
        let term = Terminal::new(24, 80, 1000);
        term.process(b"Hello, world!\r\nSecond line\r\n");

        let text = term.read(None, false, false);
        assert!(text.contains("Hello, world!"));
        assert!(text.contains("Second line"));
    }

    #[test]
    fn test_terminal_read_lines() {
        let term = Terminal::new(5, 80, 1000);
        for i in 1..=10 {
            term.process(format!("Line {i}\r\n").as_bytes());
        }

        let visible = term.read(None, false, false);
        assert!(visible.contains("Line 10"));

        let last_4 = term.read(Some(4), false, false);
        assert!(last_4.contains("Line 10"));

        let all = term.read(None, true, false);
        assert!(all.contains("Line 1\n"), "All output:\n{all}");
        assert!(all.contains("Line 10"));

        let term_large = Terminal::new(5, 80, 1000);
        for i in 1..=100 {
            term_large.process(format!("Line {i}\r\n").as_bytes());
        }
        let all_large = term_large.read(None, true, false);
        assert!(all_large.contains("Line 1\n"));
        assert!(all_large.contains("Line 50\n"));
        assert!(all_large.contains("Line 100"));
        assert_eq!(all_large.lines().count(), 100);
    }

    #[test]
    fn test_terminal_redraw_payload() {
        let term = Terminal::new(24, 80, 1000);
        term.process(b"Hello, redraw!\r\n");
        let payload = term.redraw_payload();
        assert!(!payload.is_empty());
        let s = String::from_utf8_lossy(&payload);
        assert!(s.contains("\x1b[2J\x1b[H"));
        assert!(s.contains("Hello, redraw!"));
    }

    #[test]
    fn test_terminal_read_trim_trailing_blank_lines() {
        let term = Terminal::new(84, 100, 1000);
        term.process(b"hayato@host:~$ ");

        let text = term.read(Some(10), false, false);
        assert_eq!(text, "hayato@host:~$");

        let visible = term.read(None, false, false);
        assert_eq!(visible, "hayato@host:~$");
    }

    /// A terminal showing `screen`, and a [`NewText`] that starts after it.
    fn new_text_after(screen: &[u8]) -> (Terminal, NewText) {
        let term = Terminal::new(10, 40, 1000);
        term.process(screen);
        let text = NewText::new(&term);
        (term, text)
    }

    #[test]
    fn test_new_text_starts_after_the_prompt_and_ends_at_the_cursor() {
        let (_, mut text) = new_text_after(b"old\r\n% ");
        text.push(b"ls\r\na\r\nb\r\n% ");
        assert_eq!(text.take(), "ls\na\nb\n% ");
        // Nothing new.
        assert_eq!(text.take(), "");
        text.push(b"echo hi\r\nhi\r\n% ");
        assert_eq!(text.take(), "echo hi\nhi\n% ");
    }

    #[test]
    fn test_new_text_renders_line_editor_redraws_like_the_screen() {
        // A line editor shows a grey suggestion, then redraws the whole line from the prompt
        // with only what was typed.
        let (_, mut text) = new_text_after(b"% ");
        text.push(b"l\x1b[90mong suggestion\x1b[0m\r% ls\x1b[K\r\na\r\n% ");
        assert_eq!(text.take(), "ls\na\n% ");
    }

    #[test]
    fn test_new_text_ending_at_the_start_of_a_line_ends_with_a_newline() {
        let (_, mut text) = new_text_after(b"% ");
        text.push(b"loop\r\nWed\r\n0\r\n");
        let first = text.take();
        assert_eq!(first, "loop\nWed\n0\n");
        text.push(b"Wed\r\n1\r\n");
        let second = text.take();
        assert_eq!(format!("{first}{second}"), "loop\nWed\n0\nWed\n1\n");
    }

    #[test]
    fn test_new_text_has_new_line() {
        let (_, mut text) = new_text_after(b"% ");
        // An autosuggestion: zsh steps down a line to clear below it, and back up.
        text.push(b"a\x1b[90mdb shell\x1b[39m\r\r\n\x1b[K\x1b[A\x1b[3C");
        assert!(!text.has_new_line());
        // A command line long enough to wrap.
        text.push(&[b'x'; 60]);
        assert!(!text.has_new_line());
        text.push(b"\r\n");
        assert!(text.has_new_line());
    }

    #[test]
    fn test_new_text_keeps_final_state_of_repaints() {
        let (_, mut text) = new_text_after(b"$ ");
        text.push(b"run\r\n10%\r50%\r100%\r\n$ ");
        assert_eq!(text.take(), "run\n100%\n$ ");
    }

    #[test]
    fn test_new_text_after_screen_clear() {
        let (_, mut text) = new_text_after(b"old\r\nolder\r\n% ");
        text.push(b"clear; seq 3\r\n\x1b[H\x1b[2J\x1b[3J1\r\n2\r\n3\r\n% ");
        assert_eq!(text.take(), "clear; seq 3\n1\n2\n3\n% ");
    }

    #[test]
    fn test_new_text_ignores_clear_on_alternate_screen() {
        let (_, mut text) = new_text_after(b"% ");
        text.push(b"vi\r\n\x1b[?1049h\x1b[2Jediting\x1b[?1049l% ");
        assert_eq!(text.take(), "vi\n% ");
    }

    #[test]
    fn test_new_text_marks_omitted_lines() {
        let term = Terminal::new(4, 40, 5);
        term.process(b"$ ");
        let mut text = NewText::new(&term);
        for i in 1..=20 {
            text.push(format!("{i}\r\n").as_bytes());
        }
        let taken = text.take();
        let lines: Vec<&str> = taken.lines().collect();
        assert_eq!(lines[0], "[... earlier lines omitted]");
        assert_eq!(lines.last(), Some(&"20"));
    }
}
