//! A small soft-wrapping text editor for the objective cards.
//! Positions are char indices into `text`; rows are visual (wrapped) lines.

pub struct Editor {
    text: Vec<char>,
    cursor: usize,
}

impl Editor {
    pub fn new(s: &str) -> Self {
        let text: Vec<char> = s.chars().collect();
        Editor { cursor: text.len(), text }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect::<String>().trim().to_owned()
    }

    pub fn is_blank(&self) -> bool {
        self.text.iter().all(|c| c.is_whitespace())
    }

    pub fn cursor_to_start(&mut self) {
        self.cursor = 0;
    }

    pub fn end_of_text(&mut self) {
        self.cursor = self.text.len();
    }

    /// Visual rows as (start, end) char ranges.
    pub fn rows(&self, width: usize) -> Vec<(usize, usize)> {
        wrap(&self.text, width)
    }

    pub fn row_text(&self, (s, e): (usize, usize)) -> String {
        self.text[s..e].iter().collect()
    }

    /// Index where the first logical line (the objective's title) ends.
    pub fn title_end(&self) -> usize {
        self.text.iter().position(|&c| c == '\n').unwrap_or(self.text.len())
    }

    pub fn cursor_rc(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        let row = rows.iter().rposition(|&(s, _)| s <= self.cursor).unwrap_or(0);
        (row, self.cursor - rows[row].0)
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }

    fn word_start(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        let n = self.text.len();
        while self.cursor < n && self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < n && !self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    pub fn delete_word(&mut self) {
        let i = self.word_start();
        self.text.drain(i..self.cursor);
        self.cursor = i;
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].iter().position(|&c| c == '\n').map_or(self.text.len(), |p| self.cursor + p)
    }

    /// ⌘⌫: delete back to the start of the line, or join with the previous line.
    pub fn delete_to_line_start(&mut self) {
        let ls = self.line_start();
        if ls == self.cursor {
            self.backspace();
        } else {
            self.text.drain(ls..self.cursor);
            self.cursor = ls;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Move up one visual row; false if already on the first row.
    pub fn up(&mut self, width: usize) -> bool {
        let rows = self.rows(width);
        let (row, col) = self.cursor_rc(width);
        if row == 0 {
            return false;
        }
        self.cursor = place(&rows, row - 1, col);
        true
    }

    /// Move down one visual row; false if already on the last row.
    pub fn down(&mut self, width: usize) -> bool {
        let rows = self.rows(width);
        let (row, col) = self.cursor_rc(width);
        if row + 1 >= rows.len() {
            return false;
        }
        self.cursor = place(&rows, row + 1, col);
        true
    }
}

/// Cursor position for `col` on `row`, staying on that row when it is soft-wrapped.
fn place(rows: &[(usize, usize)], row: usize, col: usize) -> usize {
    let (s, e) = rows[row];
    let soft = rows.get(row + 1).is_some_and(|&(ns, _)| ns == e);
    let max = if soft { e.saturating_sub(1).max(s) } else { e };
    (s + col).min(max)
}

fn wrap(t: &[char], width: usize) -> Vec<(usize, usize)> {
    let w = width.max(2);
    let mut out = Vec::new();
    let mut ls = 0;
    loop {
        let le = t[ls..].iter().position(|&c| c == '\n').map_or(t.len(), |p| ls + p);
        let mut s = ls;
        while le - s > w {
            // Break after the last space that fits, or hard-break a long word.
            let b = (s + 1..s + w).rev().find(|&i| t[i] == ' ').map_or(s + w, |i| i + 1);
            out.push((s, b));
            s = b;
        }
        out.push((s, le));
        if le == t.len() {
            return out;
        }
        ls = le + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_on_spaces_and_moves_between_rows() {
        let mut e = Editor::new("hello brave new world\nsecond");
        let rows = e.rows(11);
        let texts: Vec<String> = rows.iter().map(|&r| e.row_text(r)).collect();
        assert_eq!(texts, ["hello ", "brave new ", "world", "second"]);
        assert_eq!(e.cursor_rc(11), (3, 6));
        assert!(e.up(11));
        assert_eq!(e.cursor_rc(11), (2, 5));
        assert!(e.up(11));
        assert!(e.up(11));
        assert!(!e.up(11));
        e.end();
        e.delete_word();
        assert_eq!(e.text(), "hello brave new \nsecond");
        e.cursor = e.text.len();
        e.delete_to_line_start();
        assert_eq!(e.text(), "hello brave new");
    }

    #[test]
    fn hard_breaks_long_words() {
        let e = Editor::new("abcdefghij");
        assert_eq!(e.rows(4), [(0, 4), (4, 8), (8, 10)]);
    }
}
