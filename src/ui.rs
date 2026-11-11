use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

use crate::agent::{Agent, Kind, SPARK_LEN, State};
use crate::app::{App, BroadcastTarget, Modal, NEW_FIELDS, TextInput, ToastKind, View, tilde};
use crate::theme::{Theme, mix};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SIDEBAR_W: u16 = 36;

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn frame_no(app: &App) -> usize {
    (app.started.elapsed().as_millis() / 90) as usize
}

pub fn state_color(t: &Theme, s: State) -> Color {
    match s {
        State::Starting => t.cyan,
        State::Idle => t.dim,
        State::Working => t.yellow,
        State::Blocked => t.red,
        State::Done => t.green,
        State::Exited => t.muted,
    }
}

fn glyph(app: &App, s: State) -> (&'static str, Style) {
    let t = &app.theme;
    let f = frame_no(app);
    match s {
        State::Starting => (SPINNER[f % 10], Style::new().fg(t.cyan)),
        State::Working => (SPINNER[f % 10], Style::new().fg(t.yellow)),
        State::Blocked => {
            let on = (f / 6) % 2 == 0;
            ("▲", Style::new().fg(if on { t.red } else { mix(t.red, t.surface, 0.55) }).add_modifier(Modifier::BOLD))
        }
        State::Done => ("✓", Style::new().fg(t.green).add_modifier(Modifier::BOLD)),
        State::Idle => ("○", Style::new().fg(t.dim)),
        State::Exited => ("■", Style::new().fg(t.muted)),
    }
}

fn elapsed(a: &Agent) -> Duration {
    match (a.state, a.work_started) {
        (State::Working | State::Blocked, Some(t)) => t.elapsed(),
        _ => a.state_since.elapsed(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let t = app.theme.clone();
    app.hits = Default::default();

    let [top, body, bottom] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let show_sidebar = app.sidebar && body.width >= 70;
    let (side, main) = if show_sidebar {
        let [s, m] = Layout::horizontal([Constraint::Length(SIDEBAR_W), Constraint::Min(20)]).areas(body);
        (Some(s), m)
    } else {
        (None, body)
    };

    top_bar(f, app, top, &t);
    if let Some(s) = side {
        sidebar(f, app, s, &t);
    }
    let main = Rect { x: main.x + 1, width: main.width.saturating_sub(2), ..main };
    let cursor = if app.agents.is_empty() {
        empty_state(f, app, main, &t);
        None
    } else {
        match app.view {
            View::Focus => focus_view(f, app, main, &t),
            View::Grid => grid_view(f, app, main, &t),
        }
    };
    bottom_bar(f, app, bottom, &t);

    if app.modal.is_some() {
        backdrop(f.buffer_mut(), area, &t);
        modal(f, app, area, &t);
    } else if let Some((x, y)) = cursor {
        f.set_cursor_position((x, y));
    }
}

// ───────────────────────────── chrome ─────────────────────────────

fn top_bar(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    f.buffer_mut().set_style(area, Style::new().bg(t.surface));
    let count = |s: State| app.agents.iter().filter(|a| a.state == s).count();
    let mut spans = vec![
        Span::styled(" ◆ ", Style::new().fg(t.accent)),
        Span::styled("trix", Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)),
        Span::styled("   ", Style::new()),
    ];
    let pills = [
        (State::Blocked, "needs you"),
        (State::Working, "working"),
        (State::Done, "done"),
        (State::Idle, "idle"),
    ];
    let mut any = false;
    for (s, label) in pills {
        let n = count(s) + if s == State::Working { count(State::Starting) } else { 0 };
        if n == 0 {
            continue;
        }
        any = true;
        let (g, gs) = glyph(app, s);
        let c = state_color(t, s);
        let bold = if s == State::Blocked { Modifier::BOLD } else { Modifier::empty() };
        spans.push(Span::styled(format!("{g} "), gs));
        spans.push(Span::styled(format!("{n} {label}"), Style::new().fg(c).add_modifier(bold)));
        spans.push(Span::raw("   "));
    }
    if !any {
        spans.push(Span::styled("no agents yet", Style::new().fg(t.dim)));
    }
    f.render_widget(Paragraph::new(TLine::from(spans)), area);

    let now = clock();
    let view = match app.view {
        View::Focus => "focus",
        View::Grid => "grid",
    };
    let right = TLine::from(vec![
        Span::styled(view, Style::new().fg(t.dim)),
        Span::styled("  ·  ", Style::new().fg(t.muted)),
        Span::styled(now, Style::new().fg(t.fg)),
        Span::raw(" "),
    ]);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn clock() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

fn bottom_bar(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let key = |k: &'static str| Span::styled(format!(" {k} "), Style::new().fg(t.fg_bright).bg(t.surface_hi));
    let lab = |l: &'static str| Span::styled(format!(" {l}   "), Style::new().fg(t.dim));
    let mut spans = vec![Span::raw(" ")];
    let hints: &[(&str, &str)] = if app.modal.is_some() {
        &[("esc", "cancel")]
    } else if app.agents.is_empty() {
        &[("alt+n", "new agent"), ("alt+?", "keys"), ("alt+q", "quit")]
    } else {
        &[
            ("alt+n", "new"),
            ("alt+a", "attention"),
            ("alt+1-9", "jump"),
            ("alt+g", "grid"),
            ("alt+b", "broadcast"),
            ("alt+?", "keys"),
        ]
    };
    for (k, l) in hints {
        spans.push(key(k));
        spans.push(lab(l));
    }
    f.render_widget(Paragraph::new(TLine::from(spans)), area);

    if let Some(toast) = &app.toast {
        let (icon, c) = match toast.kind {
            ToastKind::Good => ("✓", t.green),
            ToastKind::Bad => ("✕", t.red),
            ToastKind::Info => ("•", t.accent),
        };
        let fade = toast.at.elapsed() > Duration::from_millis(3200);
        let c = if fade { mix(c, t.bg, 0.5) } else { c };
        let line = TLine::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(c).add_modifier(Modifier::BOLD)),
            Span::styled(toast.text.clone(), Style::new().fg(if fade { t.dim } else { t.fg_bright })),
            Span::raw(" "),
        ]);
        let w = line.width() as u16 + 1;
        let r = Rect { x: area.right().saturating_sub(w), width: w.min(area.width), ..area };
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(line), r);
    }
}

