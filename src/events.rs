//! `trix events`: what the agents do, live, one JSON object per line.
//!
//! Once per turn of its loop, the server sends what changed: each agent's
//! state changes as they happened (two in one turn both show), and how its
//! name and the list of agents differ from what it last published, so every
//! change shows, however it came about (a hook, the screen, an exit, a
//! rename, `trix close`). Each subscriber has its own writer thread and a
//! bounded queue: one that stops reading never stalls the workspace. It is
//! dropped instead, once its queue is full, and told it lost events.
//!
//! Events: `present` (each agent there when you subscribe), `spawned`,
//! `state`, `renamed`, `restarted` (a new process, and id, in the same
//! place: `prev_id` is the old one), `closed`, and `stopped` when the
//! workspace stops, or `lost` if the subscriber fell behind.

use std::io::IsTerminal;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::{Agent, State};
use crate::app::App;
use crate::control::Reply;
use crate::session::{self, ClientMsg, TEXT};

/// Events queued for one subscriber before it counts as lost.
const QUEUE: usize = 1024;

pub struct Subscriber {
    tx: SyncSender<String>,
    /// Only these agents; `None`: all of them, new ones too.
    only: Option<Vec<u32>>,
    /// Our side of the connection, to notice the client leaving.
    peer: UnixStream,
    lost: Arc<AtomicBool>,
    writer: JoinHandle<()>,
}

/// What subscribers last heard about an agent.
pub struct Published {
    name: String,
    kind: &'static str,
    state: State,
    /// Restarted under a new id since: the old one, to say so.
    prev_id: Option<u32>,
}

