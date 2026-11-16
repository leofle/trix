//! Trixy, the mascot, in the sidebar's corner where "+ new agent" was: a
//! little animated character that shows what the workspace is doing, and
//! the button for a new agent. It's nine pixels by eight, drawn two pixels
//! to a cell with half blocks, so four rows of nine cells. Each mood is a
//! few frames put together from parts: where it stands, the bulb's color
//! and glow, the eyes, the feet, and whatever floats around it.

use crate::agent::State;
use crate::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::time::Duration;

/// The drawing, in pixels.
const W: usize = 9;
const H: usize = 8;
/// The cells it takes.
pub const COLS: u16 = W as u16;
pub const ROWS: u16 = (H / 2) as u16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Working,
    Loading,
    Hop,
    Walk,
    LookAround,
    Thinking,
    Sleep,
    Celebrate,
    NeedsYou,
    Signal,
}

#[derive(Clone, Copy, PartialEq)]
enum Pose {
    Stand,
    /// The antenna pulled in: the bulb a pixel lower.
    Dip,
    /// Off the ground by a pixel.
    Jump,
}

#[derive(Clone, Copy, PartialEq)]
enum Eyes {
    Open,
    Closed,
    Left,
    Right,
    Up,
}

#[derive(Clone, Copy, PartialEq)]
enum Feet {
    Both,
    Left,
    Right,
}

/// One picture of an animation. Colors are letters: `A`mber, `R`ed,
/// `G`reen, `C`yan, `D`im and `Z` (the z's of sleep), solid in capitals and
/// faint in small letters.
#[derive(Clone, Copy)]
struct Frame {
    pose: Pose,
    bulb: char,
    /// A glow left and right of the bulb.
    glow: (bool, bool),
    eyes: Eyes,
    feet: Feet,
    /// What floats around it: `(x, y, color)`.
    extra: &'static [(u8, u8, char)],
    ms: u64,
}

const fn f(ms: u64, bulb: char) -> Frame {
    Frame { pose: Pose::Stand, bulb, glow: (false, false), eyes: Eyes::Open, feet: Feet::Both, extra: &[], ms }
}

impl Frame {
    const fn pose(self, pose: Pose) -> Frame {
        Frame { pose, ..self }
    }
    const fn glow(self, left: bool, right: bool) -> Frame {
        Frame { glow: (left, right), ..self }
    }
    const fn eyes(self, eyes: Eyes) -> Frame {
        Frame { eyes, ..self }
    }
    const fn feet(self, feet: Feet) -> Frame {
        Frame { feet, ..self }
    }
    const fn extra(self, extra: &'static [(u8, u8, char)]) -> Frame {
        Frame { extra, ..self }
    }

    /// Its pixels as letters, `.` where there's nothing: `B`ody, its
    /// `H`ighlight, `E`yes (`-` closed), and the colors.
    fn pixels(&self) -> [[char; W]; H] {
        const BODY: [&[u8; W]; 5] = [b".BBBBBBB.", b"BHBBBBBBB", b"BBBBBBBBB", b"BBBBBBBBB", b".BBBBBBB."];
        let mut px = [['.'; W]; H];
        let top = if self.pose == Pose::Jump { 1 } else { 2 };
        let bulb = if self.pose == Pose::Dip { 1 } else { 0 };
        px[bulb][4] = self.bulb;
        if self.glow.0 {
            px[bulb][3] = self.bulb.to_ascii_lowercase();
        }
        if self.glow.1 {
            px[bulb][5] = self.bulb.to_ascii_lowercase();
        }
        if self.pose == Pose::Stand {
            px[1][4] = 'B';
        }
        for (r, row) in BODY.iter().enumerate() {
            for (c, &b) in row.iter().enumerate() {
                px[top + r][c] = b as char;
            }
        }
        let (row, col, eye) = match self.eyes {
            Eyes::Open => (2, 2, 'E'),
            Eyes::Closed => (2, 2, '-'),
            Eyes::Left => (2, 1, 'E'),
            Eyes::Right => (2, 3, 'E'),
            Eyes::Up => (1, 2, 'E'),
        };
        px[top + row][col] = eye;
        px[top + row][col + 4] = eye;
        if self.feet != Feet::Right {
            px[top + 5][2] = 'B';
        }
        if self.feet != Feet::Left {
            px[top + 5][6] = 'B';
        }
        for &(x, y, c) in self.extra {
            px[y as usize][x as usize] = c;
        }
        px
    }
}