fn sidebar(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) {
    f.buffer_mut().set_style(area, Style::new().bg(t.surface));
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(1) };
    if inner.height < 4 {
        return;
    }
    let header = TLine::from(vec![
        Span::styled(" AGENTS", Style::new().fg(t.dim).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", app.agents.len()), Style::new().fg(t.muted)),
    ]);
    f.render_widget(Paragraph::new(header), Rect { height: 1, ..inner });

    let list = Rect { y: inner.y + 2, height: inner.height.saturating_sub(5), ..inner };
    let n = app.agents.len() as u16;
    let compact = n * 4 > list.height;
    let per = if compact { 1 } else { 4 };
    let mut y = list.y;
    for i in 0..app.agents.len() {
        if y + per > list.bottom() + if compact { 0 } else { 1 } {
            break;
        }
        let r = Rect { x: list.x, y, width: list.width, height: if compact { 1 } else { 3 } };
        card(f, app, i, r, compact, t);
        app.hits.cards.push((r, app.agents[i].id));
        y += per;
    }

    // "new agent" button pinned to the bottom of the rail.
    let by = area.bottom().saturating_sub(2);
    let btn = Rect { x: inner.x, y: by, width: inner.width, height: 1 };
    let line = TLine::from(vec![
        Span::styled(" + ", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::styled("new agent", Style::new().fg(t.fg)),
    ]);
    f.render_widget(Paragraph::new(line), btn);
    f.render_widget(
        Paragraph::new(Span::styled("alt+n ", Style::new().fg(t.muted))).alignment(Alignment::Right),
        btn,
    );
    app.hits.new_button = Some(btn);
}

fn card(f: &mut Frame, app: &App, i: usize, r: Rect, compact: bool, t: &Theme) {
    let a = &app.agents[i];
    let selected = i == app.focus;
    let sc = state_color(t, a.state);
    let bg = if selected { t.surface_hi } else { t.surface };
    f.buffer_mut().set_style(r, Style::new().bg(bg));

    let bar_color = if a.state == State::Blocked {
        Some(t.red)
    } else if selected {
        Some(t.accent)
    } else if a.unseen {
        Some(sc)
    } else {
        None
    };
    if let Some(c) = bar_color {
        for y in r.y..r.bottom() {
            if let Some(cell) = f.buffer_mut().cell_mut((r.x, y)) {
                cell.set_symbol("▌").set_fg(c);
            }
        }
    }

    let (g, gs) = glyph(app, a.state);
    let idx = if i < 9 { format!("{}", i + 1) } else { " ".into() };
    let name_style = if selected {
        Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)
    } else if a.state == State::Exited {
        Style::new().fg(t.dim)
    } else {
        Style::new().fg(t.fg)
    };
    let time = fmt_dur(elapsed(a));
    let w = r.width as usize;
    let name_max = w.saturating_sub(7 + time.len() + 2);
    let mut line1 = vec![
        Span::raw("  "),
        Span::styled(idx, Style::new().fg(if selected { t.accent } else { t.muted })),
        Span::raw("  "),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(trunc(&a.name, name_max), name_style),
    ];
    if a.unseen && a.state != State::Blocked {
        line1.push(Span::styled(" •", Style::new().fg(sc).add_modifier(Modifier::BOLD)));
    }
    let row1 = Rect { height: 1, ..r };
    f.render_widget(Paragraph::new(TLine::from(line1)), row1);
    let tstyle = if matches!(a.state, State::Working | State::Blocked) { Style::new().fg(mix(sc, t.dim, 0.3)) } else { Style::new().fg(t.muted) };
    f.render_widget(Paragraph::new(Span::styled(format!("{time} "), tstyle)).alignment(Alignment::Right), row1);
    if compact {
        return;
    }

    // Row 2: what it is doing right now.
    let act_style = match a.state {
        State::Blocked => Style::new().fg(t.red),
        State::Working => Style::new().fg(t.fg),
        State::Done => Style::new().fg(mix(t.green, t.fg, 0.4)),
        _ => Style::new().fg(t.dim),
    };
    let line2 = TLine::from(vec![Span::raw("       "), Span::styled(trunc(&a.activity, w.saturating_sub(8)), act_style)]);
    f.render_widget(Paragraph::new(line2), Rect { y: r.y + 1, height: 1, ..r });

    // Row 3: kind · output sparkline · git.
    let kind = a.kind.label();
    let spark_w = 10usize.min(w.saturating_sub(16));
    let mut spans = vec![
        Span::raw("       "),
        Span::styled(format!("{kind:<7}"), Style::new().fg(mix(kind_color(t, a.kind), bg, 0.25))),
    ];
    spans.extend(sparkline(a, spark_w, if a.state == State::Idle || a.state == State::Exited { t.dim } else { sc }, mix(t.muted, bg, 0.4)));
    let used = 7 + 7 + spark_w;
    let mut git = String::new();
    let mut diff = None;
    if let Some((files, add, del)) = a.diff.filter(|d| d.0 > 0) {
        let _ = files;
        diff = Some((format!(" +{add}"), format!(" −{del}")));
    }
    let diff_w = diff.as_ref().map(|(a, d)| a.width() + d.width()).unwrap_or(0);
    if let Some(b) = &a.branch {
        git = format!("  {}", trunc(b, w.saturating_sub(used + diff_w + 3)));
    }
    spans.push(Span::styled(git, Style::new().fg(t.dim)));
    if let Some((ad, de)) = diff {
        spans.push(Span::styled(ad, Style::new().fg(mix(t.green, t.dim, 0.2))));
        spans.push(Span::styled(de, Style::new().fg(mix(t.red, t.dim, 0.2))));
    }
    f.render_widget(Paragraph::new(TLine::from(spans)), Rect { y: r.y + 2, height: 1, ..r });
}

