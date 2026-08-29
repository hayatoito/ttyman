use std::sync::{Arc, Mutex};

/// In-memory virtual terminal emulator (VT100) model for screen state and scrollback history inspection.
#[derive(Clone)]
pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        let r = if rows > 0 { rows } else { 24 };
        let c = if cols > 0 { cols } else { 80 };
        Self {
            parser: Arc::new(Mutex::new(vt100::Parser::new(r, c, scrollback_len))),
        }
    }

    pub fn process(&self, data: &[u8]) {
        if let Ok(mut parser) = self.parser.lock() {
            parser.process(data);
        }
    }

    pub fn set_size(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn size(&self) -> (u16, u16) {
        if let Ok(parser) = self.parser.lock() {
            parser.screen().size()
        } else {
            (24, 80)
        }
    }

    pub fn read(&self, lines: Option<usize>, all: bool, with_color: bool) -> String {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };

        // Determine the total number of buffered scrollback lines by setting offset to MAX (clamped to actual length)
        parser.screen_mut().set_scrollback(usize::MAX);
        let total_scrollback = parser.screen().scrollback();
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
        .min(rows as usize);

        if with_color {
            for row_bytes in parser
                .screen()
                .rows_formatted(0, cols)
                .take(active_visible_rows)
            {
                all_lines.push(String::from_utf8_lossy(&row_bytes).to_string());
            }
        } else {
            for row in visible_plain.into_iter().take(active_visible_rows) {
                all_lines.push(row);
            }
        }

        // 3. Filter based on arguments
        let result_lines = if all {
            all_lines
        } else if let Some(n) = lines {
            let total = all_lines.len();
            let count = n.min(total);
            all_lines[total.saturating_sub(count)..].to_vec()
        } else {
            // Default: visible active screen
            let total = all_lines.len();
            let count = (rows as usize).min(total);
            all_lines[total.saturating_sub(count)..].to_vec()
        };

        // Trim trailing whitespace from each line
        let trimmed: Vec<String> = result_lines
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();

        parser.screen_mut().set_scrollback(0);
        trimmed.join("\n")
    }

    /// Returns the ANSI payload to cleanly repaint the terminal screen and restore cursor state.
    pub fn redraw_payload(&self) -> Vec<u8> {
        let parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
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
}