/// The names `trix ls --json` uses.
pub fn state_label(s: State) -> &'static str {
    match s {
        State::Blocked => "needs-you",
        s => s.label(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// One event about an agent that is open.
pub fn agent_event(event: &str, a: &Agent, number: usize, prev: Option<State>) -> Value {
    let mut v = json!({
        "event": event,
        "at": now_ms(),
        "session": session::name(),
        "id": a.id,
        "number": number,
        "name": a.name,
        "kind": a.kind.label(),
        "state": state_label(a.state),
        "activity": a.activity,
        "unseen": a.unseen,
        "cwd": a.cwd.display().to_string(),
        "managed_by": a.managed_by,
    });
    if let Some(p) = prev {
        v["prev"] = json!(state_label(p));
    }
    v
}

/// `trix events [agent…]`: answer, say who's there, keep the connection.
pub fn subscribe(app: &mut App, mut stream: UnixStream, targets: &[String]) {
    let only = if targets.is_empty() {
        None
    } else {
        let mut ids = Vec::new();
        for t in targets {
            match crate::control::resolve(app.agents.iter().map(|a| a.name.as_str()), t) {
                Ok(i) => ids.push(app.agents[i].id),
                Err(e) => return crate::control::reply(&mut stream, &Reply { code: 1, err: e, ..Default::default() }),
            }
        }
        Some(ids)
    };
    // What's pending goes to the others first: this one starts from now.
    publish(app);
    if app.subscribers.is_empty() {
        app.published = app.agents.iter().map(|a| (a.id, published(a))).collect();
    }
    let Ok(peer) = stream.try_clone() else { return };
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(QUEUE);
    let lost = Arc::new(AtomicBool::new(false));
    let lost_w = lost.clone();
    // No write timeout: a reader that pauses (ctrl+z, a stalled pipe) gets
    // everything once it reads again, and a cut-off frame never. Only a full
    // queue drops it, and then it hears so, after what it was sent.
    let writer = std::thread::spawn(move || {
        let ack = serde_json::to_vec(&Reply::ok("")).unwrap_or_default();
        if session::write_frame(&mut stream, TEXT, &ack).is_err() {
            return;
        }
        while let Ok(line) = rx.recv() {
            if session::write_frame(&mut stream, TEXT, line.as_bytes()).is_err() {
                return;
            }
        }
        if lost_w.load(Ordering::Relaxed) {
            let _ = session::write_frame(&mut stream, TEXT, json!({ "event": "lost", "at": now_ms() }).to_string().as_bytes());
        }
    });
    for (i, a) in app.agents.iter().enumerate() {
        if only.as_ref().is_none_or(|ids| ids.contains(&a.id)) {
            let _ = tx.try_send(agent_event("present", a, i + 1, None).to_string());
        }
    }
    app.subscribers.push(Subscriber { tx, only, peer, lost, writer });
}

fn published(a: &Agent) -> Published {
    Published { name: a.name.clone(), kind: a.kind.label(), state: a.state, prev_id: None }
}

/// `old` was restarted as `new`, in the same place: subscribers following
/// it follow the new process, and hear `restarted`, not `closed`.
pub fn restarted(app: &mut App, old: u32, new: u32) {
    if let Some(mut p) = app.published.remove(&old) {
        p.prev_id = Some(old);
        app.published.insert(new, p);
    }
    for s in &mut app.subscribers {
        if let Some(ids) = &mut s.only {
            for id in ids.iter_mut().filter(|id| **id == old) {
                *id = new;
            }
        }
    }
}

/// Tell subscribers what changed since the last call. Cheap when nothing
/// did, and free without subscribers.
pub fn publish(app: &mut App) {
    if app.subscribers.is_empty() {
        app.published.clear();
        for a in &mut app.agents {
            a.changes.clear();
        }
        return;
    }
    let mut events: Vec<(u32, String)> = Vec::new();
    // The ones gone first: a name closed and opened again in the same turn
    // reads in that order.
    let gone: Vec<u32> = app.published.keys().copied().filter(|id| !app.agents.iter().any(|a| a.id == *id)).collect();
    for id in gone {
        if let Some(p) = app.published.remove(&id) {
            let v = json!({ "event": "closed", "at": now_ms(), "session": session::name(), "id": id, "name": p.name, "kind": p.kind, "state": state_label(p.state) });
            events.push((id, v.to_string()));
        }
    }
    for (i, a) in app.agents.iter_mut().enumerate() {
        let changes = std::mem::take(&mut a.changes);
        let a = &*a;
        match app.published.get_mut(&a.id) {
            None => {
                events.push((a.id, agent_event("spawned", a, i + 1, None).to_string()));
                app.published.insert(a.id, published(a));
            }
            Some(p) if p.prev_id.is_some() => {
                let mut v = agent_event("restarted", a, i + 1, Some(p.state));
                v["prev_id"] = json!(p.prev_id.take());
                events.push((a.id, v.to_string()));
                (p.name, p.state) = (a.name.clone(), a.state);
            }
            Some(p) => {
                if p.name != a.name {
                    let mut v = agent_event("renamed", a, i + 1, None);
                    v["prev_name"] = json!(p.name);
                    events.push((a.id, v.to_string()));
                    p.name = a.name.clone();
                }
                // Each change as it happened; the last with what it does now.
                let n = changes.len();
                for (k, (from, to, activity)) in changes.into_iter().enumerate() {
                    let mut v = agent_event("state", a, i + 1, Some(from));
                    v["state"] = json!(state_label(to));
                    if k + 1 < n {
                        v["activity"] = json!(activity);
                    }
                    events.push((a.id, v.to_string()));
                    p.state = to;
                }
                if p.state != a.state {
                    events.push((a.id, agent_event("state", a, i + 1, Some(p.state)).to_string()));
                    p.state = a.state;
                }
            }
        }
    }
    if events.is_empty() {
        return;
    }
    app.subscribers.retain(|s| {
        for (id, line) in &events {
            if s.only.as_ref().is_some_and(|ids| !ids.contains(id)) {
                continue;
            }
            match s.tx.try_send(line.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    s.lost.store(true, Ordering::Relaxed);
                    return false;
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        true
    });
}

/// Forget subscribers whose client left. Once a second is plenty.
pub fn prune(app: &mut App) {
    app.subscribers.retain(|s| !crate::control::hung_up(&s.peer) && !s.writer.is_finished());
}

/// The workspace is stopping: say so, and give the writers a moment.
pub fn stop_all(app: &mut App) {
    let subs = std::mem::take(&mut app.subscribers);
    let line = json!({ "event": "stopped", "at": now_ms(), "session": session::name() }).to_string();
    let writers: Vec<JoinHandle<()>> = subs
        .into_iter()
        .map(|s| {
            let _ = s.tx.try_send(line.clone());
            s.writer
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(1);
    while writers.iter().any(|w| !w.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ───────────────────────────── client side ─────────────────────────────

/// `trix events [agent…] [--json]`: print events until the workspace stops.
pub fn run(targets: Vec<String>, as_json: bool) -> ! {
    let msg = ClientMsg::Control { protocol: crate::control::CONTROL, from: crate::control::caller(), req: crate::control::Request::Events { targets } };
    let mut s = match session::open(&msg) {
        Ok(s) => s,
        Err(e) => fail(&e, 1),
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    match session::read_frame(&mut s) {
        Ok((TEXT, p)) => match serde_json::from_slice::<Reply>(&p) {
            Ok(r) if r.code == 0 => {}
            Ok(r) => fail(&r.err, r.code),
            Err(e) => fail(&format!("bad reply from the workspace: {e}"), 1),
        },
        _ => fail(
            &format!("the running workspace is from an older trix that doesn't know this command — restart it (alt+q, then `{}`) to use it", session::command()),
            1,
        ),
    }
    let _ = s.set_read_timeout(None);
    let color = std::io::stdout().is_terminal();
    loop {
        let Ok((TEXT, p)) = session::read_frame(&mut s) else { fail("the workspace went away", 1) };
        let Ok(v) = serde_json::from_slice::<Value>(&p) else { continue };
        let kind = v["event"].as_str().unwrap_or_default();
        if as_json {
            print_line(&v.to_string());
        } else if let Some(line) = human(&v, color) {
            print_line(&line);
        }
        match kind {
            "stopped" => std::process::exit(0),
            "lost" => fail("fell behind and missed events; run it again to start from where things are now", 1),
            _ => {}
        }
    }
}

/// One line out. Whoever reads it gone (`trix events | head -1`) is how a
/// pipe ends, not an error.
fn print_line(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        fail(&format!("writing out: {e}"), 1);
    }
}

fn fail(e: &str, code: i32) -> ! {
    eprintln!("trix: {e}");
    std::process::exit(code)
}

/// `10:21:03  web  working → done  finished after 2m10s`
fn human(v: &Value, color: bool) -> Option<String> {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let time = local_time(v["at"].as_u64().unwrap_or_default() / 1000);
    let name = s("name");
    let what = match s("event").as_str() {
        "present" => s("state"),
        "spawned" => format!("new {} · {}", s("kind"), s("state")),
        "state" => format!("{} → {}", s("prev"), s("state")),
        "renamed" => format!("renamed from {}", s("prev_name")),
        "restarted" => format!("restarted · {}", s("state")),
        "closed" => "closed".into(),
        "stopped" => return Some(format!("{time}  the workspace stopped")),
        _ => return None,
    };
    let activity = if matches!(s("event").as_str(), "closed" | "renamed") { String::new() } else { s("activity") };
    let what = if color {
        let c = match s("state").as_str() {
            "needs-you" => "31",
            "working" => "33",
            "done" => "32",
            _ => "2",
        };
        format!("\x1b[{c}m{what:<20}\x1b[0m")
    } else {
        format!("{what:<20}")
    };
    Some(format!("{time}  {name:<14} {what}  {activity}").trim_end().to_string())
}

fn local_time(secs: u64) -> String {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".into();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Kind, test_agent};

    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app
    }

    /// A subscriber on one end of a socket pair; the other end reads frames.
    fn listen(app: &mut App, targets: &[&str]) -> UnixStream {
        let (ours, theirs) = UnixStream::pair().unwrap();
        subscribe(app, ours, &targets.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let _ = theirs.set_read_timeout(Some(Duration::from_secs(5)));
        theirs
    }

    fn next(s: &mut UnixStream) -> Value {
        let (_, p) = session::read_frame(s).expect("a frame");
        serde_json::from_slice(&p).unwrap()
    }

    fn add(app: &mut App, kind: Kind, id: u32, name: &str) {
        let mut a = test_agent(kind);
        a.id = id;
        a.name = name.into();
        app.agents.push(a);
    }

    #[test]
    fn changes_reach_subscribers_in_order() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        app.agents[0].set_state(State::Idle, false);
        let mut c = listen(&mut app, &[]);
        assert_eq!(next(&mut c)["code"], 0, "the answer comes first");
        let v = next(&mut c);
        assert_eq!((v["event"].as_str(), v["name"].as_str(), v["state"].as_str(), v["number"].as_u64()), (Some("present"), Some("web"), Some("idle"), Some(1)));

        app.agents[0].set_state(State::Working, false);
        app.agents[0].activity = "Bash cargo test".into();
        publish(&mut app);
        app.agents[0].set_state(State::Blocked, false);
        publish(&mut app);
        publish(&mut app);
        let v = next(&mut c);
        assert_eq!((v["event"].as_str(), v["prev"].as_str(), v["state"].as_str(), v["activity"].as_str()), (Some("state"), Some("idle"), Some("working"), Some("Bash cargo test")));
        let v = next(&mut c);
        assert_eq!((v["prev"].as_str(), v["state"].as_str()), (Some("working"), Some("needs-you")));

        add(&mut app, Kind::Codex, 2, "api");
        app.agents[0].name = "frontend".into();
        publish(&mut app);
        let v = next(&mut c);
        assert_eq!((v["event"].as_str(), v["prev_name"].as_str(), v["name"].as_str()), (Some("renamed"), Some("web"), Some("frontend")));
        let v = next(&mut c);
        assert_eq!((v["event"].as_str(), v["name"].as_str(), v["kind"].as_str()), (Some("spawned"), Some("api"), Some("codex")));

        let a = app.agents.remove(1);
        a.shutdown();
        publish(&mut app);
        let v = next(&mut c);
        assert_eq!((v["event"].as_str(), v["name"].as_str(), v["id"].as_u64()), (Some("closed"), Some("api"), Some(2)));

        stop_all(&mut app);
        assert_eq!(next(&mut c)["event"], "stopped");
        assert!(session::read_frame(&mut c).is_err(), "then the connection closes");
        app.agents[0].shutdown();
    }

    #[test]
    fn a_subscriber_can_follow_some_agents() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        add(&mut app, Kind::Claude, 2, "api");
        let mut c = listen(&mut app, &["api"]);
        next(&mut c);
        assert_eq!(next(&mut c)["name"], "api");
        app.agents[0].set_state(State::Working, false);
        app.agents[1].set_state(State::Done, false);
        add(&mut app, Kind::Shell, 3, "sh");
        publish(&mut app);
        let v = next(&mut c);
        assert_eq!((v["name"].as_str(), v["state"].as_str()), (Some("api"), Some("done")), "not web, not the new shell");
        let mut bad = listen(&mut app, &["nope"]);
        let r: Reply = serde_json::from_value(next(&mut bad)).unwrap();
        assert!(r.code == 1 && r.err.contains("no agent 'nope'"), "{r:?}");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn a_subscriber_that_stops_reading_is_dropped_not_waited_for() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        let mut c = listen(&mut app, &[]);
        let start = Instant::now();
        // Far more than the socket buffer and the queue hold, unread.
        for i in 0..20_000 {
            app.agents[0].set_state(if i % 2 == 0 { State::Working } else { State::Idle }, false);
            publish(&mut app);
        }
        assert!(start.elapsed() < Duration::from_secs(2), "publishing never blocks: {:?}", start.elapsed());
        assert!(app.subscribers.is_empty(), "dropped once its queue filled");
        // It hears why, after what it was sent.
        let mut last = Value::Null;
        while let Ok((_, p)) = session::read_frame(&mut c) {
            last = serde_json::from_slice(&p).unwrap();
        }
        assert_eq!(last["event"], "lost");
        app.agents[0].shutdown();
    }

    #[test]
    fn a_restart_reads_as_one_and_followers_keep_following() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        let mut all = listen(&mut app, &[]);
        let mut web = listen(&mut app, &["web"]);
        for c in [&mut all, &mut web] {
            next(c);
            next(c);
        }
        app.agents[0].set_state(State::Exited, false);
        publish(&mut app);
        // Restarted: a new process and id, in the same place.
        let old = app.agents.remove(0);
        old.shutdown();
        add(&mut app, Kind::Claude, 2, "web");
        restarted(&mut app, 1, 2);
        publish(&mut app);
        app.agents[0].set_state(State::Working, false);
        publish(&mut app);
        for c in [&mut all, &mut web] {
            assert_eq!(next(c)["state"], "exited");
            let v = next(c);
            assert_eq!((v["event"].as_str(), v["id"].as_u64(), v["prev_id"].as_u64(), v["prev"].as_str()), (Some("restarted"), Some(2), Some(1), Some("exited")));
            let v = next(c);
            assert_eq!((v["event"].as_str(), v["id"].as_u64(), v["state"].as_str()), (Some("state"), Some(2), Some("working")), "following the new process");
        }
        let line = human(&json!({"event":"restarted","at":0,"name":"web","state":"starting","activity":""}), false).unwrap();
        assert!(line.ends_with("web            restarted · starting"), "{line}");
        app.agents[0].shutdown();
    }

    #[test]
    fn closed_comes_before_a_new_agent_of_the_same_name() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        let mut c = listen(&mut app, &[]);
        next(&mut c);
        next(&mut c);
        app.agents.remove(0).shutdown();
        add(&mut app, Kind::Claude, 2, "web");
        publish(&mut app);
        assert_eq!((next(&mut c)["event"].as_str(), next(&mut c)["event"].as_str()), (Some("closed"), Some("spawned")));
        app.agents[0].shutdown();
    }

    #[test]
    fn every_change_shows_even_two_in_one_turn() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        app.agents[0].set_state(State::Working, false);
        let mut c = listen(&mut app, &[]);
        next(&mut c);
        next(&mut c);
        // Done and straight back to work, between two turns of the loop.
        app.agents[0].activity = "finished after 12s".into();
        app.agents[0].set_state(State::Done, false);
        app.agents[0].activity = "Bash ls".into();
        app.agents[0].set_state(State::Working, false);
        publish(&mut app);
        let v = next(&mut c);
        assert_eq!((v["prev"].as_str(), v["state"].as_str(), v["activity"].as_str()), (Some("working"), Some("done"), Some("finished after 12s")));
        let v = next(&mut c);
        assert_eq!((v["prev"].as_str(), v["state"].as_str(), v["activity"].as_str()), (Some("done"), Some("working"), Some("Bash ls")));
        // Without subscribers, nothing piles up.
        drop(c);
        std::thread::sleep(Duration::from_millis(50));
        prune(&mut app);
        for _ in 0..200 {
            app.agents[0].set_state(State::Idle, false);
            app.agents[0].set_state(State::Working, false);
        }
        assert!(app.agents[0].changes.len() <= 64);
        publish(&mut app);
        assert!(app.agents[0].changes.is_empty());
        app.agents[0].shutdown();
    }

    #[test]
    fn a_paused_reader_gets_everything_when_it_reads_again() {
        let mut app = app();
        add(&mut app, Kind::Claude, 1, "web");
        let mut c = listen(&mut app, &[]);
        // More than the socket holds, fewer than the queue: the writer waits.
        for i in 0..300 {
            app.agents[0].set_state(if i % 2 == 0 { State::Working } else { State::Idle }, false);
            publish(&mut app);
        }
        assert_eq!(app.subscribers.len(), 1, "still subscribed");
        std::thread::sleep(Duration::from_millis(200));
        let mut n = 0;
        let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
        while let Ok((_, p)) = session::read_frame(&mut c) {
            let v: Value = serde_json::from_slice(&p).unwrap();
            if v["event"] == "state" {
                n += 1;
            }
        }
        assert_eq!(n, 300, "none lost, none cut off");
        app.agents[0].shutdown();
    }

    #[test]
    fn a_client_that_leaves_is_pruned() {
        let mut app = app();
        let c = listen(&mut app, &[]);
        assert_eq!(app.subscribers.len(), 1);
        drop(c);
        std::thread::sleep(Duration::from_millis(50));
        prune(&mut app);
        assert!(app.subscribers.is_empty());
    }

    #[test]
    fn human_lines() {
        let v = json!({"event":"state","at":0,"name":"web","prev":"working","state":"done","activity":"finished after 2m10s"});
        let l = human(&v, false).unwrap();
        assert!(l.ends_with("web            working → done        finished after 2m10s"), "{l}");
        let v = json!({"event":"closed","at":0,"name":"api","state":"idle","activity":"x"});
        assert!(human(&v, false).unwrap().ends_with("api            closed"));
        assert_eq!(human(&json!({"event":"mystery"}), false), None);
    }
}