fn kind_color(t: &Theme, k: Kind) -> Color {
    match k {
        Kind::Claude => t.orange,
        Kind::Codex => t.cyan,
        Kind::Shell => t.dim,
        Kind::Custom => t.magenta,
    }
}

fn sparkline(a: &Agent, width: usize, color: Color, base: Color) -> Vec<Span<'static>> {
    const BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let buckets = SPARK_LEN / width.max(1);
    let data: Vec<u32> = a.spark.iter().copied().collect();
    let start = data.len().saturating_sub(width * buckets);
    let mut out = Vec::new();
    for c in 0..width {
        let s = start + c * buckets;
        let v: u32 = data[s..(s + buckets).min(data.len())].iter().sum();
        if v == 0 {
            out.push(Span::styled("▁", Style::new().fg(base)));
        } else {
            let lvl = (((v as f32 + 1.0).log2() * 1.4) as usize).clamp(1, 7);
            out.push(Span::styled(BARS[lvl], Style::new().fg(color)));
        }
    }
    out
}

fn trunc(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 2 > max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

// ───────────────────────────── panes ─────────────────────────────

fn pane_block<'a>(app: &App, a: &Agent, focused: bool, compact: bool, t: &Theme) -> Block<'a> {
    let sc = state_color(t, a.state);
    let border = if a.state == State::Blocked {
        t.red
    } else if focused && app.view == View::Grid {
        t.accent
    } else {
        mix(t.muted, sc, if a.state == State::Working { 0.35 } else { 0.0 })
    };
    let (g, gs) = glyph(app, a.state);
    let mut left = vec![
        Span::styled("─ ", Style::new().fg(border)),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(a.name.clone(), Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(a.kind.label(), Style::new().fg(kind_color(t, a.kind))),
        Span::raw(" "),
    ];
    if !compact && !a.activity.is_empty() {
        left.push(Span::styled("· ", Style::new().fg(t.muted)));
        left.push(Span::styled(
            trunc(&a.activity, 60),
            Style::new().fg(if a.state == State::Blocked { t.red } else { t.dim }),
        ));
        left.push(Span::raw(" "));
    }
    let mut right = vec![];
    if !compact {
        right.push(Span::styled(format!(" {}", tilde(&a.cwd)), Style::new().fg(t.dim)));
    }
    if let Some(b) = &a.branch {
        right.push(Span::styled(format!("  {b}"), Style::new().fg(t.magenta)));
    }
    if let Some((files, add, del)) = a.diff {
        if files > 0 {
            right.push(Span::styled(format!(" +{add}"), Style::new().fg(t.green)));
            right.push(Span::styled(format!(" −{del}"), Style::new().fg(t.red)));
        }
    }
    right.push(Span::raw(" "));
    let status = TLine::from(vec![
        Span::styled(format!(" {} ", a.state.label()), Style::new().fg(sc)),
        Span::styled(format!("{} ", fmt_dur(elapsed(a))), Style::new().fg(t.dim)),
    ]);
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .padding(Padding::horizontal(1))
        .title_top(TLine::from(left))
        .title_top(TLine::from(right).right_aligned())
        .title_bottom(status.right_aligned())
}

fn focus_view(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) -> Option<(u16, u16)> {
    let area = attention_banner(f, app, area, t);
    let block = pane_block(app, &app.agents[app.focus], true, false, t);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.pane_size = (inner.width, inner.height);
    // Keep every agent sized to the main pane so switching is instant.
    for a in app.agents.iter_mut() {
        a.resize(inner.width, inner.height);
    }
    let a = &app.agents[app.focus];
    app.hits.panes.push((inner, a.id));
    let cur = render_term(f.buffer_mut(), inner, a, t, app.term_focused);
    pane_overlays(f, a, area, inner, t);
    cur
}

/// A slim strip above the pane when someone *else* is waiting on the human.
fn attention_banner(f: &mut Frame, app: &App, area: Rect, t: &Theme) -> Rect {
    let waiting: Vec<(usize, &Agent)> = app
        .agents
        .iter()
        .enumerate()
        .filter(|(i, a)| *i != app.focus && a.state == State::Blocked)
        .collect();
    let Some(&(i, first)) = waiting.iter().min_by_key(|(_, a)| a.state_since) else {
        return area;
    };
    if area.height < 8 {
        return area;
    }
    let r = Rect { height: 1, ..area };
    let bg = mix(t.red, t.bg, 0.82);
    f.buffer_mut().set_style(r, Style::new().bg(bg));
    let (g, gs) = glyph(app, State::Blocked);
    let more = if waiting.len() > 1 { format!("  +{} more", waiting.len() - 1) } else { String::new() };
    let line = TLine::from(vec![
        Span::raw(" "),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(first.name.clone(), Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" ({}) ", i + 1), Style::new().fg(t.dim)),
        Span::styled(trunc(&first.activity, area.width.saturating_sub(40) as usize), Style::new().fg(t.red)),
        Span::styled(more, Style::new().fg(t.dim)),
    ]);
    f.render_widget(Paragraph::new(line), r);
    let hint = TLine::from(vec![Span::styled(" alt+a ", Style::new().fg(t.bg).bg(t.red).add_modifier(Modifier::BOLD)), Span::styled(" jump ", Style::new().fg(t.fg))]);
    f.render_widget(Paragraph::new(hint).alignment(Alignment::Right), r);
    Rect { y: area.y + 1, height: area.height - 1, ..area }
}

fn grid_view(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) -> Option<(u16, u16)> {
    let n = app.agents.len();
    let wide = area.width as f32 / (area.height as f32 * 2.2);
    let cols = ((n as f32 * wide).sqrt().ceil() as usize).clamp(1, n);
    let rows = n.div_ceil(cols);
    let row_rects = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    let mut cursor = None;
    let mut idx = 0;
    for (ri, rr) in row_rects.iter().enumerate() {
        let in_row = if ri == rows - 1 { n - idx } else { cols };
        let cells = Layout::horizontal(vec![Constraint::Ratio(1, in_row as u32); in_row]).split(*rr);
        for c in cells.iter() {
            let i = idx;
            idx += 1;
            let focused = i == app.focus;
            let block = pane_block(app, &app.agents[i], focused, c.width < 70, t);
            let inner = block.inner(*c);
            f.render_widget(block, *c);
            app.agents[i].resize(inner.width, inner.height);
            let a = &app.agents[i];
            app.hits.panes.push((inner, a.id));
            let cur = render_term(f.buffer_mut(), inner, a, t, focused && app.term_focused);
            if focused {
                cursor = cur;
            } else {
                // Unfocused tiles recede a little.
                fade(f.buffer_mut(), inner, t, 0.35);
            }
            pane_overlays(f, a, *c, inner, t);
        }
    }
    cursor
}

fn pane_overlays(f: &mut Frame, a: &Agent, outer: Rect, inner: Rect, t: &Theme) {
    let offset = a.term.lock().grid().display_offset();
    if offset > 0 {
        let s = TLine::from(vec![Span::styled(
            format!(" ↑ {offset} lines · shift+pgdn "),
            Style::new().fg(t.bg).bg(t.accent),
        )]);
        let r = Rect { x: outer.x + 2, y: outer.bottom() - 1, width: (s.width() as u16).min(outer.width.saturating_sub(4)), height: 1 };
        f.render_widget(Paragraph::new(s), r);
    }
    if a.state == State::Exited && inner.height > 2 {
        let r = Rect { y: inner.bottom() - 1, height: 1, ..inner };
        f.buffer_mut().set_style(r, Style::new().bg(t.surface_hi));
        let code = a.exit_code.map(|c| format!(" · code {c}")).unwrap_or_default();
        let line = TLine::from(vec![
            Span::styled(" ■ ", Style::new().fg(t.muted)),
            Span::styled(format!("process exited{code}"), Style::new().fg(t.fg)),
            Span::styled("    enter", Style::new().fg(t.accent)),
            Span::styled(" restart", Style::new().fg(t.dim)),
            Span::styled("   alt+w", Style::new().fg(t.accent)),
            Span::styled(" close", Style::new().fg(t.dim)),
        ]);
        f.render_widget(Paragraph::new(line), r);
    }
}

fn map_color(c: AColor) -> (Color, bool) {
    match c {
        AColor::Spec(rgb) => (Color::Rgb(rgb.r, rgb.g, rgb.b), false),
        AColor::Indexed(i) if i < 16 => (ansi(i), false),
        AColor::Indexed(i) => (Color::Indexed(i), false),
        AColor::Named(n) => {
            let v = n as usize;
            if v < 16 {
                (ansi(v as u8), false)
            } else {
                match n {
                    NamedColor::DimBlack => (ansi(0), true),
                    NamedColor::DimRed => (ansi(1), true),
                    NamedColor::DimGreen => (ansi(2), true),
                    NamedColor::DimYellow => (ansi(3), true),
                    NamedColor::DimBlue => (ansi(4), true),
                    NamedColor::DimMagenta => (ansi(5), true),
                    NamedColor::DimCyan => (ansi(6), true),
                    NamedColor::DimWhite => (ansi(7), true),
                    NamedColor::DimForeground => (Color::Reset, true),
                    _ => (Color::Reset, false),
                }
            }
        }
    }
}

fn ansi(i: u8) -> Color {
    match i {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Blit the emulator grid into the ratatui buffer. Returns the cursor cell
/// when the child wants a visible cursor.
fn render_term(buf: &mut Buffer, area: Rect, a: &Agent, t: &Theme, focused: bool) -> Option<(u16, u16)> {
    let term = a.term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let sel = content.selection;
    let mode = content.mode;
    let cursor = content.cursor.point;
    let mut sym = String::with_capacity(8);
    for ind in content.display_iter {
        let row = ind.point.line.0 + offset;
        let col = ind.point.column.0;
        if row < 0 || row >= area.height as i32 || col >= area.width as usize {
            continue;
        }
        let cell = ind.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let (fg, dim_fg) = map_color(cell.fg);
        let (bg, _) = map_color(cell.bg);
        let mut m = Modifier::empty();
        let fl = cell.flags;
        if fl.contains(Flags::BOLD) {
            m |= Modifier::BOLD;
        }
        if fl.contains(Flags::DIM) || dim_fg {
            m |= Modifier::DIM;
        }
        if fl.contains(Flags::ITALIC) {
            m |= Modifier::ITALIC;
        }
        if fl.intersects(Flags::ALL_UNDERLINES) {
            m |= Modifier::UNDERLINED;
        }
        if fl.contains(Flags::STRIKEOUT) {
            m |= Modifier::CROSSED_OUT;
        }
        if fl.contains(Flags::INVERSE) {
            m |= Modifier::REVERSED;
        }
        let mut style = Style::new().fg(fg).bg(bg).add_modifier(m);
        if sel.is_some_and(|s| s.contains(ind.point)) {
            style = style.bg(t.selection).fg(t.fg_bright).remove_modifier(Modifier::REVERSED);
        }
        sym.clear();
        if fl.contains(Flags::HIDDEN) || cell.c == '\0' || cell.c == '\t' {
            sym.push(' ');
        } else {
            sym.push(cell.c);
            if let Some(zw) = cell.zerowidth() {
                sym.extend(zw.iter());
            }
        }
        if let Some(bc) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) {
            bc.set_symbol(&sym).set_style(style);
        }
    }
    let _ = Point::new(Line(0), Column(0));
    if focused && offset == 0 && mode.contains(TermMode::SHOW_CURSOR) {
        let (x, y) = (cursor.column.0 as u16, cursor.line.0);
        if y >= 0 && (y as u16) < area.height && x < area.width {
            return Some((area.x + x, area.y + y as u16));
        }
    }
    None
}

fn fade(buf: &mut Buffer, area: Rect, t: &Theme, amount: f32) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                let fg = match c.fg {
                    Color::Rgb(..) => mix(c.fg, t.bg, amount),
                    _ => {
                        c.modifier |= Modifier::DIM;
                        c.fg
                    }
                };
                c.set_fg(fg);
            }
        }
    }
}

