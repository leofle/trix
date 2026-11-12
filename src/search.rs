//! Search across every agent's scrollback.
//!
//! Rows that the terminal soft-wrapped are joined back into one logical line,
//! so a match that wrapped on screen is still found. One hit per matching
//! line, newest first. A query with no capitals ignores case; any capital
//! makes it exact ("smart case").

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Flags, LineLength};

/// Most hits kept per agent, so one chatty pane can't drown the rest.
pub const PER_AGENT: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub agent: u32,
    /// First and last cell of the match, in grid coordinates at search time.
    pub start: Point,
    pub end: Point,
    /// The whole logical line, and where in it (in chars) the match is.
    pub text: String,
    pub at: usize,
    pub len: usize,
    /// Lines between the match and the bottom of the screen.
    pub from_bottom: usize,
    /// Scrollback size at search time, to correct for output since.
    pub history: usize,
}

/// Smart case: does this query want an exact-case match?
fn exact(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

fn fold(c: char, exact: bool) -> char {
    if exact {
        c
    } else if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Where the already folded `needle` first occurs in the already folded
/// `hay`: the fast path for scanning the whole scrollback.
fn find_folded(hay: &[char], needle: &[char]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i] == first && hay[i + 1..i + needle.len()] == *rest)
}

/// Where `needle` first occurs in `hay`, in chars.
fn find(hay: &[char], needle: &[char], exact: bool) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].iter().zip(needle).all(|(&h, &n)| fold(h, exact) == fold(n, exact)))
}

/// One logical line: its characters and the cell each one sits in.
struct Logical {
    chars: Vec<char>,
    points: Vec<Point>,
}

impl Logical {
    fn text(&self) -> String {
        self.chars.iter().collect::<String>().trim_end().to_string()
    }
}

/// The logical line that ends on row `last`, walking up through the rows it
/// wrapped from. Returns it and its first row.
fn logical_ending_at<T>(term: &Term<T>, last: i32) -> (Logical, i32) {
    let grid = term.grid();
    let top = -(grid.history_size() as i32);
    let cols = grid.columns();
    let mut first = last;
    while first > top && grid[Line(first - 1)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
        first -= 1;
    }
    let mut l = Logical { chars: Vec::new(), points: Vec::new() };
    for row in first..=last {
        let r = &grid[Line(row)];
        for c in 0..cols {
            let cell = &r[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let ch = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };
            l.chars.push(ch);
            l.points.push(Point::new(Line(row), Column(c)));
        }
    }
    (l, first)
}

/// The last `n` logical lines of a terminal (scrollback and screen), oldest
/// first, without the blank rows below the last output.
pub fn tail<T>(term: &Term<T>, n: usize) -> Vec<String> {
    let grid = term.grid();
    let top = -(grid.history_size() as i32);
    let mut last = grid.screen_lines() as i32 - 1;
    let mut out = Vec::new();
    while last >= top && out.len() < n {
        let (line, first) = logical_ending_at(term, last);
        let text = line.text();
        if !(out.is_empty() && text.is_empty()) {
            out.push(text);
        }
        last = first - 1;
    }
    out.reverse();
    out
}

/// Every matching line in one terminal, newest first, at most `limit`.
pub fn search_term<T>(term: &Term<T>, agent: u32, query: &str, limit: usize) -> Vec<Hit> {
    let needle: Vec<char> = query.chars().collect();
    if needle.iter().all(|c| c.is_whitespace()) {
        return Vec::new();
    }
    let exact = exact(query);
    // Rows are scanned without their blank ends, so check them for the query
    // without its trailing blanks: never a miss, and the exact `find` on the
    // full line below still decides.
    let mut folded: Vec<char> = needle.iter().map(|&c| fold(c, exact)).collect();
    while folded.last().is_some_and(|c| c.is_whitespace()) {
        folded.pop();
    }
    let grid = term.grid();
    let history = grid.history_size();
    let bottom = grid.screen_lines() as i32 - 1;
    let top = -(history as i32);
    let cols = grid.columns();
    let mut hits = Vec::new();
    // Most lines don't match: check each one with a reused buffer of folded
    // chars, and build the full line (text and cell positions) only for hits.
    let mut hay: Vec<char> = Vec::with_capacity(cols);
    // Walk up one logical line at a time, from the bottom of the screen.
    let mut last = bottom;
    while last >= top && hits.len() < limit {
        let mut first = last;
        while first > top && grid[Line(first - 1)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            first -= 1;
        }
        hay.clear();
        for row in first..=last {
            let r = &grid[Line(row)];
            // The blank end of a row holds nothing `folded` can match.
            let len = if row < last { cols } else { r.line_length().0 };
            for c in 0..len {
                let cell = &r[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let ch = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };
                hay.push(fold(ch, exact));
            }
        }
        if find_folded(&hay, &folded).is_none() {
            last = first - 1;
            continue;
        }
        let (line, first) = logical_ending_at(term, last);
        if let Some(at) = find(&line.chars, &needle, exact) {
            let end = at + needle.len() - 1;
            hits.push(Hit {
                agent,
                start: line.points[at],
                end: line.points[end],
                text: line.text(),
                at,
                len: needle.len(),
                from_bottom: (bottom - line.points[end].line.0) as usize,
                history,
            });
        }
        last = first - 1;
    }
    hits
}

