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

use crate::config::Action;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

use crate::agent::{Agent, Kind, SPARK_LEN, State};
use crate::app::{App, BroadcastTarget, MenuItem, Modal, NEW_FIELDS, Search, TextInput, ToastKind, View, tilde};
use crate::theme::{Theme, mix};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SIDEBAR_W: u16 = 36;
/// alt+m: number, state, name and the unseen dot.
const SIDEBAR_MINI_W: u16 = 18;

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
            let on = (f / 6).is_multiple_of(2);
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
    let side_w = if app.sidebar_mini { SIDEBAR_MINI_W } else { SIDEBAR_W };
    let show_sidebar = app.sidebar && body.width >= side_w + 34;
    let (side, main) = if show_sidebar {
        let [s, m] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(body);
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

    if let Some(Modal::Menu(m)) = app.modal {
        // A menu floats over the workspace without dimming it.
        agent_menu(f, app, m, area, &t);
    } else if app.modal.is_some() {
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
    ];
    if !crate::session::is_default() {
        spans.push(Span::styled(" · ", Style::new().fg(t.muted)));
        spans.push(Span::styled(crate::session::name(), Style::new().fg(t.accent).add_modifier(Modifier::BOLD)));
    }
    spans.push(Span::styled("   ", Style::new()));
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
    let mut right = Vec::new();
    if app.viewers > 0 {
        right.push(Span::styled("◉ ", Style::new().fg(t.accent)));
        right.push(Span::styled(format!("{} watching", app.viewers), Style::new().fg(t.fg)));
        right.push(Span::styled("  ·  ", Style::new().fg(t.muted)));
    }
    right.extend([
        Span::styled(view, Style::new().fg(t.dim)),
        Span::styled("  ·  ", Style::new().fg(t.muted)),
        Span::styled(now, Style::new().fg(t.fg)),
        Span::raw(" "),
    ]);
    let right = TLine::from(right);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

/// Every time shown on screen as text: the clock and each agent's time in
/// its state. Redrawing for the clock only matters when this changes, which
/// is once a minute once every timer is past an hour.
pub fn times_shown(app: &App) -> String {
    let mut s = clock();
    for a in &app.agents {
        s.push(' ');
        s.push_str(&fmt_dur(elapsed(a)));
    }
    s
}

fn clock() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

fn bottom_bar(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let key = |k: String| Span::styled(format!(" {k} "), Style::new().fg(t.fg_bright).bg(t.surface_hi));
    let lab = |l: &'static str| Span::styled(format!(" {l}   "), Style::new().fg(t.dim));
    let mut spans = vec![Span::raw(" ")];
    let hints: Vec<(String, &str)> = if app.modal.is_some() {
        vec![("esc".into(), "cancel")]
    } else {
        let shown: &[(Action, &str)] = if app.agents.is_empty() {
            &[(Action::New, "new agent"), (Action::Help, "keys"), (Action::Quit, "quit")]
        } else {
            &[
                (Action::New, "new"),
                (Action::Attention, "attention"),
                (Action::Jump, "jump"),
                (Action::Grid, "grid"),
                (Action::Broadcast, "broadcast"),
                (Action::Help, "keys"),
            ]
        };
        // An action with no key has nothing to hint at.
        shown.iter().filter_map(|&(a, l)| Some((app.config.keys.label(a)?, l))).collect()
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
    let n = app.agents.len();
    let mini = app.sidebar_mini;
    // Cards keep their size (alt+c makes them one line); what doesn't fit
    // scrolls, with the focused card always in view.
    let compact = mini || app.sidebar_compact;
    let per = if compact { 1 } else { 4 };
    // A full card is 3 rows and a gap; the last needs no gap.
    let fit = if compact { list.height as usize } else { (list.height as usize + 1) / 4 };
    let first = sidebar_window(app.sidebar_scroll, app.focus, n, fit);
    app.sidebar_scroll = first;
    let shown = fit.min(n - first.min(n));
    for (k, i) in (first..first + shown).enumerate() {
        let y = list.y + (k * per) as u16;
        let r = Rect { x: list.x, y, width: list.width, height: if compact { 1 } else { 3 } };
        card(f, app, i, r, compact, mini, t);
        app.hits.cards.push((r, app.agents[i].id));
    }
    // What's out of view, in the free rows above and below the list.
    let more = |k: usize, arrow: &str| if mini { format!(" {arrow} {k}") } else { format!("  {arrow} {k} more") };
    if first > 0 {
        f.render_widget(Paragraph::new(Span::styled(more(first, "↑"), Style::new().fg(t.muted))), Rect { y: inner.y + 1, height: 1, ..inner });
    }
    // Right under the last card shown: its gap row, or the next row.
    let below = n - first.min(n) - shown;
    let y = list.y + (shown.saturating_sub(1) * per) as u16 + if compact { 1 } else { 3 };
    if below > 0 && shown > 0 && y < area.bottom().saturating_sub(2) {
        f.render_widget(Paragraph::new(Span::styled(more(below, "↓"), Style::new().fg(t.muted))), Rect { y, height: 1, ..inner });
    }

    // "new agent" button pinned to the bottom of the rail.
    let by = area.bottom().saturating_sub(2);
    let btn = Rect { x: inner.x, y: by, width: inner.width, height: 1 };
    let line = TLine::from(vec![
        Span::styled(" + ", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(if mini { "new" } else { "new agent" }, Style::new().fg(t.fg)),
    ]);
    f.render_widget(Paragraph::new(line), btn);
    if let Some(k) = app.config.keys.label(Action::New).filter(|_| !mini) {
        f.render_widget(
            Paragraph::new(Span::styled(format!("{k} "), Style::new().fg(t.muted))).alignment(Alignment::Right),
            btn,
        );
    }
    app.hits.new_button = Some(btn);
}

/// The first of `n` cards to show when `fit` fit: where the list was, moved
/// just enough to show the focused card, and never past the end.
fn sidebar_window(was: usize, focus: usize, n: usize, fit: usize) -> usize {
    if fit == 0 || n <= fit {
        return 0;
    }
    let first = was.min(n - fit);
    if focus < first {
        focus
    } else if focus >= first + fit {
        focus + 1 - fit
    } else {
        first
    }
}

fn card(f: &mut Frame, app: &App, i: usize, r: Rect, compact: bool, mini: bool, t: &Theme) {
    let a = &app.agents[i];
    let selected = i == app.focus;
    let bg = if selected { t.surface_hi } else { t.surface };
    f.buffer_mut().set_style(r, Style::new().bg(bg));
    card_text(f, app, i, r, compact, mini, t);
    // Last, so the text's leading spaces don't paint over it.
    let bar_color = if a.state == State::Blocked {
        Some(t.red)
    } else if selected {
        Some(t.accent)
    } else if a.unseen {
        Some(state_color(t, a.state))
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
}

fn card_text(f: &mut Frame, app: &App, i: usize, r: Rect, compact: bool, mini: bool, t: &Theme) {
    let a = &app.agents[i];
    let selected = i == app.focus;
    let sc = state_color(t, a.state);
    let bg = if selected { t.surface_hi } else { t.surface };

    let (g, gs) = glyph(app, a.state);
    let idx = if i < 9 { format!("{}", i + 1) } else { " ".into() };
    let name_style = if selected {
        Style::new().fg(t.fg_bright).add_modifier(Modifier::BOLD)
    } else if a.state == State::Exited {
        Style::new().fg(t.dim)
    } else {
        Style::new().fg(t.fg)
    };
    if mini {
        // " 1 ⠹ name •": everything that tells agents apart and says who needs you.
        let dot = a.unseen && a.state != State::Blocked;
        let name_max = (r.width as usize).saturating_sub(5 + if dot { 2 } else { 0 });
        let mut line = vec![
            Span::raw(" "),
            Span::styled(idx, Style::new().fg(if selected { t.accent } else { t.muted })),
            Span::raw(" "),
            Span::styled(g, gs),
            Span::raw(" "),
            Span::styled(trunc(&a.name, name_max), name_style),
        ];
        if dot {
            line.push(Span::styled(" •", Style::new().fg(sc).add_modifier(Modifier::BOLD)));
        }
        f.render_widget(Paragraph::new(TLine::from(line)), Rect { height: 1, ..r });
        return;
    }
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
    if let Some(by) = &a.managed_by {
        line1.push(Span::styled(format!(" ‹{by}"), Style::new().fg(t.muted)));
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
    // The longest kind ("opencode") still gets a space before the sparkline.
    let spark_w = spark_width(10usize.min(w.saturating_sub(17)));
    let mut spans = vec![
        Span::raw("       "),
        Span::styled(format!("{kind:<9}"), Style::new().fg(mix(kind_color(t, a.kind), bg, 0.25))),
    ];
    spans.extend(sparkline(a, spark_w, if a.state == State::Idle || a.state == State::Exited { t.dim } else { sc }, mix(t.muted, bg, 0.4)));
    let used = 7 + 9 + spark_w;
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

/// Yellow from 70% used, red from 85%; otherwise nothing to say.
fn context_color(t: &Theme, used: f32) -> Option<Color> {
    if used >= 0.85 {
        Some(t.red)
    } else if used >= 0.70 {
        Some(t.yellow)
    } else {
        None
    }
}

/// "context 31% · ~691k left": for the pane's bottom border.
fn context_label(c: &crate::usage::Context) -> String {
    format!("context {}% · {}{} left", c.used_percent(), if c.exact { "" } else { "~" }, crate::usage::tokens(c.left()))
}

fn kind_color(t: &Theme, k: Kind) -> Color {
    match k {
        Kind::Claude => t.orange,
        Kind::Codex => t.cyan,
        Kind::Grok => t.fg_bright,
        Kind::OpenCode => t.accent,
        Kind::Cursor => mix(t.cyan, t.magenta, 0.5),
        Kind::Shell => t.dim,
        Kind::Custom => t.magenta,
    }
}

/// The widest bar count up to `room` that divides `SPARK_LEN`, so every bar
/// covers the same number of seconds and the line spans the whole history.
fn spark_width(room: usize) -> usize {
    (1..=room.min(SPARK_LEN)).rev().find(|&n| SPARK_LEN.is_multiple_of(n)).unwrap_or(0)
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
    // Leave a column for the ellipsis, and never split a wide character.
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
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
    if let Some((files, add, del)) = a.diff
        && files > 0
    {
        right.push(Span::styled(format!(" +{add}"), Style::new().fg(t.green)));
        right.push(Span::styled(format!(" −{del}"), Style::new().fg(t.red)));
    }
    right.push(Span::raw(" "));
    let mut status = vec![];
    if let Some(c) = a.context.filter(|_| !compact) {
        let color = context_color(t, c.used_frac()).unwrap_or(t.dim);
        status.push(Span::styled(format!(" {} ", context_label(&c)), Style::new().fg(color)));
        status.push(Span::styled("·", Style::new().fg(t.muted)));
    }
    status.push(Span::styled(format!(" {} ", a.state.label()), Style::new().fg(sc)));
    status.push(Span::styled(format!("{} ", fmt_dur(elapsed(a))), Style::new().fg(t.dim)));
    let status = TLine::from(status);
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
    app.hits.titles.push((Rect { height: 1, ..area }, app.agents[app.focus].id));
    app.pane_size = (inner.width, inner.height);
    // Keep every agent sized to the main pane so switching is instant.
    for a in app.agents.iter_mut() {
        a.resize(inner.width, inner.height);
    }
    let a = &app.agents[app.focus];
    app.hits.panes.push((inner, a.id));
    let cur = render_term(f.buffer_mut(), inner, a, t, app.term_focused);
    pane_overlays(f, a, area, inner, &app.key(Action::Close), t);
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
    if let Some(k) = app.config.keys.label(Action::Attention) {
        let hint = TLine::from(vec![Span::styled(format!(" {k} "), Style::new().fg(t.bg).bg(t.red).add_modifier(Modifier::BOLD)), Span::styled(" jump ", Style::new().fg(t.fg))]);
        f.render_widget(Paragraph::new(hint).alignment(Alignment::Right), r);
    }
    Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area }
}

/// Agents per grid page: more than that and each gets too small to read.
const GRID_PAGE: usize = 4;

/// Where each agent shown in the grid goes, and which page of how many
/// this is. A page holds up to four, two to a row; an odd one out gets its
/// row's whole width (three: two on top, one below).
fn grid_tiles(n: usize, focus: usize, area: Rect) -> (Vec<(usize, Rect)>, usize, usize) {
    if n == 0 {
        return (Vec::new(), 1, 1);
    }
    let page = focus.min(n - 1) / GRID_PAGE;
    let first = page * GRID_PAGE;
    let here = (n - first).min(GRID_PAGE);
    let rows = here.div_ceil(2);
    let row_rects = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    let mut tiles = Vec::with_capacity(here);
    for (r, rr) in row_rects.iter().enumerate() {
        let in_row = (here - r * 2).min(2);
        let cells = Layout::horizontal(vec![Constraint::Ratio(1, in_row as u32); in_row]).split(*rr);
        tiles.extend(cells.iter().enumerate().map(|(k, c)| (first + r * 2 + k, *c)));
    }
    (tiles, page + 1, n.div_ceil(GRID_PAGE))
}

fn grid_view(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) -> Option<(u16, u16)> {
    let (tiles, page, pages) = grid_tiles(app.agents.len(), app.focus, area);
    let mut cursor = None;
    for (i, c) in tiles {
        let focused = i == app.focus;
        let block = pane_block(app, &app.agents[i], focused, c.width < 70, t);
        let inner = block.inner(c);
        f.render_widget(block, c);
        app.hits.titles.push((Rect { height: 1, ..c }, app.agents[i].id));
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
        pane_overlays(f, a, c, inner, &app.key(Action::Close), t);
    }
    if pages > 1 {
        // On the bottom border: which page, and how to get to the others.
        let next = app.config.keys.label(Action::Next).unwrap_or_default();
        let s = format!(" page {page}/{pages} · {next} next ");
        let w = (s.chars().count() as u16).min(area.width);
        let r = Rect { x: area.x + area.width.saturating_sub(w) / 2, y: area.bottom().saturating_sub(1), width: w, height: 1 };
        f.render_widget(Paragraph::new(Span::styled(s, Style::new().fg(t.bg).bg(t.accent))), r);
    }
    cursor
}

fn pane_overlays(f: &mut Frame, a: &Agent, outer: Rect, inner: Rect, close: &str, t: &Theme) {
    let offset = a.term.lock().grid().display_offset();
    if offset > 0 {
        let s = TLine::from(vec![Span::styled(
            format!(" ↑ {offset} lines · shift+pgdn "),
            Style::new().fg(t.bg).bg(t.accent),
        )]);
        let r = Rect { x: outer.x + 2, y: outer.bottom().saturating_sub(1), width: (s.width() as u16).min(outer.width.saturating_sub(4)), height: 1 };
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
            Span::styled(format!("   {close}"), Style::new().fg(t.accent)),
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
    let rows = [
        ("enter".to_string(), "spawn your first agent"),
        (app.key(Action::New), "new agent anytime"),
        (app.key(Action::Attention), "jump to who needs you"),
        (app.key(Action::Help), "every shortcut"),
    ];
    let w = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0).max(6) + 1;
    for (y, (k, l)) in (y0 + 6..).zip(rows) {
        let line = TLine::from(vec![
            Span::styled(format!(" {k:<w$}"), Style::new().fg(t.accent)),
            Span::styled(format!("{l:<22}"), Style::new().fg(t.fg)),
        ]);
        center(f, y, line);
    }
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
    // Keep the cursor visible in long inputs by scrolling the window. `max`
    // is in cells, and CJK and emoji take two, so count cells, not chars.
    let chars: Vec<char> = inp.text.chars().collect();
    let cells = |c: char| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
    let cursor = inp.cursor.min(chars.len());
    let (mut start, mut before) = (cursor, 0);
    while start > 0 && before + cells(chars[start - 1]) < max {
        start -= 1;
        before += cells(chars[start]);
    }
    let (mut shown, mut used) = (String::new(), 0);
    for &c in &chars[start..] {
        if used + cells(c) > max {
            break;
        }
        used += cells(c);
        shown.push(c);
    }
    (vec![Span::styled(shown, Style::new().fg(if active { t.fg_bright } else { t.fg }))], active.then_some(before as u16))
}

fn modal(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let Some(m) = &app.modal else { return };
    match m {
        Modal::Menu(_) => {}
        Modal::Search(s) => search_box(f, app, s, area, t),
        Modal::New(form) => {
            let r = centered(area, 86, 16);
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
            let held = app.agents.iter().filter(|a| a.kind.takes_prompts() && matches!(a.state, State::Blocked | State::Starting)).count();
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
            let (spans, cur) = input_spans(inp, true, "message to send…", t, (inner.width as usize).saturating_sub(2));
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
                Rect { y: r.bottom().saturating_sub(2), height: 1, ..inner },
            );
        }
        Modal::Rename(_, inp) => {
            let r = centered(area, 50, 5);
            f.render_widget(Clear, r);
            let block = modal_block("Rename", t);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let row = Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: 1 };
            let (spans, cur) = input_spans(inp, true, "name", t, (row.width as usize).saturating_sub(2));
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
            let detach = app.config.keys.label(Action::Detach).map(|k| format!(" {k} detaches instead.")).unwrap_or_default();
            confirm(f, area, t, "Quit trix", &format!("{live} agent{} still running and will be stopped.{detach}", if live == 1 { " is" } else { "s are" }));
        }
        Modal::Help => help(f, app, area, t),
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

/// "↑340", "↑12k", or "on screen" for a hit on the live screen.
fn hit_place(h: &crate::search::Hit) -> String {
    match h.from_bottom {
        _ if h.start.line.0 >= 0 => "on screen".into(),
        n if n >= 10_000 => format!("↑{}k", n / 1000),
        n if n >= 1000 => format!("↑{:.1}k", n as f32 / 1000.0),
        n => format!("↑{n}"),
    }
}

fn search_box(f: &mut Frame, app: &App, s: &Search, area: Rect, t: &Theme) {
    let r = centered(area, 110, area.height.saturating_sub(4).clamp(9, 26));
    f.render_widget(Clear, r);
    let block = modal_block("Search every agent", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: inner.height.saturating_sub(1) };
    if inner.height < 4 || inner.width < 20 {
        return;
    }

    // Query, and the count on the right.
    let count = match (s.input.text.trim().is_empty(), s.hits.len()) {
        (true, _) => String::new(),
        (false, 0) => "no matches".into(),
        (false, n) => {
            let more = if s.capped { "+" } else { "" };
            format!("{n}{more} line{} · {} agent{}", if n == 1 { "" } else { "s" }, s.agents, if s.agents == 1 { "" } else { "s" })
        }
    };
    let room = (inner.width as usize).saturating_sub(count.width() + 4);
    let (spans, cur) = input_spans(&s.input, true, "search all scrollback…", t, room);
    let mut line = vec![Span::styled("› ", Style::new().fg(t.accent))];
    line.extend(spans);
    f.render_widget(Paragraph::new(TLine::from(line)), Rect { height: 1, ..inner });
    f.render_widget(
        Paragraph::new(Span::styled(count, Style::new().fg(t.dim))).alignment(Alignment::Right),
        Rect { height: 1, ..inner },
    );
    if let Some(cx) = cur {
        f.set_cursor_position((inner.x + 2 + cx, inner.y));
    }

    let list = Rect { y: inner.y + 2, height: inner.height.saturating_sub(4), ..inner };
    if s.hits.is_empty() {
        let note = if s.input.text.trim().is_empty() {
            "lowercase ignores case · any capital matches exactly"
        } else {
            "nothing in any agent's scrollback matches"
        };
        f.render_widget(Paragraph::new(Span::styled(note, Style::new().fg(t.muted))), Rect { height: 1, ..list });
    } else {
        let names: std::collections::HashMap<u32, &Agent> = app.agents.iter().map(|a| (a.id, a)).collect();
        let name_w = s.hits.iter().filter_map(|h| names.get(&h.agent)).map(|a| a.name.width()).max().unwrap_or(4).min(16);
        let rows = list.height as usize;
        let top = if s.sel >= rows { s.sel + 1 - rows } else { 0 };
        for (i, h) in s.hits.iter().enumerate().skip(top).take(rows) {
            let y = list.y + (i - top) as u16;
            let selected = i == s.sel;
            let row = Rect { y, height: 1, ..list };
            if selected {
                f.buffer_mut().set_style(row, Style::new().bg(t.surface_hi));
            }
            let (name, color) = match names.get(&h.agent) {
                Some(a) => (trunc(&a.name, name_w), kind_color(t, a.kind)),
                None => ("gone".into(), t.muted),
            };
            let place = hit_place(h);
            let fixed = 2 + name_w + 2 + 10 + 1;
            let width = (list.width as usize).saturating_sub(fixed);
            let (text, at) = crate::search::snippet(&h.text, h.at, h.len, width);
            let chars: Vec<char> = text.chars().collect();
            let end = (at + h.len).min(chars.len());
            let (before, hit, after): (String, String, String) =
                (chars[..at].iter().collect(), chars[at..end].iter().collect(), chars[end..].iter().collect());
            let base = Style::new().fg(if selected { t.fg_bright } else { t.fg });
            let spans = vec![
                Span::styled(if selected { "▸ " } else { "  " }, Style::new().fg(t.accent)),
                Span::styled(format!("{name:<name_w$}  "), Style::new().fg(color)),
                Span::styled(format!("{place:>9} "), Style::new().fg(t.dim)),
                Span::raw(" "),
                Span::styled(before, base),
                Span::styled(hit, Style::new().fg(t.bg).bg(t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(after, base),
            ];
            f.render_widget(Paragraph::new(TLine::from(spans)), row);
        }
    }

    f.render_widget(
        Paragraph::new(TLine::from(vec![
            Span::styled("↑↓", Style::new().fg(t.fg)),
            Span::styled(" select   ", Style::new().fg(t.dim)),
            Span::styled("enter", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" jump   ", Style::new().fg(t.dim)),
            Span::styled("esc", Style::new().fg(t.fg)),
            Span::styled(" close", Style::new().fg(t.dim)),
        ]))
        .alignment(Alignment::Right),
        Rect { y: r.bottom().saturating_sub(2), height: 1, ..inner },
    );
}

/// The key shown beside a menu item: its workspace key, if it has one and
/// it works in the menu too. (Enter restarts an exited agent outside the
/// menu, but in it Enter picks the highlighted item.)
fn menu_key(app: &App, item: MenuItem) -> String {
    item.action().and_then(|a| app.config.keys.label(a)).unwrap_or_default()
}

/// Where a menu goes: from the click down and right, kept on screen
/// (up or left when there's no room), or centred when opened by key.
fn menu_rect(at: Option<(u16, u16)>, w: u16, h: u16, area: Rect) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    let Some((x, y)) = at else { return centered(area, w, h) };
    let x = x.min(area.right().saturating_sub(w)).max(area.x);
    // Up from the click when there's no room below. A click left below the
    // screen by a resize since still lands it on screen.
    let y = if y + h <= area.bottom() { y } else { (y + 1).min(area.bottom()).saturating_sub(h).max(area.y) };
    Rect { x, y, width: w, height: h }
}

fn agent_menu(f: &mut Frame, app: &mut App, m: crate::app::Menu, area: Rect, t: &Theme) {
    let items = app.menu_items(m.agent);
    let Some(a) = app.agents.iter().find(|a| a.id == m.agent) else { return };
    let keys: Vec<String> = items.iter().map(|&i| menu_key(app, i)).collect();
    let label_w = items.iter().map(|i| i.label().width()).max().unwrap_or(0);
    let key_w = keys.iter().map(|k| k.width()).max().unwrap_or(0);
    let name = trunc(&a.name, 24);
    let w = (label_w + if key_w > 0 { key_w + 3 } else { 0 } + 4).max(name.width() + 7) as u16;
    let r = menu_rect(m.at, w, items.len() as u16 + 2, area);
    f.render_widget(Clear, r);
    let block = modal_block(&name, t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    app.hits.menu = Some(r);
    for (n, (&item, key)) in items.iter().zip(&keys).enumerate() {
        let row = Rect { y: inner.y + n as u16, height: 1, ..inner };
        if row.y >= inner.bottom() {
            break;
        }
        let selected = item == m.sel || (n == 0 && !items.contains(&m.sel));
        let base = if selected { Style::new().fg(t.bg).bg(t.accent).add_modifier(Modifier::BOLD) } else { Style::new().fg(t.fg) };
        // The item's letter, underlined where it first appears.
        let label = item.label();
        let at = label.char_indices().find(|(_, c)| c.to_ascii_lowercase() == item.letter()).map(|(i, _)| i);
        let mut spans = vec![Span::styled(" ", base)];
        match at {
            Some(i) => {
                let end = i + label[i..].chars().next().map_or(0, char::len_utf8);
                spans.push(Span::styled(&label[..i], base));
                spans.push(Span::styled(&label[i..end], base.add_modifier(Modifier::UNDERLINED)));
                spans.push(Span::styled(&label[end..], base));
            }
            None => spans.push(Span::styled(label, base)),
        }
        let gap = (row.width as usize).saturating_sub(label.width() + key.width() + 2);
        spans.push(Span::styled(" ".repeat(gap), base));
        spans.push(Span::styled(key.clone(), if selected { base } else { Style::new().fg(t.dim) }));
        spans.push(Span::styled(" ", base));
        f.render_widget(Paragraph::new(TLine::from(spans)), row);
        app.hits.menu_items.push((row, item));
    }
}

/// Two actions' keys as one label: `alt+j / k` when they share a prefix.
fn key_pair(a: &str, b: &str) -> String {
    match (a.rsplit_once('+'), b.rsplit_once('+')) {
        (Some((pa, _)), Some((pb, kb))) if pa == pb && !kb.is_empty() => format!("{a} / {kb}"),
        _ => format!("{a} / {b}"),
    }
}

fn help(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let r = centered(area, 78, 19);
    f.render_widget(Clear, r);
    let block = modal_block("Keys", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let k = |a| app.key(a);
    let left: Vec<(String, &str)> = vec![
        (String::new(), "AGENTS"),
        (k(Action::New), "new agent"),
        (k(Action::Jump), "jump to agent"),
        (key_pair(&k(Action::Next), &k(Action::Prev)), "next / previous"),
        (k(Action::Attention), "next one that needs you"),
        (k(Action::Broadcast), "broadcast a prompt"),
        (k(Action::Search), "search all scrollback"),
        (k(Action::Rename), "rename · double-click"),
        (k(Action::Close), "close"),
        (k(Action::Menu), "menu · or right-click"),
        ("enter".into(), "restart an exited agent"),
    ];
    let right: Vec<(String, &str)> = vec![
        (String::new(), "VIEW"),
        (k(Action::Grid), "grid ⇄ focus"),
        (k(Action::Sidebar), "toggle sidebar"),
        (k(Action::MiniSidebar), "mini sidebar"),
        (k(Action::Compact), "compact sidebar cards"),
        ("shift+pgup".into(), "scroll back"),
        ("wheel".into(), "scroll · click focus"),
        ("drag".into(), "select & copy"),
        ("shift+enter".into(), "newline to agent"),
        (k(Action::Detach), "detach · keeps running"),
        (k(Action::Quit), "quit · stops agents"),
    ];
    let cols = Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).split(Rect { x: inner.x + 2, y: inner.y + 1, width: inner.width.saturating_sub(4), height: inner.height.saturating_sub(1) });
    for (col, items) in cols.iter().zip([left, right]) {
        for (i, (k, d)) in items.iter().enumerate() {
            let line = if k.is_empty() {
                TLine::from(Span::styled(*d, Style::new().fg(t.dim).add_modifier(Modifier::BOLD)))
            } else {
                TLine::from(vec![Span::styled(format!("{k:<12} "), Style::new().fg(t.accent)), Span::styled(*d, Style::new().fg(t.fg))])
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
    f.render_widget(Paragraph::new(legend).alignment(Alignment::Center), Rect { y: r.bottom().saturating_sub(2), height: 1, x: r.x, width: r.width });
    let _ = Instant::now();
}

#[cfg(test)]
mod tests {
    #[test]
    fn wide_text_in_an_input_stays_inside_it() {
        let t = Theme::default();
        let cells = |spans: &[Span<'static>]| spans.iter().map(|s| s.content.width()).sum::<usize>();
        for (text, cursor) in [("漢".repeat(45), 45), ("漢".repeat(45), 0), ("a😀b".repeat(20), 30), ("x".repeat(80), 80)] {
            let mut inp = TextInput::new(&text);
            inp.cursor = cursor;
            let (spans, x) = input_spans(&inp, true, "", &t, 20);
            let x = x.unwrap() as usize;
            assert!(cells(&spans) <= 20, "{text:?}@{cursor}: {} cells in 20", cells(&spans));
            assert!(x < 20, "{text:?}@{cursor}: cursor at cell {x}");
        }
    }

    use super::*;

    #[test]
    fn a_menu_stays_on_screen() {
        let area = Rect::new(0, 0, 100, 20);
        // Below and right of the click when there's room, up when not.
        assert_eq!(menu_rect(Some((10, 5)), 24, 6, area), Rect::new(10, 5, 24, 6));
        assert_eq!(menu_rect(Some((10, 17)), 24, 6, area), Rect::new(10, 12, 24, 6));
        // Opened at row 45 of a taller window that has since shrunk.
        for at in [(10, 45), (500, 500), (99, 19)] {
            let r = menu_rect(Some(at), 24, 6, area);
            assert!(r.bottom() <= area.bottom() && r.right() <= area.right(), "{at:?}: {r:?}");
        }
        // Bigger than the screen: as much as fits.
        assert_eq!(menu_rect(Some((10, 45)), 24, 30, area), Rect::new(10, 0, 24, 20));
    }

    #[test]
    fn spark_width_divides_the_history() {
        assert_eq!(spark_width(10), 8);
        assert_eq!(spark_width(12), 12);
        assert_eq!(spark_width(5), 4);
        assert_eq!(spark_width(1), 1);
        assert_eq!(spark_width(0), 0);
        assert_eq!(spark_width(100), SPARK_LEN);
    }

    #[test]
    fn modals_survive_tiny_terminals() {
        let (_, mut app) = screen(false, 60);
        let id = app.agents[0].id;
        let modals = || vec![
            Modal::Broadcast(TextInput::new("hello"), BroadcastTarget::All),
            Modal::Rename(id, TextInput::new("web")),
            Modal::Help,
        ];
        for (w, h) in [(1, 1), (2, 2), (6, 2), (8, 3), (12, 4), (3, 18)] {
            for m in modals() {
                app.modal = Some(m);
                let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                term.draw(|f| draw(f, &mut app)).unwrap();
            }
        }
        for a in &app.agents {
            a.shutdown();
        }
    }

    /// The whole screen, drawn into a test terminal: four agents, the second
    /// one needing you, the third with unseen finished work.
    #[test]
    fn context_label_in_pane_only() {
        use crate::usage::Context;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app.sidebar_mini = false;
        for (name, ctx) in [
            ("roomy", Some(Context { used: 309_460, window: 1_000_000, exact: false })),
            ("tight", Some(Context { used: 230_000, window: 258_400, exact: true })),
            ("plain", None),
        ] {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.name = name.into();
            a.set_state(State::Idle, false);
            a.context = ctx;
            app.agents.push(a);
        }
        let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        narrow.draw(|f| draw(f, &mut app)).unwrap();
        let nb = narrow.backend().buffer();
        let bottom: String = (0..nb.area.height).map(|y| (0..nb.area.width).map(|x| nb[(x, y)].symbol().to_string()).collect::<String>()).find(|l| l.contains('╰')).unwrap();
        assert!(bottom.contains("context 31% · ~691k left · idle"), "fits an 80-column terminal: {bottom}");
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: Vec<String> = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
        let row3 = |name: &str| {
            let card = app.hits.cards[app.agents.iter().position(|a| a.name == name).unwrap()].0;
            text[card.y as usize + 2].clone()
        };
        for n in ["roomy", "tight", "plain"] {
            assert!(!row3(n).contains('%') && !row3(n).contains('●'), "no gauge in the sidebar: {}", row3(n));
        }
        let bottom = text.iter().find(|l| l.contains('╰')).unwrap();
        assert!(bottom.contains("context 31% · ~691k left"), "estimated window: {bottom}");
        app.focus = 1;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let bottom: String = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .find(|l| l.contains('╰'))
            .unwrap();
        assert!(bottom.contains("context 89% · 28k left"), "exact window, no ~: {bottom}");
        for a in &app.agents {
            a.shutdown();
        }
    }

    fn screen(mini: bool, width: u16) -> (String, App) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None; // never offer (or save over) the real workspace
        app.sidebar_mini = mini;
        for (name, state, unseen) in [("web", State::Working, false), ("api-gateway-v2", State::Blocked, true), ("docs", State::Done, true), ("sh", State::Idle, false)] {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.name = name.into();
            a.set_state(state, false);
            a.unseen = unseen;
            a.activity = "Edit src/app.rs".into();
            app.agents.push(a);
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 18)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let text = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        (text, app)
    }

    #[test]
    fn an_agents_menu_floats_where_it_was_opened() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        for (id, name) in [(1, "web"), (2, "api")] {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.id = id;
            a.name = name.into();
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        let mut frame = |app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>()
        };
        frame(&mut app);
        assert!(app.hits.titles.iter().any(|(r, id)| *id == 1 && r.height == 1), "the focused pane's title is clickable");
        app.open_menu(2, Some((10, 4)));
        let rows = frame(&mut app);
        println!("{}", rows.join("\n"));
        let menu = app.hits.menu.expect("drawn");
        assert_eq!((menu.x, menu.y), (10, 4), "from the click, down and right");
        let row = |item| app.hits.menu_items.iter().find(|(_, i)| *i == item).map(|(r, _)| rows[r.y as usize].clone()).unwrap();
        assert!(rows[4].contains("api"), "titled with its agent: {}", rows[4]);
        assert!(row(MenuItem::Rename).contains("Rename") && row(MenuItem::Rename).contains("alt+r"), "{}", row(MenuItem::Rename));
        assert!(row(MenuItem::Close).contains("Close") && row(MenuItem::Close).contains("alt+w"));
        assert!(row(MenuItem::CopyPath).contains("Copy folder path"));
        assert!(!rows.iter().any(|r| r.contains("Restart")), "only for an exited agent");
        // In the corner it opens up and to the left, all of it on screen.
        app.open_menu(2, Some((99, 23)));
        frame(&mut app);
        let menu = app.hits.menu.expect("drawn");
        assert!(menu.right() <= 100 && menu.bottom() <= 24 && menu.bottom() == 24, "{menu:?}");
        // From the keyboard, with nowhere to start from: centred.
        app.open_menu(1, None);
        frame(&mut app);
        let menu = app.hits.menu.expect("drawn");
        assert!(menu.x > 30 && menu.x < 50, "{menu:?}");
        // The help lists it.
        app.modal = Some(Modal::Help);
        let help = frame(&mut app).join("\n");
        assert!(help.contains("alt+o") && help.contains("right-click") && help.contains("double-click"), "{help}");
    }

    #[test]
    fn mini_sidebar_is_one_narrow_line_per_agent() {
        let (full, app) = screen(false, 100);
        assert!(full.lines().any(|l| l.starts_with(" ▌ 2")), "the full sidebar shows the bar too:\n{full}");
        let full_w = app.hits.cards[0].0.width;
        assert!(full.contains("new agent") && full.contains("alt+n"));
        for a in &app.agents {
            a.shutdown();
        }
        let (mini, app) = screen(true, 100);
        println!("{mini}");
        let cards = &app.hits.cards;
        assert_eq!(cards.len(), 4);
        assert!(cards.iter().all(|(r, _)| r.height == 1 && r.width == SIDEBAR_MINI_W - 2));
        assert!(cards[0].0.width * 2 <= full_w + 2, "about half as wide");
        assert!(cards.windows(2).all(|w| w[1].0.y == w[0].0.y + 1), "one row each, no gaps");
        let row = |i: usize| mini.lines().nth(cards[i].0.y as usize).unwrap().to_string();
        assert!(row(0).starts_with(" ▌1"), "focused: accent bar: {}", row(0));
        assert!(row(1).starts_with(" ▌2"), "needs you: red bar: {}", row(1));
        assert!(row(3).starts_with("  4"), "nothing to say: no bar: {}", row(3));
        assert!(row(0).contains("1 ") && row(0).contains("web"));
        assert!(row(1).contains("api-gate") && row(1).contains('…'), "long names are cut: {}", row(1));
        assert!(row(2).contains("docs •"), "unseen work keeps its dot: {}", row(2));
        assert!(!mini.contains("Edit src/app.rs") || mini.lines().all(|l| !l.starts_with("       Edit")), "no activity rows");
        assert!(mini.contains("+ new") && !mini.contains("new agent"));
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn the_sidebar_window_follows_the_focus() {
        // Everything fits: no scrolling, wherever it was.
        assert_eq!(sidebar_window(5, 3, 4, 10), 0);
        // Focus in view: the list stays put.
        assert_eq!(sidebar_window(2, 4, 20, 5), 2);
        // Focus below or above: just enough to show it.
        assert_eq!(sidebar_window(2, 9, 20, 5), 5);
        assert_eq!(sidebar_window(6, 1, 20, 5), 1);
        // Agents closed since: never past the end.
        assert_eq!(sidebar_window(18, 7, 8, 5), 3);
        assert_eq!(sidebar_window(3, 0, 20, 0), 0, "no room at all");
    }

    #[test]
    fn the_grid_shows_four_at_most_two_to_a_row() {
        let area = Rect::new(0, 0, 180, 46);
        let (w, h) = (90, 23);
        let shape = |n: usize, focus: usize| {
            let (tiles, page, pages) = grid_tiles(n, focus, area);
            (tiles.iter().map(|(i, r)| (*i, r.x, r.y, r.width, r.height)).collect::<Vec<_>>(), page, pages)
        };
        assert_eq!(shape(1, 0), (vec![(0, 0, 0, 180, 46)], 1, 1), "one: the whole area");
        assert_eq!(shape(2, 1), (vec![(0, 0, 0, w, 46), (1, w, 0, w, 46)], 1, 1), "two: side by side");
        assert_eq!(shape(3, 0), (vec![(0, 0, 0, w, h), (1, w, 0, w, h), (2, 0, h, 180, h)], 1, 1), "three: two up, one below");
        assert_eq!(shape(4, 3), (vec![(0, 0, 0, w, h), (1, w, 0, w, h), (2, 0, h, w, h), (3, w, h, w, h)], 1, 1), "four: 2×2");
        // More: four to a page, the page with the focused agent.
        let (tiles, page, pages) = shape(10, 5);
        assert_eq!((page, pages), (2, 3));
        assert_eq!(tiles.iter().map(|t| t.0).collect::<Vec<_>>(), [4, 5, 6, 7]);
        // The last page lays out what's left the same way: two, side by side.
        let (tiles, page, _) = shape(10, 9);
        assert_eq!((page, tiles.len()), (3, 2));
        assert_eq!((tiles[0].0, tiles[1].0, tiles[0].4), (8, 9, 46));
        assert_eq!(grid_tiles(0, 0, area).0.len(), 0);
    }

    #[test]
    fn full_cards_scroll_and_compact_ones_fit() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app.sidebar_mini = false;
        app.sidebar_compact = false;
        for k in 0..12 {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.id = k + 1;
            a.name = format!("agent-{}", k + 1);
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        let mut frame = |app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
        };
        let top = frame(&mut app);
        let cards = app.hits.cards.clone();
        assert!(cards.len() < 12 && cards.iter().all(|(r, _)| r.height == 3), "full cards, as many as fit: {cards:?}");
        assert_eq!(cards[0].1, 1);
        assert!(top.contains(&format!("↓ {} more", 12 - cards.len())) && !top.contains("↑"), "{top}");
        // Focus the last: the list scrolls to it.
        app.focus = 11;
        let bottom = frame(&mut app);
        let ids: Vec<u32> = app.hits.cards.iter().map(|(_, id)| *id).collect();
        assert_eq!(ids.last(), Some(&12), "{bottom}");
        assert!(bottom.contains(&format!("↑ {} more", 12 - ids.len())) && !bottom.contains("↓"), "{bottom}");
        // Compact: one line each, and all twelve fit.
        app.sidebar_compact = true;
        let compact = frame(&mut app);
        assert_eq!(app.hits.cards.len(), 12, "{compact}");
        assert!(app.hits.cards.iter().all(|(r, _)| r.height == 1));
        assert!(!compact.contains(" more"));
        // The help names the key.
        app.modal = Some(Modal::Help);
        let help = frame(&mut app);
        assert!(help.contains("alt+c") && help.contains("compact sidebar cards"), "{help}");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn mini_sidebar_fits_narrower_terminals() {
        let (full, app) = screen(false, 60);
        assert!(app.hits.cards.is_empty(), "60 columns is too narrow for the full sidebar:\n{full}");
        for a in &app.agents {
            a.shutdown();
        }
        let (_, app) = screen(true, 60);
        assert_eq!(app.hits.cards.len(), 4, "but the mini one fits");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn durations() {
        let s = Duration::from_secs;
        assert_eq!(fmt_dur(s(0)), "0s");
        assert_eq!(fmt_dur(s(59)), "59s");
        assert_eq!(fmt_dur(s(60)), "1m00s");
        assert_eq!(fmt_dur(s(134)), "2m14s");
        assert_eq!(fmt_dur(s(3599)), "59m59s");
        assert_eq!(fmt_dur(s(3600)), "1h00m");
        assert_eq!(fmt_dur(s(3725)), "1h02m");
    }

    #[test]
    fn trunc_fits_the_width() {
        assert_eq!(trunc("hello", 5), "hello");
        assert_eq!(trunc("hello world", 6), "hello…");
        assert_eq!(trunc("日本語テキスト", 6), "日本…");
        for max in 1..12 {
            for s in ["hello world", "日本語テキスト", "a日b本c語"] {
                assert!(trunc(s, max).width() <= max.max(1), "{s:?} at {max}: {:?}", trunc(s, max));
            }
        }
    }
}
