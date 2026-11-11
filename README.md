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

- **Hooks first, guessing second.** trix starts Claude Code with `--settings <overlay>` and Codex with `-c notify=…`, so the agent reports its own lifecycle: prompt submitted, each tool call, permission request, question, turn finished. Your own agent configs are never touched. Agents without hooks fall back to output cadence and screen heuristics.
- **Feedback you can't miss.** Each agent shows a live state glyph, what it is doing right now (`Edit app.rs`, `Bash cargo test`, `approve …`), time in state, a 48-second output sparkline, git branch, and the diff it has made so far. When an agent you're *not* looking at needs you, a red strip appears over your current pane. You also get a desktop notification, and the terminal bell sets the urgency hint when trix is unfocused. The terminal title tracks the count too.
- **One key to the next thing that matters.** `alt+a` goes to the agent that has been blocked longest, then to unseen finished work.
- **Many agents, one glance.** `alt+g` tiles every agent live. The focused tile is accented, blocked tiles turn red, and the rest fade back a little.
- **Isolation built in.** "Worktree" in the new-agent dialog creates `.trix/worktrees/<name>` on branch `trix/<name>`. It is excluded from git automatically, so parallel agents never step on each other.
- **Broadcast.** `alt+b` sends one prompt to every agent, or only to the ones that are idle or done. Shells and agents sitting on a prompt are skipped, so a broadcast can never answer a permission dialog.
- **Agents outlive the terminal.** The workspace runs in a background server. Closing the window, losing SSH or pressing `alt+d` only detaches; agents keep working and still notify you on the desktop. Run `trix` again from any terminal to pick up where you were, including scrollback. `trix ls` shows who needs you without attaching.
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
trix kill                 # stop the workspace and every agent in it

trix -s work              # the same, for a named session (works with every command)
trix kill --all           # stop every running session
```

`trix` starts a background server on first use (`$XDG_RUNTIME_DIR/trix/session.sock`, log in `server.log` next to it) and attaches to it. One terminal is in control at a time: attaching from a second one takes over from the first. To look without taking over, use `trix view`. The server keeps the environment of the terminal that started it, so agents launched later inherit that `PATH`.

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
| `alt+r` / `alt+w` | rename / close |
| `alt+d` | detach (agents keep running) |
| `alt+s` | toggle sidebar |
| `shift+pgup/pgdn`, wheel | scrollback |
| drag | select & copy (OSC 52 + wl-copy) |
| `alt+?` | all keys |
| `alt+q` | quit (stops every agent) |

Every other key goes straight to the focused agent, including `shift+enter` for a newline in the Claude and Codex composers.

## States

| glyph | state | source |
| --- | --- | --- |
| `⠹` | working | `UserPromptSubmit` / `PreToolUse` hooks, or sustained output |
| `▲` | needs you | `PermissionRequest`, `AskUserQuestion`, or an approval prompt seen on screen |
| `✓` | done | `Stop` hook / Codex `agent-turn-complete`, or work that went quiet |
| `○` | idle | at the prompt |
| `•` | unseen | it finished or asked something while you were elsewhere |

## Layout

```
src/main.rs    CLI, attach client, frame-paced server loop
src/session.rs detach/attach: control socket, framing, remote ratatui backend
src/state.rs   saved workspace for restore
src/app.rs     workspace state, keys, mouse, modals, worktrees, notifications
src/agent.rs   PTY + emulator per agent, state machine, heuristics
src/hooks.rs   unix socket + `trix hook` reporter + Claude settings overlay
src/ui.rs      all rendering
src/input.rs   key → xterm byte encoding
src/theme.rs   Omarchy theme loading / live reload
```

`cargo test` runs the unit tests, which take well under a second. They cover the agent state machine (hooks and heuristics, on a real PTY running `sleep`), hook parsing, key and paste encoding, the session flag and wire framing, the saved workspace, and theme parsing.

Environment: `TRIX_NO_NOTIFY=1` turns off desktop notifications. `TRIX_THEME=path/to/colors.toml` overrides the theme. Inside a pane, `TRIX`, `TRIX_SOCKET`, `TRIX_AGENT_ID`, `TRIX_AGENT_NAME` and `TRIX_SESSION` are set, so any tool can report state with `trix hook <source>` and a hook-style JSON payload on stdin (e.g. `{"hook_event_name":"Stop"}`). Once a tool reports, trix trusts it and stops guessing from output.

## Next

- Codex lifecycle hooks once trusting them can be scripted, for tool-level detail from Codex too.
- Search across all agent scrollbacks.