fn backdrop(buf: &mut Buffer, area: Rect, t: &Theme) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_fg(t.muted);
                c.modifier = Modifier::empty();
                if let Color::Rgb(..) = c.bg {
                    c.set_bg(mix(c.bg, t.bg, 0.5));
                }
            }
        }
    }
}

// ───────────────────────────── empty state ─────────────────────────────

const LOGO: [&str; 3] = ["▀█▀ █▀█ █ ▀▄▀", " █  █▀▄ █ ▄▀▄", ""];

fn empty_state(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) {
    let h = 13u16;
    let y0 = area.y + area.height.saturating_sub(h) / 2;
    let w = LOGO[0].chars().count() as u16;
    let x0 = area.x + area.width.saturating_sub(w) / 2;
    let buf = f.buffer_mut();
    let shimmer = (app.started.elapsed().as_millis() as f32 / 1800.0).sin() * 0.5 + 0.5;
    for (ry, line) in LOGO.iter().enumerate() {
        for (cx, ch) in line.chars().enumerate() {
            let tpos = cx as f32 / (w as f32 - 1.0);
            let c = mix(t.accent, t.magenta, (tpos * 0.8 + shimmer * 0.2).clamp(0.0, 1.0));
            if let Some(cell) = buf.cell_mut((x0 + cx as u16, y0 + ry as u16)) {
                cell.set_char(ch).set_fg(c);
            }
        }
    }
    let center = |f: &mut Frame, y: u16, line: TLine| {
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), Rect { x: area.x, y, width: area.width, height: 1 });
    };
    center(f, y0 + 3, TLine::from(Span::styled("a workspace where agents live", Style::new().fg(t.dim))));
    let row = |k: &'static str, l: &'static str| {
        TLine::from(vec![
            Span::styled(format!(" {k:<7}"), Style::new().fg(t.accent)),
            Span::styled(format!("{l:<22}"), Style::new().fg(t.fg)),
        ])
    };
    center(f, y0 + 6, row("enter", "spawn your first agent"));
    center(f, y0 + 7, row("alt+n", "new agent anytime"));
    center(f, y0 + 8, row("alt+a", "jump to who needs you"));
    center(f, y0 + 9, row("alt+?", "every shortcut"));
    let theme_note = if app.themed_by_omarchy { "following your omarchy theme" } else { "tokyo night" };
    center(f, y0 + 12, TLine::from(Span::styled(theme_note, Style::new().fg(t.muted))));
}

