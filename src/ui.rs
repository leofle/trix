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
use crate::settings::{Picker, Row, Settings};
use crate::theme::{Source as ThemeSource, Theme, contrast, mix};

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
    // Painting the panes, paint the space around them too: nothing is left
    // in the terminal's own colors.
    if app.paints_panes() {
        f.buffer_mut().set_style(area, Style::new().fg(t.fg).bg(t.bg));
    }

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
    if app.view != View::Focus || app.agents.is_empty() {
        // Only the focus view has a strip: back there, it drops in again.
        app.strip = None;
    }
    let cursor = if app.agents.is_empty() {
        empty_state(f, app, main, &t);
        None
    } else {
        match app.view {
            View::Focus => focus_view(f, app, main, &t),
            View::Grid => grid_view(f, app, main, &t),
        }
    };
    app.hits.hints = bottom_bar(f, app, bottom, &t);

    if let Some(Modal::Menu(m)) = app.modal {
        // A menu floats over the workspace without dimming it.
        agent_menu(f, app, m, area, &t);
    } else if let Some(Modal::Settings(s)) = &app.modal {
        // A theme tried on shows undimmed all round.
        if s.picker.is_none() {
            backdrop(f.buffer_mut(), area, &t);
        }
        settings(f, app, area, &t);
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
        Span::styled(concat!(" v", env!("CARGO_PKG_VERSION")), Style::new().fg(t.muted)),
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

/// The keys to know, each a button for what it does but `jump` (which
/// agent?): returns where they are.
fn bottom_bar(f: &mut Frame, app: &App, area: Rect, t: &Theme) -> Vec<(Rect, Action)> {
    let key = |k: &str| Span::styled(format!(" {k} "), Style::new().fg(t.fg_bright).bg(t.surface_hi));
    let lab = |l: &'static str| Span::styled(format!(" {l}   "), Style::new().fg(t.dim));
    let mut spans = vec![Span::raw(" ")];
    let hints: Vec<(String, &str, Option<Action>)> = if app.modal.is_some() {
        vec![("esc".into(), "cancel", None)]
    } else {
        let shown: &[(Action, &str)] = if app.agents.is_empty() {
            &[(Action::New, "new agent"), (Action::Help, "keys"), (Action::Settings, "settings"), (Action::Quit, "quit")]
        } else {
            &[
                (Action::New, "new"),
                (Action::Attention, "attention"),
                (Action::Jump, "jump"),
                (Action::Grid, "grid"),
                (Action::Broadcast, "broadcast"),
                (Action::Help, "keys"),
                (Action::Settings, "settings"),
            ]
        };
        // An action with no key has nothing to hint at.
        shown.iter().filter_map(|&(a, l)| Some((app.config.keys.label(a)?, l, Some(a).filter(|&a| a != Action::Jump)))).collect()
    };
    let mut buttons = Vec::new();
    let mut x = area.x + 1;
    for (k, l, action) in hints {
        let (k, l) = (key(&k), lab(l));
        // The button is the key and its word, not the gap after them.
        let w = (k.width() + l.width()).saturating_sub(2) as u16;
        if let Some(a) = action
            && x < area.right()
        {
            buttons.push((Rect { x, y: area.y, width: w.min(area.right() - x), height: 1 }, a));
        }
        x = x.saturating_add(w + 2);
        spans.push(k);
        spans.push(l);
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
        // A toast over a button hides it.
        buttons.retain(|(b, _)| b.right() <= r.x);
    }
    buttons
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
    // Pinned agents (the first ones) hold still at the top while the rest
    // scroll under them. Between the two, a row says what's scrolled out of
    // view: a full card's gap row, or one a one-line list gives up for it.
    let pins = app.agents.iter().take_while(|a| a.pinned).count();
    let gap = usize::from(compact);
    // Unless it all fits, or holding them leaves no room for the rest.
    let held = if n > fit && fit > pins + gap { pins } else { 0 };
    let gap = if held > 0 { gap } else { 0 };
    let room = fit - held - gap;
    let height = if compact { 1 } else { 3 };
    for i in 0..held {
        let r = Rect { x: list.x, y: list.y + (i * per) as u16, width: list.width, height };
        card(f, app, i, r, compact, mini, t);
        app.hits.cards.push((r, app.agents[i].id));
    }
    let top = list.y + (held * per + gap) as u16;
    let rest = n - held;
    // A held card in focus is already in view: the rest stay where they were.
    let first = sidebar_window(app.sidebar_scroll, app.focus.checked_sub(held), rest, room);
    app.sidebar_scroll = first;
    let shown = room.min(rest - first.min(rest));
    for (k, i) in (held + first..held + first + shown).enumerate() {
        let r = Rect { x: list.x, y: top + (k * per) as u16, width: list.width, height };
        card(f, app, i, r, compact, mini, t);
        app.hits.cards.push((r, app.agents[i].id));
    }
    // What's out of view, in the free rows above and below the scrolling part.
    let more = |k: usize, arrow: &str| if mini { format!(" {arrow} {k}") } else { format!("  {arrow} {k} more") };
    if first > 0 {
        f.render_widget(Paragraph::new(Span::styled(more(first, "↑"), Style::new().fg(t.muted))), Rect { y: top - 1, height: 1, ..inner });
    }
    // Right under the last card shown: its gap row, or the next row.
    let below = rest - first.min(rest) - shown;
    let y = top + (shown.saturating_sub(1) * per) as u16 + if compact { 1 } else { 3 };
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
/// just enough to show the focused card (if it's one of these), and never
/// past the end.
fn sidebar_window(was: usize, focus: Option<usize>, n: usize, fit: usize) -> usize {
    if fit == 0 || n <= fit {
        return 0;
    }
    let first = was.min(n - fit);
    match focus {
        Some(f) if f < first => f,
        Some(f) if f >= first + fit => f + 1 - fit,
        _ => first,
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
        let name_max = (r.width as usize).saturating_sub(5 + if dot { 2 } else { 0 } + if a.pinned { 2 } else { 0 });
        let mut line = vec![
            Span::raw(" "),
            Span::styled(idx, Style::new().fg(if selected { t.accent } else { t.muted })),
            Span::raw(" "),
            Span::styled(g, gs),
            Span::raw(" "),
            Span::styled(trunc(&a.name, name_max), name_style),
        ];
        if a.pinned {
            line.extend([Span::raw(" "), pin_span(app, true)]);
        }
        if dot {
            line.push(Span::styled(" •", Style::new().fg(sc).add_modifier(Modifier::BOLD)));
        }
        f.render_widget(Paragraph::new(TLine::from(line)), Rect { height: 1, ..r });
        return;
    }
    let time = fmt_dur(elapsed(a));
    let w = r.width as usize;
    let name_max = w.saturating_sub(7 + time.len() + 2 + if a.pinned { 2 } else { 0 });
    let mut line1 = vec![
        Span::raw("  "),
        Span::styled(idx, Style::new().fg(if selected { t.accent } else { t.muted })),
        Span::raw("  "),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(trunc(&a.name, name_max), name_style),
    ];
    if a.pinned {
        line1.extend([Span::raw(" "), pin_span(app, true)]);
    }
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
        Kind::Copilot => t.green,
        Kind::Muse => mix(t.magenta, t.orange, 0.5),
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

/// The pin is this red in every theme.
pub const PIN_RED: Color = Color::Rgb(0xe5, 0x1c, 0x23);
/// A flat pushpin: a round head on a stem, one cell wide, in any font.
const PIN: &str = "⚲";
/// The Nerd Font pin (nf-oct-pin), for `[ui] nerd_font = true`.
const PIN_NERD: &str = "\u{f435}";

/// Where a pane's pin is: on its title, right after the corner and "─ ".
fn pin_rect(pane: Rect) -> Rect {
    Rect { x: pane.x + 3, y: pane.y, width: 1.min(pane.width.saturating_sub(3)), height: 1 }
}

fn pin_span(app: &App, pinned: bool) -> Span<'static> {
    let style = Style::new().fg(PIN_RED);
    let glyph = if app.config.ui.nerd_font { PIN_NERD } else { PIN };
    Span::styled(glyph, if pinned { style.add_modifier(Modifier::BOLD) } else { style })
}

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
        pin_span(app, a.pinned),
        Span::styled(if a.pinned { " pinned " } else { " " }, Style::new().fg(PIN_RED)),
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
    let block = pane_block(app, &app.agents[app.focus], true, false, t);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hits.titles.push((Rect { height: 1, ..area }, app.agents[app.focus].id));
    app.hits.pins.push((pin_rect(area), app.agents[app.focus].id));
    app.pane_size = (inner.width, inner.height);
    // Keep every agent sized to the main pane so switching is instant.
    for a in app.agents.iter_mut() {
        a.resize(inner.width, inner.height);
    }
    let a = &app.agents[app.focus];
    app.hits.panes.push((inner, a.id));
    let cur = render_term(f.buffer_mut(), inner, a, t, app.paints_panes().then_some(t), app.term_focused);
    pane_overlays(f, a, area, inner, &app.key(Action::Close), t);
    attention_strip(f, app, area, t);
    cur
}

/// When the strip's drop-in steps, in ms from when it appeared: its lower
/// edge first, then the whole bar, flashing bright and settling.
pub const STRIP_STEPS: [u64; 3] = [90, 180, 270];

/// The strip's background `ms` after it appeared: none while only its
/// lower edge shows, then `bar`, flashing bright and settling.
pub fn strip_bg(bar: Color, ms: u64) -> Option<Color> {
    let white = Color::Rgb(255, 255, 255);
    match ms {
        ms if ms < STRIP_STEPS[0] => None,
        ms if ms < STRIP_STEPS[1] => Some(mix(bar, white, 0.45)),
        ms if ms < STRIP_STEPS[2] => Some(mix(bar, white, 0.2)),
        _ => Some(bar),
    }
}

/// How long after it appeared the strip, `age` old now, steps next. None
/// once it has settled.
pub fn next_strip_step(age: Duration) -> Option<Duration> {
    STRIP_STEPS.iter().map(|&ms| Duration::from_millis(ms)).find(|&at| age < at)
}

/// A config color, as this theme has it.
fn paint(t: &Theme, p: crate::config::Paint) -> Color {
    match p {
        crate::config::Paint::Rgb(r, g, b) => Color::Rgb(r, g, b),
        crate::config::Paint::Theme(name) => t.named(name).unwrap_or(t.red),
    }
}

/// The strip's colors: the configured ones, or the theme's red with
/// whichever of its dark and light reads best on it.
fn strip_colors(app: &App, t: &Theme) -> (Color, Color) {
    let bar = app.config.strip.color.map_or(t.red, |p| paint(t, p));
    let text = match app.config.strip.text {
        Some(p) => paint(t, p),
        None if contrast(t.bg, bar) >= contrast(t.fg_bright, bar) => t.bg,
        None => t.fg_bright,
    };
    (bar, text)
}

/// The bar over the focused pane's title when someone *else* is waiting
/// on the human. It covers the title rather than taking a row, so nothing
/// moves and no agent is resized when it comes and goes.
fn attention_strip(f: &mut Frame, app: &mut App, pane: Rect, t: &Theme) {
    let waiting: Vec<(usize, &Agent)> = app
        .agents
        .iter()
        .enumerate()
        .filter(|(i, a)| *i != app.focus && a.state == State::Blocked)
        .collect();
    let Some(&(i, first)) = waiting.iter().min_by_key(|(_, a)| a.state_since).filter(|_| pane.width >= 24 && pane.height >= 3) else {
        app.strip = None;
        return;
    };
    let (id, name, activity) = (first.id, first.name.clone(), first.activity.clone());
    let waited = fmt_dur(elapsed(first));
    let more = if waiting.len() > 1 { format!("  +{} more", waiting.len() - 1) } else { String::new() };
    let since = match app.strip {
        Some((shown, at)) if shown == id => at,
        _ => Instant::now(),
    };
    app.strip = Some((id, since));
    let r = Rect { x: pane.x + 1, width: pane.width - 2, ..Rect { height: 1, ..pane } };
    app.hits.strip = Some((r, id));

    let (bar, text) = strip_colors(app, t);
    let buf = f.buffer_mut();
    let Some(bg) = strip_bg(bar, since.elapsed().as_millis() as u64) else {
        // Its lower edge, coming down over the title.
        for x in r.x..r.right() {
            if let Some(c) = buf.cell_mut((x, r.y)) {
                c.set_symbol("▀").set_fg(bar);
            }
        }
        return;
    };
    let st = Style::new().fg(text).bg(bg);
    let bold = st.add_modifier(Modifier::BOLD);
    for x in r.x..r.right() {
        if let Some(c) = buf.cell_mut((x, r.y)) {
            c.set_symbol(" ").set_style(st);
        }
    }
    let number = format!(" ({})", i + 1);
    let wait = format!(" · {waited}");
    let right = app.config.keys.label(Action::Attention).map(|k| (k, "  jump "));
    let right_w = right.as_ref().map_or(0, |(k, j)| k.width() + j.width());
    let fixed = 3 + name.width() + number.width() + 2 + wait.width() + more.width() + right_w + 1;
    let room = (r.width as usize).saturating_sub(fixed);
    let mut left = vec![Span::styled(" ▲ ", bold), Span::styled(name, bold), Span::styled(number, st), Span::styled("  ", st)];
    if room >= 4 {
        left.push(Span::styled(trunc(&activity, room), st));
    }
    left.extend([Span::styled(wait, st), Span::styled(more, st)]);
    f.render_widget(Paragraph::new(TLine::from(left)), r);
    if let Some((k, jump)) = right {
        f.render_widget(Paragraph::new(TLine::from(vec![Span::styled(k, bold), Span::styled(jump, st)])).alignment(Alignment::Right), r);
    }
}

/// Agents per grid page: more than that and each gets too small to read.
pub const GRID_PAGE: usize = 4;

/// Is agent `i` on the grid page that's on screen (the focused agent's)?
pub fn on_grid_page(i: usize, focus: usize) -> bool {
    i / GRID_PAGE == focus / GRID_PAGE
}

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
        app.hits.pins.push((pin_rect(c), app.agents[i].id));
        app.agents[i].resize(inner.width, inner.height);
        let a = &app.agents[i];
        app.hits.panes.push((inner, a.id));
        let cur = render_term(f.buffer_mut(), inner, a, t, app.paints_panes().then_some(t), focused && app.term_focused);
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

/// An emulator color as ratatui's, and whether it's a dim one. With a
/// palette, the 16 terminal colors and the default ones are the theme's;
/// without, they're left to the terminal. Exact (24-bit) colors stay.
fn map_color(c: AColor, palette: Option<&Theme>) -> (Color, bool) {
    let color = |i: u8| palette.map_or_else(|| ansi(i), |t| t.ansi[i as usize]);
    match c {
        AColor::Spec(rgb) => (Color::Rgb(rgb.r, rgb.g, rgb.b), false),
        AColor::Indexed(i) if i < 16 => (color(i), false),
        AColor::Indexed(i) => (Color::Indexed(i), false),
        AColor::Named(n) => {
            let v = n as usize;
            if v < 16 {
                (color(v as u8), false)
            } else {
                let (fg, bg, bright) = palette.map_or((Color::Reset, Color::Reset, Color::Reset), |t| (t.fg, t.bg, t.fg_bright));
                match n {
                    NamedColor::DimBlack => (color(0), true),
                    NamedColor::DimRed => (color(1), true),
                    NamedColor::DimGreen => (color(2), true),
                    NamedColor::DimYellow => (color(3), true),
                    NamedColor::DimBlue => (color(4), true),
                    NamedColor::DimMagenta => (color(5), true),
                    NamedColor::DimCyan => (color(6), true),
                    NamedColor::DimWhite => (color(7), true),
                    NamedColor::Foreground => (fg, false),
                    NamedColor::Background => (bg, false),
                    NamedColor::BrightForeground => (bright, false),
                    NamedColor::DimForeground => (fg, true),
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

/// Blit the emulator grid into the ratatui buffer, its terminal colors from
/// `palette` when trix paints the panes. Returns the cursor cell when the
/// child wants a visible cursor.
fn render_term(buf: &mut Buffer, area: Rect, a: &Agent, t: &Theme, palette: Option<&Theme>, focused: bool) -> Option<(u16, u16)> {
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
        let (fg, dim_fg) = map_color(cell.fg, palette);
        let (bg, _) = map_color(cell.bg, palette);
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
    center(f, y0 + 4, TLine::from(Span::styled(concat!("v", env!("CARGO_PKG_VERSION")), Style::new().fg(t.muted))));
    let rows = [
        ("enter".to_string(), "spawn your first agent"),
        (app.key(Action::New), "new agent anytime"),
        (app.key(Action::Attention), "jump to who needs you"),
        (app.key(Action::Help), "every shortcut"),
        (app.key(Action::Settings), "theme, sounds, notifications"),
    ];
    let w = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0).max(6) + 1;
    for (y, (k, l)) in (y0 + 6..).zip(rows) {
        let line = TLine::from(vec![
            Span::styled(format!(" {k:<w$}"), Style::new().fg(t.accent)),
            Span::styled(format!("{l:<29}"), Style::new().fg(t.fg)),
        ]);
        center(f, y, line);
    }
    let theme_note = match &app.theme_source {
        ThemeSource::Omarchy => "following your omarchy theme".to_string(),
        ThemeSource::File(p) => format!("theme from {}", crate::app::tilde(p)),
        ThemeSource::Builtin => "tokyo night".to_string(),
    };
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

/// The chips `from..to` of a row too long for `room` cells: as many as fit,
/// scrolled only as far as it takes to keep `sel` in sight.
fn chip_window(widths: &[usize], sel: usize, room: usize) -> (usize, usize) {
    let mut from = 0;
    while from < sel && widths[from..=sel].iter().sum::<usize>() > room {
        from += 1;
    }
    let (mut to, mut used) = (from, 0);
    while to < widths.len() && used + widths[to] <= room {
        used += widths[to];
        to += 1;
    }
    (from, to.max(sel + 1))
}

fn modal(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let Some(m) = &app.modal else { return };
    match m {
        Modal::Menu(_) | Modal::Settings(_) => {}
        Modal::Search(s) => search_box(f, app, s, area, t),
        Modal::New(form) => {
            // The kind row keeps the main kinds and "more ▸", so new kinds
            // don't widen it.
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
                        let row = form.kind_row();
                        // Each chip is " label " and a space.
                        let widths: Vec<usize> = row.iter().map(|(l, _)| l.width() + 3).collect();
                        let chosen = row.iter().position(|(_, k)| *k == Some(form.kind)).unwrap_or(0);
                        // Open, more's kinds follow the main ones: ‹ says so.
                        let lead = usize::from(form.more);
                        let (from, to) = if lead + widths.iter().sum::<usize>() <= field_w { (0, row.len()) } else { chip_window(&widths, chosen, field_w.saturating_sub(2)) };
                        let mut v = Vec::new();
                        if form.more || from > 0 {
                            v.push(Span::styled("‹", Style::new().fg(t.muted)));
                        }
                        for (c, (label, kind)) in row.iter().enumerate().take(to).skip(from) {
                            v.push(match kind {
                                Some(k) if c == chosen => Span::styled(
                                    format!(" {label} "),
                                    Style::new().fg(t.bg).bg(if active { t.accent } else { kind_color(t, *k) }).add_modifier(Modifier::BOLD),
                                ),
                                Some(_) => Span::styled(format!(" {label} "), Style::new().fg(t.dim)),
                                None => Span::styled(format!(" {label} "), Style::new().fg(t.muted)),
                            });
                            v.push(Span::raw(" "));
                        }
                        if to < row.len() {
                            v.push(Span::styled("›", Style::new().fg(t.muted)));
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
                        let ph = if form.kind == Kind::Custom { "command to run, e.g. aider --model …" } else { "extra flags, e.g. --model opus" };
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

// ───────────────────────────── settings ─────────────────────────────

fn settings(f: &mut Frame, app: &mut App, area: Rect, t: &Theme) {
    let Some(Modal::Settings(s)) = &app.modal else { return };
    let mut rows = Vec::new();
    let r = match &s.picker {
        Some(p) => theme_list(f, p, area, t, &mut rows),
        None => settings_list(f, app, s, area, t, &mut rows),
    };
    app.hits.settings = Some(r);
    app.hits.settings_rows = rows;
}

/// What a setting is now, and in what color, flush right on its row.
fn setting_value(app: &App, s: &Settings, row: Row, t: &Theme) -> Vec<Span<'static>> {
    use crate::config::Tone;
    let text = |v: String, c: Color| vec![Span::styled(v, Style::new().fg(c))];
    let toggle = |on: bool| if on { text("on".into(), t.green) } else { text("off".into(), t.dim) };
    let tone = |tone: &Tone| text(crate::settings::tone_label(tone), if *tone == Tone::Off { t.dim } else { t.fg });
    let n = &app.config.notify;
    match row {
        Row::Theme => text(format!("{} ›", s.theme), t.accent),
        Row::ThemePanes => toggle(app.config.theme.panes),
        Row::Strip => {
            let (bar, words) = strip_colors(app, t);
            let name = crate::settings::paint_text(app.config.strip.color);
            vec![Span::styled(format!("{name} "), Style::new().fg(t.fg)), Span::styled(" ▲ ", Style::new().fg(words).bg(bar))]
        }
        Row::SidebarMini => text((if app.sidebar_mini { "narrow" } else { "wide" }).into(), t.fg),
        Row::SidebarCompact => text((if app.sidebar_compact { "compact" } else { "full" }).into(), t.fg),
        Row::NerdFont => {
            let mut v = toggle(app.config.ui.nerd_font);
            v.insert(0, pin_span(app, true));
            v.insert(1, Span::raw("  "));
            v
        }
        Row::NewAgent => text(crate::settings::start_as_text(app.config.new.agent).into(), t.fg),
        Row::NewWorktree => toggle(app.config.new.worktree),
        Row::Restore => text(crate::settings::restore_label(app.config.workspace.restore).into(), t.fg),
        Row::NotifyNeedsYou => toggle(n.needs_you),
        Row::NotifyDone => toggle(n.done),
        Row::Desktop => toggle(n.desktop),
        Row::Bell => toggle(n.bell),
        Row::SoundNeedsYou => tone(&app.config.sound.needs_you),
        Row::SoundDone => tone(&app.config.sound.done),
    }
}

/// One row of a list, `right` flush right: the selected one on a lighter
/// band, with a marker.
fn list_row(f: &mut Frame, r: Rect, selected: bool, left: Vec<Span<'static>>, right: Vec<Span<'static>>, t: &Theme) {
    let band = if selected { Style::new().bg(t.surface_hi) } else { Style::new() };
    let marker = Span::styled(if selected { " › " } else { "   " }, Style::new().fg(t.accent).add_modifier(Modifier::BOLD));
    let used = 3 + left.iter().chain(&right).map(|s| s.width()).sum::<usize>() + 1;
    let gap = Span::raw(" ".repeat((r.width as usize).saturating_sub(used)));
    let spans: Vec<Span> = std::iter::once(marker).chain(left).chain([gap]).chain(right).chain([Span::raw(" ")]).collect();
    f.render_widget(Paragraph::new(TLine::from(spans)).style(band), r);
}

/// Keys and what they do, for the foot of a panel.
fn key_help(pairs: &[(&str, &str)], t: &Theme) -> TLine<'static> {
    let mut spans = Vec::new();
    for (i, (k, what)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(k.to_string(), Style::new().fg(t.accent)));
        spans.push(Span::styled(format!(" {what}"), Style::new().fg(t.dim)));
    }
    TLine::from(spans)
}

/// The foot of a panel `r`: its keys, and a line under them.
fn panel_foot(f: &mut Frame, r: Rect, keys: TLine<'static>, note: String, t: &Theme) {
    let foot = Rect { x: r.x + 3, width: r.width.saturating_sub(6), y: r.bottom().saturating_sub(3), height: 1 };
    if r.height < 6 {
        return;
    }
    f.render_widget(Paragraph::new(keys), foot);
    let width = foot.width as usize;
    f.render_widget(Paragraph::new(Span::styled(trunc(&note, width), Style::new().fg(t.muted))), Rect { y: foot.y + 1, ..foot });
}

/// A line of the settings list: a heading, the gap before one, or a row
/// (its number in `Row::ALL`).
#[derive(Clone, Copy, PartialEq)]
enum SettingsLine {
    Gap,
    Heading(&'static str),
    Row(usize),
}

fn settings_lines() -> Vec<SettingsLine> {
    let mut lines = Vec::new();
    for (i, row) in Row::ALL.iter().enumerate() {
        if let Some(h) = row.heading() {
            if i > 0 {
                lines.push(SettingsLine::Gap);
            }
            lines.push(SettingsLine::Heading(h));
        }
        lines.push(SettingsLine::Row(i));
    }
    lines
}

/// The first of `n` lines to show in room for `fit`, keeping line `at` on
/// screen, in the middle once they don't all fit.
fn window(at: usize, n: usize, fit: usize) -> usize {
    if n <= fit { 0 } else { at.saturating_sub(fit / 2).min(n - fit) }
}

fn settings_list(f: &mut Frame, app: &App, s: &Settings, area: Rect, t: &Theme, rows: &mut Vec<(Rect, usize)>) -> Rect {
    let lines = settings_lines();
    // Borders, a line above and below the list, two lines at the foot.
    let r = centered(area, 66, lines.len() as u16 + 6);
    f.render_widget(Clear, r);
    let block = modal_block("Settings", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let fit = (inner.height as usize).saturating_sub(4);
    let sel = Row::ALL.iter().position(|&r| r == s.sel).unwrap_or(0);
    let at = lines.iter().position(|&l| l == SettingsLine::Row(sel)).unwrap_or(0);
    // Up to the heading over the first one, so the list starts with it.
    let start = match window(at, lines.len(), fit) {
        1 if matches!(lines[0], SettingsLine::Heading(_)) => 0,
        n => n,
    };
    // Notifications that never come have no way to come; a theme that's
    // not trix's own has no panes to paint.
    let quiet = !app.config.notify.needs_you && !app.config.notify.done;
    let unpainted = !app.theme_source.paints_panes();
    for (k, line) in lines.iter().skip(start).take(fit).enumerate() {
        let l = Rect { y: inner.y + 1 + k as u16, height: 1, ..inner };
        let i = match *line {
            SettingsLine::Gap => continue,
            SettingsLine::Heading(head) => {
                f.render_widget(Paragraph::new(Span::styled(format!("   {head}"), Style::new().fg(t.dim).add_modifier(Modifier::BOLD))), l);
                continue;
            }
            SettingsLine::Row(i) => i,
        };
        let row = Row::ALL[i];
        let selected = row == s.sel;
        let off = (quiet && matches!(row, Row::Desktop | Row::Bell)) || (unpainted && row == Row::ThemePanes);
        let label = Style::new().fg(if off { t.muted } else if selected { t.fg_bright } else { t.fg });
        let mut left = vec![Span::styled(row.label(), if selected { label.add_modifier(Modifier::BOLD) } else { label })];
        let key = match row {
            Row::SidebarMini => app.config.keys.label(Action::MiniSidebar),
            Row::SidebarCompact => app.config.keys.label(Action::Compact),
            _ => None,
        };
        if let Some(k) = key {
            left.push(Span::styled(format!("  {k}"), Style::new().fg(t.muted)));
        }
        let mut value = setting_value(app, s, row, t);
        for v in &mut value {
            if off {
                v.style = v.style.fg(t.muted);
            }
            if selected {
                v.style = v.style.add_modifier(Modifier::BOLD);
            }
        }
        list_row(f, l, selected, left, value, t);
        rows.push((l, i));
    }
    more_marks(f, inner, start, fit, lines.len(), t);
    let keys = key_help(&[("↑↓", "choose"), ("enter ←→", "change"), ("esc", "close")], t);
    panel_foot(f, r, keys, format!("Keys, mutes, a notify command: {}", tilde(&app.config_path)), t);
    r
}

/// "▲ more" and "▼ more" over and under a list that goes on.
fn more_marks(f: &mut Frame, inner: Rect, start: usize, fit: usize, n: usize, t: &Theme) {
    let more = |f: &mut Frame, y: u16, s: &'static str| {
        f.render_widget(Paragraph::new(Span::styled(s, Style::new().fg(t.muted))).alignment(Alignment::Right), Rect { y, height: 1, width: inner.width.saturating_sub(1), ..inner });
    };
    if start > 0 {
        more(f, inner.y, "▲ more");
    }
    if start + fit < n {
        more(f, inner.y + 1 + fit as u16, "▼ more");
    }
}

/// The list of themes: each with its colors, the one in use marked.
fn theme_list(f: &mut Frame, p: &Picker, area: Rect, t: &Theme, rows: &mut Vec<(Rect, usize)>) -> Rect {
    let n = p.choices.len();
    // Borders, a line above and below the list, two lines at the foot.
    let h = (n as u16).saturating_add(2 + 2 + 2);
    let r = centered(area, 60, h);
    f.render_widget(Clear, r);
    let block = modal_block("Theme", t);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let fit = (inner.height as usize).saturating_sub(4);
    let start = window(p.sel, n, fit);
    for (k, (i, c)) in p.choices.iter().enumerate().skip(start).take(fit).enumerate() {
        let l = Rect { y: inner.y + 1 + k as u16, height: 1, ..inner };
        let selected = i == p.sel;
        let name = Style::new().fg(if selected { t.fg_bright } else { t.fg });
        let mut left = vec![Span::styled(c.name.clone(), if selected { name.add_modifier(Modifier::BOLD) } else { name })];
        if i == p.current {
            left.push(Span::styled("  in use", Style::new().fg(t.green)));
        }
        list_row(f, l, selected, left, swatch(c), t);
        rows.push((l, i));
    }
    more_marks(f, inner, start, fit, n, t);
    let keys = key_help(&[("↑↓", "try one"), ("enter", "keep it"), ("esc", "back")], t);
    panel_foot(f, r, keys, "Agents running now may need a restart to match.".into(), t);
    r
}

/// A theme's colors at a glance: its text on its background, then its
/// red, yellow, green, cyan, accent and magenta. None for the terminal's
/// own, which trix doesn't know.
fn swatch(c: &crate::theme::Choice) -> Vec<Span<'static>> {
    if c.source == ThemeSource::Builtin {
        return Vec::new();
    }
    let l = &c.look;
    let mut v = vec![Span::styled(" Aa ", Style::new().fg(l.fg).bg(l.bg))];
    v.extend([l.red, l.yellow, l.green, l.cyan, l.accent, l.magenta].map(|c| Span::styled("  ", Style::new().bg(c))));
    v
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
        (k(Action::Settings), "settings · theme, sounds"),
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
            Modal::Settings(Settings { sel: Row::Bell, theme: "Ristretto".into(), files: [None, None], strip: None, picker: None }),
            Modal::Settings(Settings {
                sel: Row::Theme,
                theme: "Ristretto".into(),
                files: [None, None],
                strip: None,
                picker: Some(Picker { choices: crate::theme::choices_here(std::path::Path::new("/nonexistent")).0, sel: 3, current: 0 }),
            }),
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
    fn a_theme_of_trix_own_paints_the_panes() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let a = crate::agent::test_agent(crate::agent::Kind::Claude);
        let mut p: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        p.advance(&mut *a.term.lock(), b"zplain \x1b[31mzred\x1b[0m \x1b[38;2;1;2;3mzexact\x1b[0m");
        app.agents.push(a);
        let t = app.theme.clone();
        // fg and bg of the first letter of `word`, and of the pane's border left of it.
        let cells = |app: &mut App, word: &str| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let b = term.backend().buffer().clone();
            let (w, h) = (b.area.width, b.area.height);
            let at = (0..h).flat_map(|y| (0..w).map(move |x| (x, y)));
            let hit = |&(x, y): &(u16, u16)| word.chars().enumerate().all(|(i, c)| x + (i as u16) < w && b[(x + i as u16, y)].symbol() == c.to_string());
            let (x, y) = at.clone().find(hit).unwrap_or_else(|| panic!("{word} not on screen"));
            ((b[(x, y)].fg, b[(x, y)].bg), b[(x - 1, y)].bg)
        };

        app.theme_source = ThemeSource::Builtin;
        assert_eq!(cells(&mut app, "zplain"), ((Color::Reset, Color::Reset), Color::Reset), "no file: the terminal's colors");
        assert_eq!(cells(&mut app, "zred").0.0, Color::Red, "and its palette");
        app.theme_source = ThemeSource::Omarchy;
        assert_eq!(cells(&mut app, "zplain"), ((Color::Reset, Color::Reset), Color::Reset), "Omarchy themes the terminal already");

        app.theme_source = ThemeSource::File("/x/colors.toml".into());
        assert_eq!(cells(&mut app, "zplain"), ((t.fg, t.bg), t.bg), "default text, and the space around it");
        assert_eq!(cells(&mut app, "zred").0, (t.ansi[1], t.bg), "the theme's red");
        assert_eq!(cells(&mut app, "zexact").0.0, Color::Rgb(1, 2, 3), "an exact color stays");
        app.config.theme.panes = false;
        assert_eq!(cells(&mut app, "zplain"), ((Color::Reset, Color::Reset), Color::Reset), "[theme] panes = false: the terminal's");
        assert_eq!(cells(&mut app, "zred").0.0, Color::Red);
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn empty_screen_says_where_the_theme_is_from() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let own = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".config/trix/colors.toml");
        for (source, note) in [
            (ThemeSource::Builtin, "tokyo night"),
            (ThemeSource::Omarchy, "following your omarchy theme"),
            (ThemeSource::File(own), "theme from ~/.config/trix/colors.toml"),
            (ThemeSource::File("/opt/themes/colors.toml".into()), "theme from /opt/themes/colors.toml"),
        ] {
            app.theme_source = source;
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer();
            let text: Vec<String> = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
            assert!(text.iter().any(|l| l.contains(note)), "{note}:\n{}", text.join("\n"));
        }
    }

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
    fn the_new_agent_form_keeps_its_width_with_more() {
        use crate::agent::Kind;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        // The kind row, from the label on, and the dialog's width.
        let mut kind_row = |app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer();
            let lines: Vec<String> = (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
            let top = lines.iter().find(|l| l.contains("New agent")).expect("the dialog");
            let width = top.chars().skip_while(|&c| c != '╭').take_while(|&c| c != '╮').count() + 1;
            let row = lines.iter().find(|l| l.contains("Agent ")).expect("the kind row");
            let row = row[row.find("Agent").unwrap() + 5..].trim().trim_end_matches('│').trim_end().to_string();
            (row, width)
        };
        app.open_new();
        assert_eq!(kind_row(&mut app), ("claude   codex   shell   custom   more ▸".into(), 72));
        let Some(Modal::New(f)) = &mut app.modal else { panic!("no form") };
        f.kind = Kind::Grok;
        f.more = true;
        assert_eq!(kind_row(&mut app).0, "‹ grok   opencode   cursor   copilot   muse", "open, in place");
        let Some(Modal::New(f)) = &mut app.modal else { panic!("no form") };
        f.more = false;
        f.kind = Kind::Copilot;
        assert_eq!(kind_row(&mut app).0, "claude   codex   shell   custom   copilot ▸", "chosen, in more's place");

        // Too many for the row: it scrolls to the kind chosen, ‹ › saying what's off it.
        app.config.new.kinds = Kind::ALL.to_vec();
        app.config.new.agent = Some(Kind::Claude);
        app.open_new();
        let (row, width) = kind_row(&mut app);
        assert_eq!(width, 72, "no wider for more kinds");
        assert!(row.starts_with("claude ") && row.ends_with('›') && !row.contains('‹'), "{row}");
        let Some(Modal::New(f)) = &mut app.modal else { panic!("no form") };
        f.kind = Kind::Custom;
        let (row, _) = kind_row(&mut app);
        assert!(row.starts_with('‹') && row.ends_with("custom") && !row.contains("claude"), "{row}");
    }

    #[test]
    fn a_long_chip_row_scrolls_to_the_chosen_one() {
        let w = [8, 7, 7, 8, 9];
        assert_eq!(chip_window(&w, 0, 39), (0, 5), "it all fits");
        assert_eq!(chip_window(&w, 1, 20), (0, 2));
        assert_eq!(chip_window(&w, 4, 20), (3, 5), "only as far as it takes");
        assert_eq!(chip_window(&w, 2, 5), (2, 3), "wider than the room: the chosen one, cut off");
    }

    #[test]
    fn the_strip_steps_by_its_age() {
        let (bar, white) = (Color::Rgb(0xf7, 0x76, 0x8e), Color::Rgb(255, 255, 255));
        assert_eq!((strip_bg(bar, 0), strip_bg(bar, 89)), (None, None), "its lower edge only");
        for ms in [90, 179] {
            assert_eq!(strip_bg(bar, ms), Some(mix(bar, white, 0.45)), "{ms}: flashing bright");
        }
        for ms in [180, 269] {
            assert_eq!(strip_bg(bar, ms), Some(mix(bar, white, 0.2)), "{ms}: settling");
        }
        assert_eq!((strip_bg(bar, 270), strip_bg(bar, 60_000)), (Some(bar), Some(bar)), "settled");
        let step = |ms| next_strip_step(Duration::from_millis(ms)).map(|d| d.as_millis());
        assert_eq!([step(0), step(89), step(90), step(269), step(270), step(5000)], [Some(90), Some(90), Some(180), Some(270), None, None]);
    }

    #[test]
    fn the_strip_drops_over_the_title_and_moves_nothing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        for (id, name) in [(1, "web"), (2, "api"), (3, "docs")] {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.id = id;
            a.name = name.into();
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        let t = app.theme.clone();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
        let mut frame = |app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            term.backend().buffer().clone()
        };
        let row = |b: &Buffer, y: u16| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>();
        frame(&mut app);
        let (size, title) = (app.pane_size, app.hits.titles[0].0);
        assert!(app.hits.strip.is_none() && app.strip.is_none());

        app.agents[1].set_state(State::Blocked, false);
        app.agents[1].activity = "approve Bash rm -rf dist".into();
        // It drops in: its lower edge first, over the title.
        let b = frame(&mut app);
        let (r, id) = app.hits.strip.expect("a strip");
        assert_eq!((id, r.y, r.x, r.right()), (2, title.y, title.x + 1, title.right() - 1), "over the title, inside its corners");
        assert_eq!(app.pane_size, size, "the pane keeps its size");
        assert!((r.x..r.right()).all(|x| b[(x, r.y)].symbol() == "▀" && b[(x, r.y)].fg == t.red), "{}", row(&b, r.y));
        assert!(app.next_frame_change().is_some_and(|n| n <= Instant::now() + Duration::from_millis(90)), "the next step is due");
        // Then the bar, flashing: the step for its age as the frame was
        // drawn, which a slow machine reaches a step later
        // (`the_strip_steps_by_its_age` has each step's color).
        let since = |ms| Instant::now().checked_sub(Duration::from_millis(ms)).unwrap();
        let shown = since(120);
        app.strip = Some((2, shown));
        let age = || shown.elapsed().as_millis() as u64;
        let before = age();
        let b = frame(&mut app);
        let (after, bg) = (age(), b[(r.x + 1, r.y)].bg);
        assert!((before..=after).any(|ms| strip_bg(t.red, ms) == Some(bg)), "{bg:?} is no step of {before}..={after} ms");
        // And settled.
        let shown = since(1000);
        app.strip = Some((2, shown));
        let b = frame(&mut app);
        let line = row(&b, r.y);
        println!("{}\n{}\n{}", row(&b, 0), line, row(&b, r.y + 1));
        assert!(line.contains("▲ api (2)  approve Bash rm -rf dist · ") && line.contains("alt+a  jump"), "{line}");
        assert_eq!((b[(r.x + 1, r.y)].bg, b[(r.x + 1, r.y)].fg), (t.red, t.bg), "the theme's red, dark text");
        // Nothing more for the strip to draw. (The waiting agent's ▲ still
        // blinks, on the app's own clock.)
        assert_eq!(next_strip_step(shown.elapsed()), None, "settled: no more steps");
        // A second agent waiting.
        app.agents[2].set_state(State::Blocked, false);
        assert!(row(&frame(&mut app), r.y).contains("+1 more"));
        // Its color is yours; the text stays readable.
        app.config.strip.color = Some(crate::config::Paint::Rgb(0x10, 0x20, 0x60));
        let b = frame(&mut app);
        assert_eq!((b[(r.x + 1, r.y)].bg, b[(r.x + 1, r.y)].fg), (Color::Rgb(0x10, 0x20, 0x60), t.fg_bright), "light text on a dark bar");
        app.config.strip.color = Some(crate::config::Paint::Theme("yellow"));
        app.config.strip.text = Some(crate::config::Paint::Rgb(0, 0, 0));
        let b = frame(&mut app);
        assert_eq!((b[(r.x + 1, r.y)].bg, b[(r.x + 1, r.y)].fg), (t.yellow, Color::Rgb(0, 0, 0)));
        // The grid has red tiles instead, and coming back drops it in again.
        app.view = View::Grid;
        frame(&mut app);
        assert!(app.hits.strip.is_none() && app.strip.is_none());
        // Looking at the agent that waits: no strip about it.
        app.view = View::Focus;
        app.agents[2].set_state(State::Idle, false);
        app.focus = 1;
        frame(&mut app);
        assert!(app.hits.strip.is_none());
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
        assert_eq!(sidebar_window(5, Some(3), 4, 10), 0);
        // Focus in view: the list stays put.
        assert_eq!(sidebar_window(2, Some(4), 20, 5), 2);
        // Focus below or above: just enough to show it.
        assert_eq!(sidebar_window(2, Some(9), 20, 5), 5);
        assert_eq!(sidebar_window(6, Some(1), 20, 5), 1);
        // Agents closed since: never past the end.
        assert_eq!(sidebar_window(18, Some(7), 8, 5), 3);
        assert_eq!(sidebar_window(3, Some(0), 20, 0), 0, "no room at all");
        // The focus elsewhere (on a pinned card): the list stays put.
        assert_eq!(sidebar_window(9, None, 20, 5), 9);
        assert_eq!(sidebar_window(18, None, 8, 5), 3);
    }

    #[test]
    fn pinned_cards_hold_still_while_the_rest_scroll() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app.sidebar_mini = false;
        app.sidebar_compact = false;
        let n = 40;
        for k in 0..n {
            let mut a = crate::agent::test_agent(crate::agent::Kind::Claude);
            a.id = k + 1;
            a.name = format!("agent-{}", k + 1);
            a.pinned = k < 2;
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        let mut frame = |app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>()
        };
        let ids = |app: &App| app.hits.cards.iter().map(|(_, id)| *id).collect::<Vec<u32>>();
        for compact in [false, true] {
            app.sidebar_compact = compact;
            app.focus = 0;
            app.sidebar_scroll = 0;
            frame(&mut app);
            let pins: Vec<Rect> = app.hits.cards[..2].iter().map(|(r, _)| *r).collect();
            assert_eq!(ids(&app)[..3], [1, 2, 3], "compact: {compact}");
            // The last agent: the rest scroll to it, the pinned ones stay.
            app.focus = n as usize - 1;
            let lines = frame(&mut app);
            let shown = ids(&app);
            assert_eq!((shown[0], shown[1], *shown.last().unwrap()), (1, 2, n), "compact: {compact}: {shown:?}");
            assert_eq!(app.hits.cards[..2].iter().map(|(r, _)| *r).collect::<Vec<_>>(), pins, "in the same place");
            // Right under them, what's scrolled out of view between.
            let hidden = n as usize - shown.len();
            let row = &lines[pins[1].bottom() as usize];
            assert!(row.contains(&format!("↑ {hidden} more")), "compact: {compact}:\n{}", lines.join("\n"));
            assert!(app.hits.cards[2].0.y > pins[1].bottom(), "the rest start below that row");
            // A pinned agent in focus: nothing moves.
            app.focus = 1;
            frame(&mut app);
            assert_eq!(ids(&app), shown, "compact: {compact}");
            // The first one after the pins: the rest scroll back up.
            app.focus = 2;
            let lines = frame(&mut app);
            assert_eq!(ids(&app)[..3], [1, 2, 3], "compact: {compact}");
            assert!(!lines.join("\n").contains('↑'));
        }
        for a in &app.agents {
            a.shutdown();
        }
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
        // Output redraws for exactly the agents on screen (see `on_grid_page`).
        for (n, focus) in [(10, 0), (10, 5), (10, 9), (3, 2), (8, 7)] {
            let shown: Vec<usize> = grid_tiles(n, focus, area).0.iter().map(|t| t.0).collect();
            assert_eq!(shown, (0..n).filter(|&i| on_grid_page(i, focus)).collect::<Vec<_>>(), "{n} agents, focus {focus}");
        }
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

    #[test]
    fn the_settings_and_the_theme_list_on_screen() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let text = |app: &mut App| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer();
            (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
        };
        app.config.notify.bell = false;
        app.open_settings();
        let shown = text(&mut app);
        for want in ["Settings", "LOOK", "Theme", "Your terminal's colors ›", "Sidebar  alt+m", "wide", "With the terminal bell", "off", "When one is done", "chime", "Keys, mutes, a notify command"] {
            assert!(shown.contains(want), "{want:?} in\n{shown}");
        }
        assert_eq!(app.hits.settings_rows.len(), Row::ALL.len(), "every row on screen, each one a button");
        if let Some(Modal::Settings(s)) = &mut app.modal {
            let (choices, current) = crate::theme::choices_here(std::path::Path::new("/nonexistent"));
            s.picker = Some(Picker { choices, sel: 2, current });
        }
        let shown = text(&mut app);
        for want in ["Theme", "Your terminal's colors  in use", "› Catppuccin Latte", "Everforest", "▼ more", " Aa ", "enter keep it"] {
            assert!(shown.contains(want), "{want:?} in\n{shown}");
        }
    }
}
