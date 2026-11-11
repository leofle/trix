# trix

A workspace where coding agents live. Run Claude Code, Codex, shells, or any CLI agent side by side. trix tells you, at a glance and without asking, which agent is working, which one needs you, and which one finished while you were looking elsewhere.

```
◆ trix   ▲ 1 needs you   ⠹ 2 working   ✓ 1 done                              focus · 14:02
                                     ▲ api (2) approve Bash rm -rf dist        alt+a  jump
 AGENTS  4                         ╭─ ⠹ web claude · Edit src/app.tsx ──── ~/code/web  main +42 −7 ╮
                                   │                                                              │
  1  ⠹ web                 2m14s   │   (live agent terminal)                                      │
       Edit src/app.tsx            │                                                              │
       claude ▂▃▅▇▅▃▂▁▂▅  main     │                                                              │
                                   │                                                              │
  2  ▲ api                   12s   │                                                              │
       approve Bash rm -rf dist    │                                                              │
       claude ▁▁▁▂▅▁▁▁▁▁  trix/api │                                                              │
```

## Why

Generic multiplexers (tmux, zellij) don't know anything about agents. herdr and Rex added agent awareness, but they still treat the agent as just another pane. In trix, the agent comes first:

- **Hooks first, guessing second.** trix starts Claude Code with `--settings <overlay>` and Codex with `-c` hook overrides, so the agent reports its own lifecycle: prompt submitted, each tool call, permission request, question, turn finished, interrupted. Your own agent configs are never touched. (Codex asks you to trust trix's hooks once; see [Codex](#codex).) Agents without hooks fall back to output cadence and screen heuristics.
- **Feedback you can't miss.** Each agent shows a live state glyph, what it is doing right now (`Edit app.rs`, `Bash cargo test`, `approve …`), time in state, a 48-second output sparkline, git branch, and the diff it has made so far. When an agent you're *not* looking at needs you, a red strip appears over your current pane. You also get a desktop notification, and the terminal bell sets the urgency hint when trix is unfocused. When an agent you’re not looking at finishes, a short rising chime plays (through `pw-play` from PipeWire on Linux, `afplay` on macOS). Completions within two seconds share one chime. The terminal title tracks the count too.
- **One key to the next thing that matters.** `alt+a` goes to the agent that has been blocked longest, then to unseen finished work.
- **Many agents, one glance.** `alt+g` tiles every agent live. The focused tile is accented, blocked tiles turn red, and the rest fade back a little.
- **Isolation built in.** "Worktree" in the new-agent dialog creates `.trix/worktrees/<name>` on branch `trix/<name>`. It is excluded from git automatically, so parallel agents never step on each other.
- **Search every agent at once.** `alt+f` searches every agent's scrollback as you type, newest first, with each hit's agent and how far back it is. Enter jumps there: it focuses that agent, scrolls to the line and selects the match.
- **Broadcast.** `alt+b` sends one prompt to every agent, or only to the ones that are idle or done. Shells and agents sitting on a prompt are skipped, so a broadcast can never answer a permission dialog.
- **Agents outlive the terminal.** The workspace runs in a background server. Closing the window, losing SSH or pressing `alt+d` only detaches; agents keep working and still notify you on the desktop. Run `trix` again from any terminal to pick up where you were, including scrollback. `trix ls` shows who needs you without attaching.
- **Agents can run agents.** `trix spawn`, `send`, `read`, `wait` and `close` let a script, or an agent in another pane, work the workspace the way you do. A "lead" Claude can start workers in their own worktrees, hand them tasks, wait for them and read what they did. It never answers a permission prompt for you, and the sidebar shows which agent it is driving. See [Agent control](#agent-control).
- **Watch without taking over.** `trix view` attaches read-only from any number of other terminals: a second monitor, a laptop over SSH, a teammate on the same box. Viewers see exactly what the controlling terminal sees, and nothing they type reaches an agent.
- **Named sessions.** `trix -s work` and `trix -s home` are separate workspaces, each with its own agents, its own saved layout and its own attached terminal. `trix ls` shows all of them at once.
- **Survives restarts too.** The workspace is saved continuously. After `trix kill`, alt+q, a crash or a reboot, the next `trix` offers to bring it back: Claude and Codex resume the exact conversation each pane was in (`claude --resume <id>`, `codex resume <id>`), and shells reopen in the folder they were in.
- **Fast.** Rust, with `alacritty_terminal` as the VT engine (the same one Zed uses) and `ratatui`. Rendering is event-driven and capped at about 120 fps, and trix does nothing when nothing changes. Idle cost is about 0.4% of one core and about 10 MB of RAM. `seq 1 2000000` streams through in under a second.
- **Follows your theme.** trix reads Omarchy's `colors.toml` and restyles live when you switch themes. Agent terminals keep your terminal's own ANSI palette. Without Omarchy it falls back to Tokyo Night, or you can point `TRIX_THEME` at a colors.toml.

## Use

```sh
cargo install --path .
trix                      # open the workspace (or attach to the running one)
trix claude codex shell   # …and start these agents in the current folder
trix attach               # attach only; fails if nothing is running
trix view                 # watch read-only from another terminal (q leaves)
trix ls                   # agents, states and activity, without attaching
trix ls --json            # the same, for scripts and agents
trix kill                 # stop the workspace and every agent in it

trix -s work              # the same, for a named session (works with every command)
trix kill --all           # stop every running session
```

`trix` starts a background server on first use (`$XDG_RUNTIME_DIR/trix/session.sock`, log in `server.log` next to it) and attaches to it. One terminal is in control at a time: attaching from a second one takes over from the first. To look without taking over, use `trix view`. The server keeps the environment of the terminal that started it, so agents launched later inherit that `PATH`.

### Search

`alt+f` opens a search over every agent's scrollback, including exited ones. The last 10,000 lines of each are kept.

- Results update as you type, one row per matching line, newest first. They're grouped by agent, in sidebar order. Each row shows how far back the line is (`↑340`, `↑2.1k`, `on screen`). An agent shows at most 200 lines; a `+` after the count means some were left out.
- Smart case: a lowercase query ignores case, and any capital makes the match exact.
- Lines that wrapped on screen are searched as one line, so a match that wraps across rows is still found.
- `↑` `↓` (or `tab`, `ctrl+n` `ctrl+p`) select a result, `pgup` `pgdn` move by ten, `enter` jumps, `esc` closes.
- Jumping focuses the agent, scrolls the line into view and selects the match. New output since the search is accounted for. If the line has already fallen off the scrollback, you're told so. Typing into the agent returns it to the live view.
- Full-screen programs (vim, htop) only have their current screen to search.

### Agent control

Six commands drive a running workspace from any shell. They're meant for scripts and for agents managing other agents:

```sh
trix spawn claude --name api --worktree --prompt "add rate limiting to /login"
trix send api "also cover it with a test"     # or pipe the prompt in on stdin
trix wait api                                 # blocks until api stops working
trix read api -n 80                           # the last 80 lines of its screen and scrollback
trix ls --json                                # every agent: state, activity, branch, diff, last prompt…
trix close api
```

- `<agent>` is an agent's name, or its number in `trix ls`. Names are safer, because numbers shift when an agent closes. `spawn` prints the new agent's name and folder. It refuses a name that's already taken, and it never steals focus from the agent you're looking at.
- `wait` takes one or more agents and returns when one of them stops working (`--all`: when every one has). It prints one `name<TAB>state<TAB>activity` line per agent, where the state is `done`, `idle`, `needs-you` or `exited`. A prompt just sent with `send` counts as work until the agent's turn starts, so `send` followed by `wait` waits for that turn. After `-t` seconds (default 300, `0` waits forever) it exits with 124, and you can simply run it again. A shell counts as done when it's back at its prompt. Builtins and shell functions run inside the shell itself, so `wait` can't see them.
- Exit codes: 0 ok, 1 refused or failed (the reason goes to stderr), 2 bad usage, 124 `wait` timed out.

Guard rails, enforced by the server:

- `send` refuses an agent that **needs you**, because its Enter would answer that prompt, for example approve a `rm -rf`. It also refuses a **shell** (the prompt would run as a command) and an agent still **starting** (it may be on a trust dialog). `--force` overrides the first two. Answering prompts is left to you on purpose.
- An agent can't `send` to, `wait` for or `close` itself. trix knows which pane is asking from its `TRIX_AGENT_ID`.
- `close` won't stop an agent that is working or needs you without `--force`.
- Every `send` shows a toast (`lead → api: also cover it…`). Agents that another agent started or prompted show `‹lead` next to their name in the sidebar.

To make a manager, start a Claude pane and tell it what it's for. For example:

> You're the lead for this repo. Don't edit code yourself. Split the task into independent pieces. For each piece, start a worker with `trix spawn claude --worktree --name <piece> --prompt "<brief>"`. Then loop: `trix wait <all workers> -t 280`, `trix read <worker>` to check its work, and `trix send` follow-ups. If a worker shows `needs-you`, tell me which one and why, and leave it alone. When they're all done, summarise each worktree's branch and diff (`trix ls --json`).

Inside a pane, these commands act on that pane's session. Elsewhere, add `-s <name>` for a named session.

### Viewers

`trix view` (or `trix -s work view`) watches a running session read-only. Any number of viewers can watch at once, and the controlling terminal's top bar shows how many (`◉ 2 watching`), as does `trix ls`.

- Viewers see the controller's screen. A smaller viewer sees it cropped, a bigger one sees it with blank space around it. A "view only" badge covers the clock. The layout, and the agents' terminal size, always follow the controller, so watching never changes anything.
- Keys go nowhere, except `q`, `esc`, `ctrl+c` or `alt+d`, which stop watching. Mouse selection uses the viewer's own terminal.
- Viewers stay when the controller detaches, and keep seeing live output. When the workspace quits or is killed, they're told and let go.
- Viewers don't mark anything as seen, and they don't receive clipboard copies made in the controller.

### Sessions

Without `-s` you get the default session. `-s <name>` (or `--session <name>`) picks another one: letters, digits, `-` and `_`, up to 32 characters. Each session is its own server, with `session-<name>.sock` and `server-<name>.log` in the runtime folder and `workspace-<name>.json` for restore, so killing or restoring one never touches another. A named session shows its name in the top bar, the terminal title and its desktop notifications.

`trix ls` without `-s` lists every running session. Inside a pane, `TRIX_SESSION` holds the pane's session, so `trix ls` and `trix kill` there act on that session.

### Restore

The layout (kind, name, folder, extra args, and the conversation id Claude and Codex report through their hooks) is saved to `$XDG_STATE_HOME/trix/workspace.json` (default `~/.local/state/trix/`; `workspace-<name>.json` for a named session) whenever it changes. When a fresh server finds it, you're asked once:

- `y` restores. Claude and Codex resume their conversation, or start fresh if they never reported one. Shells reopen where they were. Custom commands come back **stopped** (they could do anything when re-run); press enter on one to start it. Agents whose folder is gone, such as a deleted worktree, are skipped.
- `n` starts fresh and forgets the saved workspace.
- `esc` decides later. Nothing is lost, and the question comes back on the next attach.

Scrollback isn't restored, and closed or exited agents aren't saved.

| key | action |
| --- | --- |
| `alt+n` | new agent (claude · codex · shell · custom command, folder, first prompt, worktree) |
| `alt+1…9` / `alt+j` `alt+k` | jump / next / previous |
| `alt+a` | next agent that needs you |
| `alt+g` | grid ⇄ focus |
| `alt+b` | broadcast a prompt |
| `alt+f` | search every agent's scrollback |
| `alt+r` / `alt+w` | rename / close |
| `alt+d` | detach (agents keep running) |
| `alt+s` | toggle sidebar |
| `shift+pgup/pgdn`, wheel | scrollback |
| drag | select & copy (OSC 52, plus wl-copy or pbcopy) |
| `alt+?` | all keys |
| `alt+q` | quit (stops every agent) |

Every other key goes straight to the focused agent, including `shift+enter` for a newline in the Claude and Codex composers.

## macOS

trix runs on Linux and macOS. On a Mac:

- **Make Option send Alt.** Every trix key is `alt+…`, and macOS terminals type `˜` for Option-N unless told otherwise. In Ghostty, set `macos-option-as-alt = true`. In iTerm2, go to Settings › Profiles › Keys and set Left Option to `Esc+`. In Terminal.app, go to Settings › Profiles › Keyboard and turn on "Use Option as Meta key". In kitty, set `macos_option_as_alt yes`.
- **Notifications** go through `osascript`. They appear as coming from Script Editor; allow it once in System Settings › Notifications.
- **The chime** plays through `afplay`. **Copying** goes through OSC 52, plus `pbcopy` for terminals that ignore it (Terminal.app).
- **Files:** sockets and logs live under `$TMPDIR/trix` (there's no `XDG_RUNTIME_DIR`). The saved workspace lives in `~/.local/state/trix`. Without Omarchy, the theme is Tokyo Night, or whatever `TRIX_THEME` points at.

## Codex

Codex runs a hook only after you've trusted it in its `/hooks` screen. trix passes its hooks the same way on every launch (`-c hooks=…`, one hook per event, all running `trix hook codex`), so trusting them once covers every Codex pane trix ever opens:

1. Open a Codex pane in trix (`alt+n`, codex).
2. Type `/hooks`, and trust the eight trix hooks.

From then on, Codex panes show live detail like Claude's: `exec_command cargo test`, `apply_patch app.rs, new.md`, `approve exec_command rm -rf dist`, and `interrupted` when you press esc.

Until the hooks are trusted, Codex skips them and trix falls back to Codex's `notify`, which only reports finished turns. Screen heuristics cover the rest, and the first finished turn shows a one-time reminder. Codex records trust against the hook's exact definition, which includes the path to trix. If you run trix from another path, such as `target/release/trix` while developing, Codex asks you to trust its hooks again.

Codex shows a startup warning that `-c` overrides run it "without the shared background server". That's been true of every Codex pane in trix, because trix has always passed `-c notify=…`.

## States

| glyph | state | source |
| --- | --- | --- |
| `⠹` | working | `UserPromptSubmit` / `PreToolUse` hooks, or sustained output |
| `▲` | needs you | `PermissionRequest`, `AskUserQuestion`, or an approval prompt seen on screen |
| `✓` | done | `Stop` hook / Codex `agent-turn-complete`, or work that went quiet |
| `○` | idle | at the prompt, or `Interrupt` (Codex) |
| `•` | unseen | it finished or asked something while you were elsewhere |

## Layout

```
src/main.rs    CLI, attach client, frame-paced server loop
src/session.rs detach/attach and viewers: control socket, framing, remote ratatui backend
src/search.rs  search across agent scrollbacks
src/control.rs agent control: spawn / send / read / wait / close, ls --json, guard rails
src/state.rs   saved workspace for restore
src/app.rs     workspace state, keys, mouse, modals, worktrees, notifications
src/agent.rs   PTY + emulator per agent, state machine, heuristics
src/hooks.rs   unix socket + `trix hook` reporter + Claude settings overlay + Codex payloads
src/ui.rs      all rendering
src/input.rs   key → xterm byte encoding
src/theme.rs   Omarchy theme loading / live reload
src/platform.rs Linux / macOS differences: process liveness and folders, foreground job, notifications, sound, clipboard
```

`cargo test` runs the unit tests, which take well under a second. They cover agent control (argument parsing, name lookup, the send guard, when `wait` counts an agent as settled), scrollback search (on a real emulator fed real output), the agent state machine (hooks and heuristics, on a real PTY running `sleep`), hook parsing, key and paste encoding, the session flag and wire framing, the saved workspace, and theme parsing.

Environment: `TRIX_NO_NOTIFY=1` turns off desktop notifications. `TRIX_NO_SOUND=1` mutes completion chimes independently. Set these before starting the workspace server; an existing server keeps its original environment. `TRIX_THEME=path/to/colors.toml` overrides the theme. Inside a pane, `TRIX`, `TRIX_SOCKET`, `TRIX_AGENT_ID`, `TRIX_AGENT_NAME` and `TRIX_SESSION` are set, so any tool can report state with `trix hook <source>` and a hook-style JSON payload on stdin (e.g. `{"hook_event_name":"Stop"}`). Once a tool reports, trix trusts it and stops guessing from output.

## Next

Nothing queued. Ideas and requests welcome.