// ───────────────────────────── modals ─────────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 3, width: w, height: h }
}

fn modal_block<'a>(title: &'a str, t: &Theme) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.accent))
        .style(Style::new().bg(t.surface).fg(t.fg))
        .title(TLine::from(vec![
            Span::styled(" ✦ ", Style::new().fg(t.accent)),
            Span::styled(format!("{title} "), Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)),
        ]))
}

fn input_spans(inp: &TextInput, active: bool, placeholder: &str, t: &Theme, max: usize) -> (Vec<Span<'static>>, Option<u16>) {
    if inp.text.is_empty() {
        let mut v = vec![Span::styled(placeholder.to_string(), Style::new().fg(t.muted).add_modifier(Modifier::ITALIC))];
        if active {
            v.insert(0, Span::raw(""));
        }
        return (v, active.then_some(0));
    }
    // Keep the cursor visible in long inputs by scrolling the window.
    let chars: Vec<char> = inp.text.chars().collect();
    let start = inp.cursor.saturating_sub(max.saturating_sub(1));
    let shown: String = chars[start..].iter().take(max).collect();
    let pre: String = chars[start..inp.cursor].iter().collect();
    (
        vec![Span::styled(shown, Style::new().fg(if active { t.fg_bright } else { t.fg }))],
        active.then_some(pre.width() as u16),
    )
}