/// Where `hit` is now. Output since the search pushes lines up into the
/// scrollback; when the scrollback is full the oldest fall off instead, so
/// check the text and look for the same line nearby if it moved. `None` when
/// it's gone.
pub fn relocate<T>(term: &Term<T>, hit: &Hit, query: &str) -> Option<Hit> {
    let grid = term.grid();
    let history = grid.history_size();
    let shift = history as i32 - hit.history as i32;
    let moved = |p: Point| Point::new(Line(p.line.0 - shift), p.column);
    let guess = Hit { start: moved(hit.start), end: moved(hit.end), history, ..hit.clone() };
    let top = -(history as i32);
    let bottom = grid.screen_lines() as i32 - 1;
    // The scrollback can shrink too (cleared, or reflowed by a resize), which
    // moves the guess down, possibly past the last line.
    if guess.start.line.0 >= top && guess.end.line.0 <= bottom {
        let (line, _) = logical_ending_at(term, guess.end.line.0);
        if line.text() == hit.text && line.points.get(hit.at) == Some(&guess.start) {
            return Some(Hit { from_bottom: (bottom - guess.end.line.0) as usize, ..guess });
        }
    }
    search_term(term, hit.agent, query, usize::MAX)
        .into_iter()
        .filter(|h| h.text == hit.text)
        .min_by_key(|h| (h.start.line.0 - guess.start.line.0).abs())
}

/// The display offset that shows `line`: none when it's on the live screen,
/// otherwise enough to put it about a third of the way down.
pub fn offset_for(line: i32, screen_lines: usize, history: usize) -> usize {
    if line >= 0 {
        return 0;
    }
    let want = screen_lines as i32 / 3 - line;
    want.clamp(0, history as i32) as usize
}