/// A letter's color over `bg`. Without a `bg` to blend with (a theme in the
/// terminal's own colors) the faint ones are left out.
fn color(c: char, bg: Option<(u8, u8, u8)>) -> Option<(u8, u8, u8)> {
    const BODY: (u8, u8, u8) = (0x82, 0xa1, 0xf1);
    const EYE: (u8, u8, u8) = (0x14, 0x14, 0x1a);
    let l = |a: (u8, u8, u8), b: (u8, u8, u8), t: f32| {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
    };
    let solid = match c.to_ascii_uppercase() {
        'B' => BODY,
        'H' | 'Z' => (0xb7, 0xc9, 0xf7),
        'E' => EYE,
        '-' => l(BODY, EYE, 0.4),
        'A' => (0xe0, 0xaf, 0x68),
        'R' => (0xf7, 0x76, 0x8e),
        'G' => (0x9e, 0xce, 0x6a),
        'C' => (0x7d, 0xcf, 0xff),
        'D' => (0x56, 0x5f, 0x89),
        _ => return None,
    };
    if c.is_ascii_lowercase() { bg.map(|bg| l(bg, solid, 0.4)) } else { Some(solid) }
}

impl Mood {
    fn frames(self) -> &'static [Frame] {
        use Eyes::*;
        use Pose::*;
        match self {
            // Bobs with a pulsing amber light.
            Mood::Working => &const { [f(500, 'A').glow(true, true), f(300, 'A'), f(500, 'A').glow(true, true).pose(Dip), f(300, 'A').pose(Dip)] },
            // The light runs across the bulb, the eyes follow it.
            Mood::Loading => &const { [f(200, 'A').glow(true, false).eyes(Left), f(200, 'A'), f(200, 'A').glow(false, true).eyes(Right), f(200, 'A')] },
            Mood::Hop => &const { [f(350, 'A'), f(150, 'A').pose(Dip), f(250, 'A').pose(Jump), f(150, 'A').pose(Dip)] },
            Mood::Walk => &const { [f(200, 'A'), f(200, 'A').feet(Feet::Left), f(200, 'A'), f(200, 'A').feet(Feet::Right)] },
            Mood::LookAround => &const { [f(800, 'A'), f(600, 'A').eyes(Left), f(300, 'A'), f(600, 'A').eyes(Right), f(500, 'A'), f(150, 'A').eyes(Closed)] },
            // Looks up, thought dots appear.
            Mood::Thinking => &const { [f(500, 'A').eyes(Up), f(400, 'A').eyes(Up).extra(&[(6, 1, 'z')]), f(700, 'A').eyes(Up).extra(&[(6, 1, 'Z'), (8, 0, 'Z')]), f(300, 'A').eyes(Up).extra(&[(8, 0, 'z')])] },
            // Dozes, z's float up.
            Mood::Sleep => &const {
                [
                    f(700, 'D').eyes(Closed).pose(Dip),
                    f(500, 'D').eyes(Closed).pose(Dip).extra(&[(6, 1, 'z')]),
                    f(500, 'D').eyes(Closed).extra(&[(6, 1, 'Z'), (7, 0, 'z')]),
                    f(500, 'D').eyes(Closed).extra(&[(6, 1, 'z'), (7, 0, 'Z')]),
                    f(500, 'D').eyes(Closed).extra(&[(7, 0, 'z')]),
                ]
            },
            // A happy bounce with sparkles.
            Mood::Celebrate => &const {
                [
                    f(200, 'G').glow(true, true).pose(Jump).eyes(Up).extra(&[(0, 0, 'A'), (8, 0, 'C'), (0, 7, 'c'), (8, 7, 'a')]),
                    f(200, 'G').eyes(Up).extra(&[(0, 0, 'a'), (8, 1, 'c'), (7, 0, 'G')]),
                    f(200, 'G').glow(true, true).pose(Jump).eyes(Up).extra(&[(0, 0, 'C'), (8, 0, 'A'), (0, 7, 'a'), (8, 7, 'c')]),
                    f(200, 'G').eyes(Up).extra(&[(8, 0, 'a'), (0, 1, 'c'), (1, 0, 'G')]),
                ]
            },
            // Startled, the red light flashing.
            Mood::NeedsYou => &const { [f(300, 'R').glow(true, true).extra(&[(7, 0, 'R'), (7, 1, 'R')]), f(300, 'r').extra(&[(7, 0, 'r'), (7, 1, 'r')])] },
            // Waves go out from the antenna.
            Mood::Signal => &const {
                [
                    f(250, 'C'),
                    f(250, 'C').glow(true, true),
                    f(250, 'C').glow(true, true).extra(&[(2, 0, 'c'), (6, 0, 'c')]),
                    f(250, 'C').extra(&[(2, 0, 'c'), (6, 0, 'c'), (1, 0, 'c'), (7, 0, 'c')]),
                    f(200, 'C').extra(&[(1, 0, 'c'), (7, 0, 'c')]),
                ]
            },
        }
    }

    /// How long `loops` of it take.
    fn loops(self, loops: u64) -> Duration {
        Duration::from_millis(self.frames().iter().map(|f| f.ms).sum::<u64>() * loops)
    }

    /// The frame showing `t` into it, and how long until the next.
    fn at(self, t: Duration) -> (&'static Frame, Duration) {
        let frames = self.frames();
        let mut ms = t.as_millis() as u64 % self.loops(1).as_millis() as u64;
        for f in frames {
            if ms < f.ms {
                return (f, Duration::from_millis(f.ms - ms));
            }
            ms -= f.ms;
        }
        (&frames[0], Duration::from_millis(frames[0].ms))
    }
}