fn modal(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let Some(m) = &app.modal else { return };
    match m {
        Modal::New(form) => {
            let r = centered(area, 72, 16);
            f.render_widget(Clear, r);
            let block = modal_block("New agent", t);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: inner.height.saturating_sub(1) };
            let label_w = 11u16;
            let field_w = inner.width.saturating_sub(label_w) as usize;
            let mut cursor = None;
            for (i, name) in NEW_FIELDS.iter().enumerate() {
                let y = inner.y + i as u16 * 2;
                let active = form.field == i;
                let lab = Span::styled(
                    format!("{}{:<9}", if active { "› " } else { "  " }, name),
                    Style::new().fg(if active { t.accent } else { t.dim }).add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }),
                );
                let (spans, cur): (Vec<Span>, Option<u16>) = match i {
                    0 => {
                        let mut v = Vec::new();
                        for (k, kind) in Kind::ALL.iter().enumerate() {
                            if k == form.kind {
                                v.push(Span::styled(
                                    format!(" {} ", kind.label()),
                                    Style::new().fg(t.bg).bg(if active { t.accent } else { kind_color(t, *kind) }).add_modifier(Modifier::BOLD),
                                ));
                            } else {
                                v.push(Span::styled(format!(" {} ", kind.label()), Style::new().fg(t.dim)));
                            }
                            v.push(Span::raw(" "));
                        }
                        (v, None)
                    }
                    1 => input_spans(&form.dir, active, "~/path/to/project", t, field_w),
                    2 => input_spans(&form.name, active, "agent name", t, field_w),
                    3 => input_spans(&form.prompt, active, "optional first message", t, field_w),
                    4 => {
                        let (dot, txt) = if form.worktree { ("●", "isolated git worktree on ") } else { ("○", "off — work in the folder directly") };
                        let mut v = vec![
                            Span::styled(format!("{dot} "), Style::new().fg(if form.worktree { t.green } else { t.dim })),
                            Span::styled(txt, Style::new().fg(if form.worktree { t.fg } else { t.dim })),
                        ];
                        if form.worktree {
                            v.push(Span::styled(format!("trix/{}", crate::app::slug(&form.name.text)), Style::new().fg(t.magenta)));
                        }
                        (v, None)
                    }
                    _ => {
                        let ph = if Kind::ALL[form.kind] == Kind::Custom { "command to run, e.g. aider --model …" } else { "extra flags, e.g. --model opus" };
                        input_spans(&form.args, active, ph, t, field_w)
                    }
                };
                let mut line = vec![lab];
                if !matches!(i, 0) {
                    line.push(Span::raw(" "));
                }
                line.extend(spans);
                f.render_widget(Paragraph::new(TLine::from(line)), Rect { y, height: 1, ..inner });
                if let Some(cx) = cur {
                    cursor = Some((inner.x + label_w + 1 + cx, y));
                }
            }
            let foot_y = r.bottom().saturating_sub(2);
            let foot = if let Some(e) = &form.error {
                TLine::from(Span::styled(format!("✕ {e}"), Style::new().fg(t.red)))
            } else {
                TLine::from(vec![
                    Span::styled("tab", Style::new().fg(t.fg)),
                    Span::styled(" next   ", Style::new().fg(t.dim)),
                    Span::styled("←→", Style::new().fg(t.fg)),
                    Span::styled(" agent   ", Style::new().fg(t.dim)),
                    Span::styled("space", Style::new().fg(t.fg)),
                    Span::styled(" toggle   ", Style::new().fg(t.dim)),
                    Span::styled("enter", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
                    Span::styled(" launch ", Style::new().fg(t.dim)),
                ])
            };
            f.render_widget(Paragraph::new(foot).alignment(Alignment::Right), Rect { x: r.x + 2, y: foot_y, width: r.width.saturating_sub(4), height: 1 });
            if let Some(c) = cursor {
                f.set_cursor_position(c);
            }
        }
        Modal::Broadcast(inp, target) => {
            let r = centered(area, 72, 9);
            f.render_widget(Clear, r);
            let block = modal_block("Broadcast", t);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let inner = Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: inner.height };
            let n = app.agents.iter().filter(|a| crate::app::broadcast_reaches(a, *target)).count();
            let held = app.agents.iter().filter(|a| a.kind.is_agent() && matches!(a.state, State::Blocked | State::Starting)).count();
            let pill = |on: bool, s: &'static str| {
                if on {
                    Span::styled(format!(" {s} "), Style::new().fg(t.bg).bg(t.accent).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(format!(" {s} "), Style::new().fg(t.dim))
                }
            };
            f.render_widget(
                Paragraph::new(TLine::from(vec![
                    Span::styled("To  ", Style::new().fg(t.dim)),
                    pill(*target == BroadcastTarget::All, "every agent"),
                    Span::raw(" "),
                    pill(*target == BroadcastTarget::Waiting, "idle & done only"),
                    Span::styled(format!("   → {n} agent{}", if n == 1 { "" } else { "s" }), Style::new().fg(t.fg)),
                ])),
                Rect { height: 1, ..inner },
            );
            if held > 0 {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!("skipping {held} on a prompt — answer {} first", if held == 1 { "it" } else { "them" }),
                        Style::new().fg(t.dim),
                    )),
                    Rect { y: inner.y + 4, height: 1, ..inner },
                );
            }
            let (spans, cur) = input_spans(inp, true, "message to send…", t, inner.width as usize - 2);
            let mut line = vec![Span::styled("› ", Style::new().fg(t.accent))];
            line.extend(spans);
            f.render_widget(Paragraph::new(TLine::from(line)), Rect { y: inner.y + 2, height: 1, ..inner });
            if let Some(cx) = cur {
                f.set_cursor_position((inner.x + 2 + cx, inner.y + 2));
            }
            f.render_widget(
                Paragraph::new(TLine::from(vec![
                    Span::styled("tab", Style::new().fg(t.fg)),
                    Span::styled(" target   ", Style::new().fg(t.dim)),
                    Span::styled("enter", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
                    Span::styled(" send", Style::new().fg(t.dim)),
                ]))
                .alignment(Alignment::Right),
                Rect { y: r.bottom() - 2, height: 1, ..inner },
            );
        }
        Modal::Rename(_, inp) => {
            let r = centered(area, 50, 5);
            f.render_widget(Clear, r);
            let block = modal_block("Rename", t);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let row = Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: 1 };
            let (spans, cur) = input_spans(inp, true, "name", t, row.width as usize - 2);
            let mut line = vec![Span::styled("› ", Style::new().fg(t.accent))];
            line.extend(spans);
            f.render_widget(Paragraph::new(TLine::from(line)), row);
            if let Some(cx) = cur {
                f.set_cursor_position((row.x + 2 + cx, row.y));
            }
        }
        Modal::ConfirmClose(id) => {
            let name = app.agents.iter().find(|a| a.id == *id).map(|a| a.name.clone()).unwrap_or_default();
            confirm(f, area, t, "Close agent", &format!("Stop {name} and close its pane?"));
        }
        Modal::ConfirmQuit => {
            let live = app.agents.iter().filter(|a| a.state != State::Exited).count();
            confirm(f, area, t, "Quit trix", &format!("{live} agent{} still running and will be stopped. alt+d detaches instead.", if live == 1 { " is" } else { "s are" }));
        }
        Modal::Help => help(f, area, t),
        Modal::Restore => {
            let summary = app.pending_restore.as_ref().map(|p| p.summary()).unwrap_or_default();
            restore_prompt(f, area, t, &summary);
        }
    }
}