/// A slice of `text` of at most `width` chars that shows the match, and the
/// match's position within it.
pub fn snippet(text: &str, at: usize, len: usize, width: usize) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return (text.to_string(), at);
    }
    // Some context before the match, but never cut the match itself.
    let lead = (width.saturating_sub(len) / 3).min(at);
    let start = (at - lead).min(chars.len().saturating_sub(width));
    let shown: String = chars[start..(start + width).min(chars.len())].iter().collect();
    (shown, at - start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(cols: usize, lines: usize, output: &str) -> Term<VoidListener> {
        let mut t = Term::new(Config { scrolling_history: 1000, ..Config::default() }, &Size(cols, lines), VoidListener);
        feed(&mut t, output);
        t
    }
    fn feed(t: &mut Term<VoidListener>, output: &str) {
        let mut p: Processor = Processor::new();
        p.advance(t, output.replace('\n', "\r\n").as_bytes());
    }

    #[test]
    fn tail_reads_back_through_scrollback() {
        let out: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let t = term(20, 5, &out);
        assert_eq!(tail(&t, 3), ["line 28", "line 29", "line 30"], "blank rows below the output are skipped");
        assert_eq!(tail(&t, 100).len(), 30);
        let t = term(10, 5, "0123456789abcdef\nend\n");
        assert_eq!(tail(&t, 2), ["0123456789abcdef", "end"], "wrapped rows come back as one line");
    }
    fn texts(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.text.as_str()).collect()
    }

    #[test]
    fn finds_lines_newest_first() {
        let t = term(40, 5, "error one\nfine\nERROR two\nok\nerror three\n");
        assert_eq!(texts(&search_term(&t, 1, "error", 10)), ["error three", "ERROR two", "error one"]);
    }

    #[test]
    fn smart_case() {
        let t = term(40, 5, "Error a\nerror b\n");
        assert_eq!(search_term(&t, 1, "error", 10).len(), 2);
        assert_eq!(texts(&search_term(&t, 1, "Error", 10)), ["Error a"]);
    }

    #[test]
    fn searches_the_scrollback_too() {
        let out: String = (0..300).map(|i| format!("line {i}\n")).collect();
        let t = term(20, 10, &out);
        assert!(t.grid().history_size() > 200);
        let h = &search_term(&t, 1, "line 7", 100);
        // "line 7", "line 70".."line 79" — newest first.
        assert_eq!(h.first().unwrap().text, "line 79");
        assert_eq!(h.last().unwrap().text, "line 7");
        assert!(h.last().unwrap().start.line.0 < 0, "found above the screen");
        assert_eq!(h.len(), 11);
    }

    #[test]
    fn one_hit_per_line_and_the_limit() {
        let t = term(40, 5, "aa aa aa\naa\naa\n");
        let h = search_term(&t, 1, "aa", 10);
        assert_eq!(h.len(), 3);
        assert_eq!(search_term(&t, 1, "aa", 2).len(), 2);
        assert!(search_term(&t, 1, "  ", 10).is_empty(), "blank queries find nothing");
        assert!(search_term(&t, 1, "", 10).is_empty());
    }

    #[test]
    fn matches_across_a_soft_wrap() {
        // 10 columns: "0123456789abcdef" wraps after the 9.
        let t = term(10, 5, "0123456789abcdef\n");
        let h = search_term(&t, 1, "89ab", 10);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text, "0123456789abcdef");
        assert_eq!((h[0].start.line.0, h[0].start.column.0), (0, 8));
        assert_eq!((h[0].end.line.0, h[0].end.column.0), (1, 1));
        assert_eq!(h[0].at, 8);
    }

    #[test]
    fn positions_with_wide_characters() {
        let t = term(20, 3, "日本 error\n");
        let h = search_term(&t, 1, "error", 10);
        assert_eq!(h[0].at, 3, "chars, not cells");
        assert_eq!(h[0].start.column.0, 5, "cells: each wide char takes two");
    }

    #[test]
    fn a_query_ending_in_a_blank_still_matches_at_the_end_of_a_line() {
        let t = term(20, 5, "an error\nerrors here\n");
        let hits = search_term(&t, 1, "error ", 10);
        assert_eq!(hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(), ["an error"]);
        assert_eq!(search_term(&t, 1, "ERROR", 10), vec![], "a capital means exact case");
        assert_eq!(search_term(&t, 1, "Error", 10).len(), 0);
        assert_eq!(search_term(&t, 1, "errors", 10)[0].text, "errors here");
    }

    #[test]
    fn relocate_after_the_scrollback_was_cleared() {
        let out: String = (0..50).map(|i| format!("x{i}\n")).collect();
        let mut t = term(20, 5, &out);
        let hit = search_term(&t, 1, "x40", 10).remove(0);
        assert!(hit.start.line.0 < 0, "found in the scrollback");
        feed(&mut t, "\x1b[3J");
        assert_eq!(t.grid().history_size(), 0);
        // Must not read past the screen; it's gone, so nothing to jump to.
        assert_eq!(relocate(&t, &hit, "x40"), None);
    }

    #[test]
    fn relocate_follows_new_output() {
        let mut t = term(20, 5, "target here\n");
        let hit = search_term(&t, 1, "target", 10).remove(0);
        let out: String = (0..50).map(|i| format!("more {i}\n")).collect();
        feed(&mut t, &out);
        let now = relocate(&t, &hit, "target").unwrap();
        assert_eq!(now.text, "target here");
        assert!(now.start.line.0 < hit.start.line.0, "it moved up into the scrollback");
        let (line, _) = logical_ending_at(&t, now.end.line.0);
        assert_eq!(line.text(), "target here");
    }

    #[test]
    fn relocate_when_the_scrollback_is_full() {
        let mut t = Term::new(Config { scrolling_history: 20, ..Config::default() }, &Size(20, 5), VoidListener);
        let out: String = (0..40).map(|i| format!("x{i}\n")).collect();
        feed(&mut t, &out);
        feed(&mut t, "needle\n");
        let hit = search_term(&t, 1, "needle", 10).remove(0);
        // History is full now, so the size stays put while lines move.
        feed(&mut t, "a\nb\nc\n");
        let now = relocate(&t, &hit, "needle").unwrap();
        let (line, _) = logical_ending_at(&t, now.end.line.0);
        assert_eq!(line.text(), "needle");
        // Pushed out entirely: gone.
        let out: String = (0..60).map(|i| format!("y{i}\n")).collect();
        feed(&mut t, &out);
        assert_eq!(relocate(&t, &hit, "needle"), None);
    }

    #[test]
    fn scroll_offset_puts_the_line_in_view() {
        assert_eq!(offset_for(5, 30, 1000), 0, "on the live screen: no scrolling");
        assert_eq!(offset_for(0, 30, 1000), 0);
        assert_eq!(offset_for(-1, 30, 1000), 11);
        assert_eq!(offset_for(-100, 30, 1000), 110);
        assert_eq!(offset_for(-1000, 30, 1000), 1000, "never past the top");
    }

    #[test]
    fn snippets_keep_the_match_in_view() {
        assert_eq!(snippet("short line", 6, 4, 40), ("short line".into(), 6));
        let long = format!("{}MATCH{}", "a".repeat(100), "b".repeat(100));
        let (s, at) = snippet(&long, 100, 5, 30);
        assert_eq!(s.chars().count(), 30);
        assert_eq!(&s[at..at + 5], "MATCH");
        // Near the end: the window slides back instead of running short.
        let (s, at) = snippet(&format!("{}END", "a".repeat(100)), 100, 3, 20);
        assert_eq!((s.chars().count(), &s[at..]), (20, "END"));
    }
}