/// What Trixy is doing, and how far into it: the workspace's state, the most
/// pressing first.
pub fn mood(app: &App) -> (Mood, Duration) {
    let since_start = app.started.elapsed();
    let any = |s: State| app.agents.iter().any(|a| a.state == s);
    if any(State::Blocked) {
        return (Mood::NeedsYou, since_start);
    }
    // One just finished: a little party, from its start.
    let party = Mood::Celebrate.loops(4);
    if let Some(t) = app.agents.iter().filter(|a| a.state == State::Done).map(|a| a.state_since.elapsed()).filter(|&t| t < party).min() {
        return (Mood::Celebrate, t);
    }
    let mut working = app.agents.iter().filter(|a| a.state == State::Working).peekable();
    if working.peek().is_some() {
        // Thinking only while nobody has got to doing yet.
        let thinking = working.all(|a| a.activity.eq_ignore_ascii_case("thinking"));
        return (if thinking { Mood::Thinking } else { Mood::Working }, since_start);
    }
    if any(State::Starting) {
        return (Mood::Loading, since_start);
    }
    // Finished work nobody has looked at: calling you over.
    if app.agents.iter().any(|a| a.state == State::Done && a.unseen) {
        return (Mood::Signal, since_start);
    }
    // All quiet. A while of looking for something to do, then sleep.
    let quiet = app.agents.iter().map(|a| a.state_since).max().unwrap_or(app.started).elapsed();
    let awake: &[(Mood, u64)] = if app.agents.is_empty() { &[(Mood::Hop, 4), (Mood::LookAround, 2)] } else { &[(Mood::LookAround, 2), (Mood::Walk, 3), (Mood::LookAround, 2)] };
    let mut t = quiet;
    for &(m, loops) in awake {
        let d = m.loops(loops);
        if t < d {
            return (m, t);
        }
        t -= d;
    }
    (Mood::Sleep, t)
}