fn restore_prompt(f: &mut Frame, area: Rect, t: &Theme, text: &str) {
    let r = centered(area, (text.width() as u16 + 8).max(52), 7);
    f.render_widget(Clear, r);
    let block = modal_block("Pick up where you left off?", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(
        Paragraph::new(Span::styled(text.to_string(), Style::new().fg(t.fg))).alignment(Alignment::Center),
        Rect { y: inner.y + 1, height: 1, ..inner },
    );
    f.render_widget(
        Paragraph::new(TLine::from(vec![
            Span::styled(" y ", Style::new().fg(t.bg).bg(t.green).add_modifier(Modifier::BOLD)),
            Span::styled(" restore     ", Style::new().fg(t.fg)),
            Span::styled(" n ", Style::new().fg(t.fg_bright).bg(t.surface_hi)),
            Span::styled(" start fresh     ", Style::new().fg(t.fg)),
            Span::styled(" esc ", Style::new().fg(t.fg_bright).bg(t.surface_hi)),
            Span::styled(" later", Style::new().fg(t.fg)),
        ]))
        .alignment(Alignment::Center),
        Rect { y: inner.y + 3, height: 1, ..inner },
    );
}

fn confirm(f: &mut Frame, area: Rect, t: &Theme, title: &str, text: &str) {
    let r = centered(area, (text.width() as u16 + 8).max(44), 7);
    f.render_widget(Clear, r);
    let block = modal_block(title, t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(
        Paragraph::new(Span::styled(text.to_string(), Style::new().fg(t.fg))).alignment(Alignment::Center),
        Rect { y: inner.y + 1, height: 1, ..inner },
    );
    f.render_widget(
        Paragraph::new(TLine::from(vec![
            Span::styled(" y ", Style::new().fg(t.bg).bg(t.red).add_modifier(Modifier::BOLD)),
            Span::styled(" yes     ", Style::new().fg(t.fg)),
            Span::styled(" esc ", Style::new().fg(t.fg_bright).bg(t.surface_hi)),
            Span::styled(" keep", Style::new().fg(t.fg)),
        ]))
        .alignment(Alignment::Center),
        Rect { y: inner.y + 3, height: 1, ..inner },
    );
}

fn help(f: &mut Frame, area: Rect, t: &Theme) {
    let r = centered(area, 78, 19);
    f.render_widget(Clear, r);
    let block = modal_block("Keys", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let left: &[(&str, &str)] = &[
        ("", "AGENTS"),
        ("alt+n", "new agent"),
        ("alt+1…9", "jump to agent"),
        ("alt+j / k", "next / previous"),
        ("alt+a", "next one that needs you"),
        ("alt+b", "broadcast a prompt"),
        ("alt+r", "rename"),
        ("alt+w", "close"),
        ("enter", "restart an exited agent"),
    ];
    let right: &[(&str, &str)] = &[
        ("", "VIEW"),
        ("alt+g", "grid ⇄ focus"),
        ("alt+s", "toggle sidebar"),
        ("shift+pgup", "scroll back"),
        ("wheel", "scroll · click focus"),
        ("drag", "select & copy"),
        ("shift+enter", "newline to agent"),
        ("alt+d", "detach · keeps running"),
        ("alt+q", "quit · stops agents"),
    ];
    let cols = Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).split(Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: inner.height.saturating_sub(1) });
    for (col, items) in cols.iter().zip([left, right]) {
        for (i, (k, d)) in items.iter().enumerate() {
            let line = if k.is_empty() {
                TLine::from(Span::styled(*d, Style::new().fg(t.dim).add_modifier(Modifier::BOLD)))
            } else {
                TLine::from(vec![Span::styled(format!("{k:<13}"), Style::new().fg(t.accent)), Span::styled(*d, Style::new().fg(t.fg))])
            };
            f.render_widget(Paragraph::new(line), Rect { y: col.y + i as u16 + if i > 0 { 1 } else { 0 }, height: 1, ..*col });
        }
    }
    let legend = TLine::from(vec![
        Span::styled("⠹ ", Style::new().fg(t.yellow)),
        Span::styled("working   ", Style::new().fg(t.dim)),
        Span::styled("▲ ", Style::new().fg(t.red)),
        Span::styled("needs you   ", Style::new().fg(t.dim)),
        Span::styled("✓ ", Style::new().fg(t.green)),
        Span::styled("done   ", Style::new().fg(t.dim)),
        Span::styled("○ ", Style::new().fg(t.dim)),
        Span::styled("idle   ", Style::new().fg(t.dim)),
        Span::styled("• ", Style::new().fg(t.green)),
        Span::styled("unseen", Style::new().fg(t.dim)),
    ]);
    f.render_widget(Paragraph::new(legend).alignment(Alignment::Center), Rect { y: r.bottom() - 2, height: 1, x: r.x, width: r.width });
    let _ = Instant::now();
}
