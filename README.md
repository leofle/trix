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
- **Broadcast.** `alt+b` sends one prompt to every agent, or only to the ones that are idle or done.
- **Fast.** Rust, with `alacritty_terminal` as the VT engine (the same one Zed uses) and `ratatui`. Rendering is event-driven and capped at about 120 fps, and trix does nothing when nothing changes. Idle cost is about 0.4% of one core and about 10 MB of RAM. `seq 1 2000000` streams through in under a second.
- **Follows your theme.** trix reads Omarchy's `colors.toml` and restyles live when you switch themes. Agent terminals keep your terminal's own ANSI palette. Without Omarchy it falls back to Tokyo Night, or you can point `TRIX_THEME` at a colors.toml.

## Use

```sh
cargo install --path .
trix                      # empty workspace — press enter to spawn
trix claude codex shell   # start with agents in the current folder
```

| key | action |
| --- | --- |
| `alt+n` | new agent (claude · codex · shell · custom command, folder, first prompt, worktree) |
| `alt+1…9` / `alt+j` `alt+k` | jump / next / previous |
| `alt+a` | next agent that needs you |
| `alt+g` | grid ⇄ focus |
| `alt+b` | broadcast a prompt |
| `alt+r` / `alt+w` | rename / close |
| `alt+s` | toggle sidebar |
| `shift+pgup/pgdn`, wheel | scrollback |
| drag | select & copy (OSC 52 + wl-copy) |
| `alt+?` | all keys |
| `alt+q` | quit |

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
src/main.rs    terminal setup, frame-paced event loop
src/app.rs     workspace state, keys, mouse, modals, worktrees, notifications
src/agent.rs   PTY + emulator per agent, state machine, heuristics
src/hooks.rs   unix socket + `trix hook` reporter + Claude settings overlay
src/ui.rs      all rendering
src/input.rs   key → xterm byte encoding
src/theme.rs   Omarchy theme loading / live reload
```

Environment: `TRIX_NO_NOTIFY=1` turns off desktop notifications. `TRIX_THEME=path/to/colors.toml` overrides the theme. Inside a pane, `TRIX`, `TRIX_SOCKET`, `TRIX_AGENT_ID` and `TRIX_AGENT_NAME` are set, so any tool can report state with `trix hook <source>` and a hook-style JSON payload on stdin.

## Next

- Detach/attach: run agents in a background server so they survive closing the terminal, as herdr does.
- Codex lifecycle hooks once trusting them can be scripted, for tool-level detail from Codex too.
- Session restore (resume Claude/Codex conversations on relaunch).
- Search across all agent scrollbacks.