/// How long until Trixy's picture changes, for `App::next_frame_change`.
/// `None` when it holds still: asleep with the terminal out of focus, so a
/// trix nobody is looking at draws nothing.
pub fn next_change(app: &App) -> Option<Duration> {
    let (mood, t) = mood(app);
    if mood == Mood::Sleep && !app.term_focused {
        return None;
    }
    Some(mood.at(t).1)
}

/// Draw `mood`, `t` into it, with its top left corner at `area`'s, over
/// `bg`; `dim` draws it halfway to that color, for under a dialog.
pub fn draw(buf: &mut Buffer, area: Rect, mood: Mood, t: Duration, bg: Color, dim: Option<Color>) {
    let px = mood.at(t).0.pixels();
    let bg = match bg {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    };
    let rgb = |c: char| {
        let (r, g, b) = color(c, bg)?;
        Some(crate::theme::mix(Color::Rgb(r, g, b), dim.unwrap_or(Color::Reset), 0.5))
    };
    for (row, pair) in px.chunks(2).enumerate().take(area.height as usize) {
        for (col, (&top, &bottom)) in pair[0].iter().zip(&pair[1]).enumerate().take(area.width as usize) {
            let Some(cell) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) else { continue };
            match (rgb(top), rgb(bottom)) {
                (None, None) => {}
                (Some(top), None) => drop(cell.set_symbol("▀").set_fg(top)),
                (None, Some(bottom)) => drop(cell.set_symbol("▄").set_fg(bottom)),
                (Some(top), Some(bottom)) => drop(cell.set_symbol("▀").set_fg(top).set_bg(bottom)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Mood; 10] = [Mood::Working, Mood::Loading, Mood::Hop, Mood::Walk, Mood::LookAround, Mood::Thinking, Mood::Sleep, Mood::Celebrate, Mood::NeedsYou, Mood::Signal];

    #[test]
    fn every_frame_is_a_whole_trixy() {
        let text = |f: &Frame| f.pixels().map(|r| r.iter().collect::<String>()).join("\n");
        assert_eq!(text(&f(100, 'A')), "....A....\n....B....\n.BBBBBBB.\nBHBBBBBBB\nBBEBBBEBB\nBBBBBBBBB\n.BBBBBBB.\n..B...B..");
        assert_eq!(text(&f(100, 'R').pose(Pose::Jump).glow(true, true).eyes(Eyes::Up).feet(Feet::Left)), "...rRr...\n.BBBBBBB.\nBHEBBBEBB\nBBBBBBBBB\nBBBBBBBBB\n.BBBBBBB.\n..B......\n.........");
        for m in ALL {
            assert!(m.frames().len() >= 2, "{m:?} moves");
            for fr in m.frames() {
                let px = fr.pixels();
                assert_eq!(px.iter().flatten().filter(|&&c| c == 'E' || c == '-').count(), 2, "{m:?}: two eyes, nothing floating over them");
                assert!(px.iter().flatten().all(|&c| c == '.' || color(c, None).is_some() || c.is_ascii_lowercase()), "{m:?}: every letter is a color");
            }
        }
    }

    #[test]
    fn frames_follow_the_clock_and_loop() {
        let ms = Duration::from_millis;
        let frame = |t| Mood::Hop.frames().iter().position(|f| std::ptr::eq(f, Mood::Hop.at(ms(t)).0)).unwrap();
        assert_eq!((frame(0), frame(349), frame(350), frame(899), frame(900)), (0, 0, 1, 3, 0));
        assert_eq!(Mood::Hop.at(ms(300)).1, ms(50), "the next frame is due");
        assert_eq!(Mood::Hop.at(ms(890)).1, ms(10), "the last one lasts to the loop's end");
    }

    #[test]
    fn the_mood_is_the_most_pressing_thing_going_on() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let ago = |s: u64| std::time::Instant::now().checked_sub(Duration::from_secs(s)).unwrap();
        // Nobody here: hopping by the button, then nodding off.
        assert_eq!(mood(&app).0, Mood::Hop);
        app.started = ago(600);
        assert_eq!(mood(&app).0, Mood::Sleep);
        for _ in 0..2 {
            app.agents.push(crate::agent::test_agent(crate::agent::Kind::Claude));
        }
        let set = |app: &mut App, i: usize, s: State| app.agents[i].set_state(s, false);
        set(&mut app, 0, State::Idle);
        set(&mut app, 1, State::Starting);
        assert_eq!(mood(&app).0, Mood::Loading);
        set(&mut app, 1, State::Working);
        app.agents[1].activity = "thinking".into();
        assert_eq!(mood(&app).0, Mood::Thinking);
        app.agents[1].activity = "Bash cargo test".into();
        assert_eq!(mood(&app).0, Mood::Working);
        set(&mut app, 0, State::Blocked);
        assert_eq!(mood(&app).0, Mood::NeedsYou, "over the one working");
        set(&mut app, 0, State::Idle);
        // Done: a party, then a signal until it's been looked at.
        set(&mut app, 1, State::Done);
        assert_eq!(mood(&app).0, Mood::Celebrate);
        app.agents[1].state_since = ago(4);
        assert_eq!(mood(&app).0, Mood::Signal);
        app.agents[1].unseen = false;
        app.agents[0].state_since = ago(4);
        assert_eq!(mood(&app).0, Mood::LookAround, "quiet, still up");
        app.agents[1].state_since = ago(7);
        app.agents[0].state_since = ago(7);
        assert_eq!(mood(&app).0, Mood::Walk);
        app.agents[1].state_since = ago(60);
        app.agents[0].state_since = ago(60);
        assert_eq!(mood(&app).0, Mood::Sleep);
        // Asleep, it only moves while someone may be looking.
        app.term_focused = true;
        assert!(next_change(&app).is_some_and(|d| d <= Duration::from_millis(700)));
        app.term_focused = false;
        assert_eq!(next_change(&app), None);
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn drawn_two_pixels_to_a_cell() {
        let area = Rect::new(2, 1, COLS, ROWS);
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 6));
        draw(&mut buf, area, Mood::NeedsYou, Duration::ZERO, Color::Rgb(0, 0, 0), None);
        let body = Color::Rgb(0x82, 0xa1, 0xf1);
        let at = |x: u16, y: u16| {
            let c = buf.cell((x, y)).unwrap();
            (c.symbol().to_string(), c.fg, c.bg)
        };
        assert_eq!(at(6, 1), ("▀".into(), Color::Rgb(0xf7, 0x76, 0x8e), body), "the bulb on its stem");
        // Its glow is the red over the background, two parts in five.
        assert_eq!((at(5, 1).0.as_str(), at(5, 1).1), ("▀", Color::Rgb(99, 47, 57)));
        assert_eq!(at(3, 3), ("▀".into(), body, body), "the body, both halves");
        assert_eq!(at(2, 2).0, "▄", "its rounded corner");
        assert_eq!(at(1, 3).0, " ", "nothing beside it");
        // The terminal's own colors: nothing to blend a glow with.
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 6));
        draw(&mut buf, area, Mood::NeedsYou, Duration::ZERO, Color::Reset, None);
        assert_eq!(buf.cell((5, 1)).unwrap().symbol(), " ");
        // Under a dialog: halfway to the background.
        draw(&mut buf, area, Mood::NeedsYou, Duration::ZERO, Color::Rgb(0, 0, 0), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(buf.cell((3, 3)).unwrap().fg, Color::Rgb(0x41, 0x51, 0x79));
    }
}
