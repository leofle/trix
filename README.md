# trix

A workspace where coding agents live. Run Claude Code, Codex, Grok, OpenCode, Cursor, shells, or any CLI agent side by side. trix tells you, at a glance and without asking, which agent is working, which one needs you, and which one finished while you were looking elsewhere.

```
◆ trix   ▲ 1 needs you   ⠹ 2 working   ✓ 1 done                              focus · 14:02
                                   ╭ ▲ api (2)  approve Bash rm -rf dist · 12s        alt+a  jump ╮
 AGENTS  4                         │                                                              │
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

- **Hooks first, guessing second.** trix starts Claude Code with `--settings <overlay>`, Codex with `-c` hook overrides, OpenCode with a plugin in `OPENCODE_CONFIG_CONTENT` and Cursor with `--plugin-dir`, so the agent reports its own lifecycle: prompt submitted, each tool call, permission request, question, turn finished, interrupted. Your own agent configs are never touched. (Codex asks you to trust trix's hooks once; see [Codex](#codex).) Agents without hooks fall back to output cadence and screen heuristics. See [Supported agents](#supported-agents).
- **Feedback you can't miss.** Each agent shows a live state glyph, what it is doing right now (`Edit app.rs`, `Bash cargo test`, `approve …`), time in state, a 48-second output sparkline, git branch, and the diff it has made so far. When an agent you're *not* looking at needs you, a red strip drops in over your current pane's title: who, what it's asking and how long it has waited. Click it, or press `alt+a`, to go there. It covers the title rather than taking a row, so your pane never moves. You also get a desktop notification, and the terminal bell sets the urgency hint when trix is unfocused. When an agent you’re not looking at finishes, a short rising chime plays (through `pw-play` from PipeWire on Linux, `afplay` on macOS). Completions within two seconds share one chime. The terminal title tracks the count too. Which of these happen, and which sound plays, is up to you: see [Config](#config).
- **Context at a glance.** Each Claude and Codex agent shows how full its context window is on its pane: `context 31% · ~691k left`, yellow from 70%, red from 85%. trix reads it from the agent's own conversation log, in the background. `trix ls --json` has it too, so a lead agent can see when a worker is running low. See [Context gauge](#context-gauge).
- **One key to the next thing that matters.** `alt+a` goes to the agent that has been blocked longest, then to unseen finished work.
- **Many agents, one glance.** `alt+g` tiles your agents live, up to four at a time. The focused tile is accented, blocked tiles turn red, and the rest fade back a little. A page holds up to four, two to a row, and three are two on top and one below. With more agents, the grid shows the page with the focused agent, and `alt+j`/`alt+k` move through them. The sidebar keeps full cards and scrolls, or shows one-line cards after `alt+c`.
- **Isolation built in.** "Worktree" in the new-agent dialog creates `.trix/worktrees/<name>` on branch `trix/<name>`. It is excluded from git automatically, so parallel agents never step on each other.
- **Search every agent at once.** `alt+f` searches every agent's scrollback as you type, newest first, with each hit's agent and how far back it is. Enter jumps there: it focuses that agent, scrolls to the line and selects the match.
- **Broadcast.** `alt+b` sends one prompt to every agent, or only to the ones that are idle or done. Shells and agents sitting on a prompt are skipped, so a broadcast can never answer a permission dialog.
- **Agents outlive the terminal.** The workspace runs in a background server. Closing the window, losing SSH or pressing `alt+d` only detaches; agents keep working and still notify you on the desktop. Run `trix` again from any terminal to pick up where you were, including scrollback. `trix ls` shows who needs you without attaching.
- **Agents can run agents.** `trix spawn`, `send`, `read`, `wait`, `close` and `events` let a script, or an agent in another pane, work the workspace the way you do. A "lead" Claude can start workers in their own worktrees, hand them tasks, wait for them and read what they did. It never answers a permission prompt for you, and the sidebar shows which agent it is driving. See [Agent control](#agent-control).
- **Layouts on demand.** `trix layout save work` remembers the tabs you have open, with their folders. `trix layout open work` brings them back whenever you want, with fresh conversations, or the old ones with `--resume`. See [Layouts](#layouts).
- **Watch without taking over.** `trix view` attaches read-only from any number of other terminals: a second monitor, a laptop over SSH, a teammate on the same box. Viewers see exactly what the controlling terminal sees, and nothing they type reaches an agent.
- **Named sessions.** `trix -s work` and `trix -s home` are separate workspaces, each with its own agents, its own saved layout and its own attached terminal. `trix ls` shows all of them at once.
- **Survives restarts too.** The workspace is saved continuously. After `trix kill`, alt+q, a crash or a reboot, the next `trix` offers to bring it back: Claude, Codex, Grok, OpenCode and Cursor resume the exact conversation each pane was in (`claude --resume <id>`, `codex resume <id>`, …), and shells reopen in the folder they were in.
- **Fast.** Rust, with `alacritty_terminal` as the VT engine (the same one Zed uses) and `ratatui`. Rendering is event-driven and capped at about 120 fps, and trix does nothing when nothing changes. Idle cost is about 0.4% of one core and about 10 MB of RAM. `seq 1 2000000` streams through in under a second.
- **Follows your theme.** trix reads Omarchy's `colors.toml` and restyles live when you switch themes. Agent terminals keep your terminal's own ANSI palette. Without Omarchy it falls back to Tokyo Night, or you copy one of the four themes in `themes/` to `~/.config/trix/colors.toml` (see [Themes](#themes)).

## Use

Install from a clone with [Rust](https://rustup.rs) (on a Mac, see [macOS](#macos) first):

```sh
git clone https://github.com/leofle/trix.git && cd trix
cargo install --path .
trix                      # open the workspace (or attach to the running one)
trix claude codex shell   # …and start these agents in the current folder
                          # (also grok, opencode, cursor)
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

Seven commands drive a running workspace from any shell. They're meant for scripts and for agents managing other agents:

```sh
trix spawn claude --name api --worktree --prompt "add rate limiting to /login"
trix send api "also cover it with a test"     # or pipe the prompt in on stdin
trix wait api                                 # blocks until api stops working
trix read api -n 80                           # the last 80 lines of its screen and scrollback
trix ls --json                                # every agent: state, activity, branch, diff, last prompt…
trix close api
trix events --json                            # follow every change, live: one JSON object per line
```

- `<agent>` is an agent's name, or its number in `trix ls`. Names are safer, because numbers shift when an agent closes. `spawn` prints the new agent's name and folder. It refuses a name that's already taken, and it never steals focus from the agent you're looking at.
- `wait` takes one or more agents and returns when one of them stops working (`--all`: when every one has). It prints one `name<TAB>state<TAB>activity` line per agent, where the state is `done`, `idle`, `needs-you` or `exited`. A prompt just sent with `send` counts as work until the agent's turn starts, so `send` followed by `wait` waits for that turn. After `-t` seconds (default 300, `0` waits forever) it exits with 124, and you can simply run it again. A shell counts as done when it's back at its prompt. Builtins and shell functions run inside the shell itself, so `wait` can't see them.
- `events` follows the workspace live, the way `wait` would if it never returned. It starts with one `present` line per agent, then prints a line per change: `spawned`, `state` (with the state it left), `renamed`, `restarted` (a new process in the same place, with a new `id`; `prev_id` is the old one) and `closed`. Name agents to follow only those. Without `--json` it prints a line a person can read (`14:02:11  api  working → needs-you  approve Bash rm -rf dist`). With `--json`, each line is an object with `event`, `at` (Unix ms), `session`, `id`, `number`, `name`, `kind`, `state`, `prev`, `activity`, `unseen`, `cwd` and `managed_by`, the same states as `wait`. It exits 0 when the workspace stops. A reader that stops reading isn't waited for: once 1024 events are queued for it, trix drops it, and it exits 1 saying it fell behind.
- Exit codes: 0 ok, 1 refused or failed (the reason goes to stderr), 2 bad usage, 124 `wait` timed out.

Guard rails, enforced by the server:

- `send` refuses an agent that **needs you**, because its Enter would answer that prompt, for example approve a `rm -rf`. It also refuses a **shell** (the prompt would run as a command) and an agent still **starting** (it may be on a trust dialog). `--force` overrides the first two. Answering prompts is left to you on purpose.
- An agent can't `send` to, `wait` for or `close` itself. trix knows which pane is asking from its `TRIX_AGENT_ID`.
- `close` won't stop an agent that is working or needs you without `--force`.
- Every `send` shows a toast (`lead → api: also cover it…`). Agents that another agent started or prompted show `‹lead` next to their name in the sidebar.

To make a manager, start a Claude pane and tell it what it's for. For example:

> You're the lead for this repo. Don't edit code yourself. Split the task into independent pieces. For each piece, start a worker with `trix spawn claude --worktree --name <piece> --prompt "<brief>"`. Then loop: `trix wait <all workers> -t 280`, `trix read <worker>` to check its work, and `trix send` follow-ups. If a worker shows `needs-you`, tell me which one and why, and leave it alone. When they're all done, summarise each worktree's branch and diff (`trix ls --json`).

Inside a pane, these commands act on that pane's session. Elsewhere, add `-s <name>` for a named session.

### Layouts

A layout is a named set of tabs you can open again whenever you like: the three repos you always work in, a shell per service, a dev server.

```sh
trix layout save work-layout          # the tabs open now: kind, name, folder, extra args
trix layout open work-layout          # open them again, with fresh conversations
trix layout open work-layout --resume # …or pick up the conversations they had when saved
trix layout ls                        # saved layouts and their tabs
trix layout rm work-layout
```

- Layouts live in `~/.config/trix/layouts/<name>.toml` (`$XDG_CONFIG_HOME` if set), one plain TOML file each, meant to be edited by hand or by an agent:
  ```toml
  [[agent]]
  kind = "claude"            # claude, codex, grok, opencode, cursor, shell or custom
  name = "api"
  cwd = "~/code/api"         # a relative folder starts from your home folder
  args = "--model opus"      # optional: extra arguments (for custom, the command)
  session = "0a955a0a-…"     # optional: what --resume picks up
  ```
- `save` records where each shell is now, even if you `cd`'d since. It won't overwrite an existing layout without `--force`.
- `open` adds to the workspace you're in and skips tabs that are already open under the same name, so opening twice is harmless. New tabs never steal focus. A tab whose folder is gone is reported and skipped.
- From a terminal outside trix, `open` starts the workspace if needed and attaches to it, unless a tab failed to start: then it reports why and leaves attaching to you. From inside a pane, it just adds the tabs, so an agent can run it too: "open my work layout". `trix -s <name> layout open …` from a pane starts that session if needed and opens the tabs there.
- `--resume` only applies to Claude, Codex, Grok, OpenCode and Cursor tabs that have a saved `session`. Everything else starts fresh either way.
- How this differs from [Restore](#restore): restore brings back whatever was open after a crash or restart, and it's per session and one-shot. A layout stays until you remove it, and you can open it into any session, any number of times.

### Config

`~/.config/trix/config.toml` (`$XDG_CONFIG_HOME` if set) changes keys, sounds and notifications. Every setting is optional, and without the file trix works as described above. trix reloads the file within a second of each save and says so in a toast. If the file has a mistake, the toast names the line and trix keeps the previous settings, then tells you when it's fixed. `trix config` checks the file from the command line. A `colors.toml` next to it sets the theme (see [Themes](#themes)).

```toml
[keys]
new = "alt+t"                     # one key…
next = ["alt+j", "ctrl+down"]     # …or several
jump = "ctrl+1..9"                # jump is the only one that takes a range
grid = []                         # none: alt+g then reaches the agent

[notify]
on = ["needs-you"]                # which events notify: needs-you, done (both by default)
desktop = true                    # the desktop notification
bell = true                       # the terminal bell: the urgency hint while trix is unfocused
command = "~/bin/ping-phone"      # also run this, for each notification

[sound]
done = "chime"                    # the built-in chime (default), "off", or a sound file
needs_you = "~/sounds/knock.ogg"  # off by default
mute = ["codex", "scratch"]       # agent kinds or names that never make a sound

[strip]
color = "#e0af68"                 # the needs-you strip: "#rrggbb", or a theme color like "yellow"
text = "bg"                       # optional: its text (by default whichever reads best)
```

- **Keys.** The actions are `new`, `jump`, `next`, `prev`, `attention`, `grid`, `sidebar`, `mini_sidebar`, `compact`, `search`, `broadcast`, `rename`, `close`, `menu`, `detach`, `help` and `quit`. Naming an action replaces its keys, and the actions you leave out keep theirs. A key is `alt`, `ctrl` and/or `shift` plus a letter, a symbol, a named key (`up`, `pgdn`, `enter`, `space`, `plus`…) or `f1`–`f12`. It needs `alt` or `ctrl`, so it can't swallow what you type into agents. The exception is an F-key, which may stand alone. `super`/`cmd` is refused, because terminals don't pass it on. A key bound to two actions is an error. The footer, the help (`alt+?`) and the other hints show the keys you chose.
- **Notifications** cover agents you're not looking at, as before. `on` picks the events; `desktop`, `bell` and `command` pick where they go. `command` runs without a shell (as an argv, or a string split like one; `~` works in the program), from your home folder, with the event's JSON on stdin (the same object as `trix events --json`). It also gets `TRIX_NOTIFY_EVENT` (`needs-you` or `done`), `TRIX_NOTIFY_AGENT`, `TRIX_NOTIFY_TITLE`, `TRIX_NOTIFY_BODY` and `TRIX_NOTIFY_SESSION`. For example, `command = ['sh', '-c', 'curl -s -d "$TRIX_NOTIFY_TITLE" ntfy.sh/my-agents']` puts every notification on your phone through [ntfy](https://ntfy.sh).
- **Sounds** follow their own settings, separate from `on`. A sound file can be anything `pw-play` (wav, flac, ogg) or `afplay` (also mp3, m4a) plays. A relative path is looked up next to `config.toml`. Sounds within two seconds of each other share one sound, done and needs-you separately.
- **The strip** is the theme's red by default. `color` and `text` take `#rrggbb` (or `#rgb`), or the name of one of the theme's colors (`red`, `yellow`, `green`, `cyan`, `magenta`, `orange`, `accent`, `fg`, `fg_bright`, `bg`), which follows a theme switch. Without `text`, trix picks the theme's dark or light, whichever contrasts more with the strip.
- `TRIX_NO_NOTIFY=1` still turns off desktop notifications, and `TRIX_NO_SOUND=1` every sound, whatever the file says.

### Context gauge

trix follows each Claude and Codex agent's conversation log (Claude's `~/.claude/projects/…/<session>.jsonl`, Codex's `~/.codex/sessions/…/rollout-…-<thread>.jsonl`). It reads only what was added since its last look, at most every two seconds, in a background thread. The figure is the context as of the agent's last turn.

- **Codex** logs its window size, so its gauge is exact.
- **Claude** doesn't log it. trix starts from 200K, and once any conversation of a model goes past 200K without being compacted, it knows that model has 1M. It remembers that per model in `prefs.json`. Until then a 1M model reads fuller than it is, which is why its pane says `~` before the number left. To set it outright, start trix with `TRIX_CLAUDE_CONTEXT=1000000`. The `[1m]` model variants are recognised as 1M.
- In `trix ls --json`, each agent gets `"context": {"used", "window", "left", "used_percent", "window_exact"}`, or `null` before its first turn.

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

The layout (kind, name, folder, extra args, and the conversation id each agent reports, or that trix gave Grok) is saved to `$XDG_STATE_HOME/trix/workspace.json` (default `~/.local/state/trix/`; `workspace-<name>.json` for a named session) whenever it changes. When a fresh server finds it, you're asked once:

- `y` restores. Claude, Codex, Grok, OpenCode and Cursor resume their conversation, or start fresh if they never reported one. Shells reopen where they were. Custom commands come back **stopped** (they could do anything when re-run); press enter on one to start it. Agents whose folder is gone, such as a deleted worktree, are skipped.
- `n` starts fresh and forgets the saved workspace.
- `esc` decides later. Nothing is lost, and the question comes back on the next attach.

Scrollback isn't restored, and closed or exited agents aren't saved.

The sidebar size you pick with `alt+m`, and what `alt+c` chose, are kept separately, in `prefs.json` in the same folder. It applies to every session and survives restarts whether or not you restore.

| key | action |
| --- | --- |
| `alt+n` | new agent (claude · codex · grok · opencode · cursor · shell · custom command, folder, first prompt, worktree); on the kind row, `c` `x` `g` `o` `u` `s` pick claude, codex, grok, opencode, cursor, shell |
| `alt+1…9` / `alt+j` `alt+k` | jump / next / previous |
| `alt+a` | next agent that needs you |
| `alt+g` | grid ⇄ focus |
| `alt+b` | broadcast a prompt |
| `alt+f` | search every agent's scrollback |
| `alt+r` / `alt+w` | rename (or double-click its name) / close |
| `alt+o`, right-click | an agent's menu: rename, restart, copy folder path, close |
| `alt+d` | detach (agents keep running) |
| `alt+s` | toggle sidebar |
| `alt+m` | mini sidebar: one line per agent, half the width (remembered) |
| `alt+c` | compact sidebar: one-line cards instead of full ones that scroll (remembered) |
| `shift+pgup/pgdn`, wheel | scrollback |
| drag | select & copy (OSC 52, plus wl-copy or pbcopy) |
| `alt+?` | all keys |
| `alt+q` | quit (stops every agent) |

Every other key goes straight to the focused agent, including `shift+enter` for a newline in the Claude and Codex composers.

## macOS

trix runs on Linux and macOS. macOS support is new: if something doesn't work, please [open an issue](https://github.com/leofle/trix/issues) and include `$TMPDIR/trix-$(id -u)/server.log` (`server-<name>.log` for a named session).

### Install

```sh
xcode-select --install          # Apple's compiler and linker (skip it if git already works)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env             # or open a new terminal
git clone https://github.com/leofle/trix.git && cd trix
cargo install --path .          # installs ~/.cargo/bin/trix
```

Then set your terminal's Option key to send Alt (below), and run `trix claude` in a project folder. trix starts [Claude Code](https://claude.com/claude-code), Codex and the [other supported agents](#supported-agents). One that isn't installed yet installs in its own pane the first time you start it, with Homebrew or npm (see [Not installed yet](#not-installed-yet)). Log in to each agent yourself the first time it runs.

### Differences

- **Make Option send Alt.** Every trix key is `alt+…`, and macOS terminals type `˜` for Option-N unless told otherwise. In Ghostty, set `macos-option-as-alt = true`. In iTerm2, go to Settings › Profiles › Keys and set Left Option to `Esc+`. In Terminal.app, go to Settings › Profiles › Keyboard and turn on "Use Option as Meta key". In kitty, set `macos_option_as_alt yes`.
- **Notifications** go through `osascript`. They appear as coming from Script Editor; allow it once in System Settings › Notifications.
- **The chime** plays through `afplay`. **Copying** goes through OSC 52, plus `pbcopy` for terminals that ignore it (Terminal.app).
- **Files:** sockets and logs live under `$TMPDIR/trix-<uid>`, where `<uid>` is your user ID (`id -u`), because there's no `XDG_RUNTIME_DIR`. The saved workspace lives in `~/.local/state/trix`. Without Omarchy, the theme is Tokyo Night until you pick one (see [Themes](#themes)).

### Themes

A Mac has no Omarchy theme for trix to follow, so trix ships four of Omarchy's in `themes/`: `ristretto`, `tokyo-night` (the default), `retro-82` and `lupine`, a light one. To use one, run `install-theme.sh` with its name, from your trix clone:

```sh
./install-theme.sh ristretto
```

It copies the theme to `~/.config/trix/colors.toml` (`$XDG_CONFIG_HOME` if set), next to `config.toml`, and every running trix restyles within a second, with no restart. Run it again with another name to switch, or with `tokyo-night` to go back to the default. Run it with no name to list the themes.

It also installs an Omarchy theme folder or any `colors.toml` by path, such as `./install-theme.sh ~/Downloads/some-theme`. If the `colors.toml` it replaces isn't one of the shipped themes, because you made or edited it, it saves that file next to it as `colors.toml.<date>.bak` first. Installing by hand is the same thing: `mkdir -p ~/.config/trix && cp themes/ristretto/colors.toml ~/.config/trix/colors.toml`. Removing the file keeps the theme you have until trix restarts.

The theme colors trix's own bars, sidebar, cards and borders. The panes, and the space around them, keep your terminal's background and ANSI colors, so give your terminal a matching theme too. This matters most for Lupine: on a dark terminal you get light bars around dark panes.

To make your own theme, edit your copy. It is Omarchy's `colors.toml` format, one `key = "#rrggbb"` per line, and any key you leave out keeps its Tokyo Night color.

trix uses the first theme it finds: the file `TRIX_THEME` points at, then `~/.config/trix/colors.toml`, then Omarchy's current theme. The start screen, with no agents open, says which one is in use. On Linux with Omarchy, the file sets trix's theme apart from the desktop's; remove it to follow Omarchy again. On Linux without Omarchy, it all works as on a Mac.

## Supported agents

trix starts each of these agents by name, with an argv (never through a shell), and installs nothing into your agent configs: everything it adds is passed to that one process and lives in `$XDG_RUNTIME_DIR/trix` until the server stops. An agent that isn't installed yet installs first, in its own pane ([below](#not-installed-yet)). Log in to each agent yourself the first time it runs.

| kind | runs | live state from | needs-you from | resume |
| --- | --- | --- | --- | --- |
| `claude` | `claude` | hooks (`--settings` overlay) | `PermissionRequest`, `AskUserQuestion` hooks | yes, `--resume <id>` |
| `codex` | `codex` | hooks (`-c hooks=…`, trusted once) + `notify` | `PermissionRequest` hook | yes, `codex resume <id>` |
| `grok` | `grok` | terminal title, screen fallback; hooks if you add them (below) | title (`⚠ Action Required`) + Grok's own prompt wording on screen | yes: trix starts it with `--session-id <uuid>`, restores with `--resume <uuid>` |
| `opencode` | `opencode` (2.x: `opencode --standalone`) | plugin events (`OPENCODE_CONFIG_CONTENT`) | `permission.asked`, and `question.asked` (1.x) or a question form (2.x) | yes, `--session <id>` |
| `cursor` | `cursor-agent` | output cadence and screen; plugin hooks (`--plugin-dir`) add each tool | screen only: Cursor's approval prompts | yes, `--resume <chatId>` from `sessionStart` |

- **Grok** ([Grok Build](https://docs.x.ai/build)) can't be handed hooks per process: it reads them only from `~/.grok/hooks`, its config and the other files in its [hook locations](https://docs.x.ai/build/features/hooks). So trix reads Grok's terminal title instead: a braille spinner and what it's doing while it works, `⚠ Action Required` while a permission waits, `grok` when idle. Prompts and dialogs the title doesn't cover (folder trust, questions) come from wording only Grok's prompts use (`Ctrl+o:always-approve`, `No, reject (type to add feedback)`, `x:dismiss`). trix names each new conversation with `--session-id`, so restore resumes it, unless your extra args pick one (`-c`, `-r`, `-s`, `--fork-session`). For tool-by-tool detail, you can add trix's hook yourself, as `~/.grok/hooks/trix.json` (`trix hook grok` does nothing outside a trix pane, and Grok runs it only in folders you've trusted):
  ```json
  { "hooks": {
      "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "PreToolUse":       [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "Stop":             [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "StopCancelled":    [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "Notification":     [{ "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "trix hook grok" }] }]
  } }
  ```
  With it, the hooks report the turn and each tool, and approvals come from both the title and the hook. Grok has no `PermissionRequest` hook; a waiting permission prompt raises `Notification` with type `permission_prompt` ([Grok hook events](https://docs.x.ai/build/features/hooks)), which trix shows as needs-you, with Grok's own message as the activity. Grok's installer also links the name `agent`, which Cursor's installer uses too, so trix never runs `agent`.
- **OpenCode** gets a small plugin, appended to your own `OPENCODE_CONFIG_CONTENT` if you set one (left alone if that isn't a JSON object). It hands OpenCode's events (`session.status`, `permission.asked`, `question.asked`, `tool.execute.before`, …) to `trix hook opencode`, and tells a subagent's sessions from the pane's own so a subagent finishing isn't the turn finishing. It reports only inside a trix pane. OpenCode 2 changed three things, and trix follows each when the installed `opencode --help` lists `--standalone`:
  - **`--standalone`.** OpenCode 2 otherwise joins a background server (`opencode serve --service`) that the first OpenCode started. That server keeps the first one's environment, plugin included, even after that pane or trix itself has gone. With `--standalone`, each pane runs its own server with its own environment. Pass `--standalone` or `--server` in the pane's args and trix adds nothing.
  - **A plugin folder.** OpenCode 2 loads a plugin from a folder only, so trix writes its plugin as `opencode-plugin-<pid>/index.js` too.
  - **A new plugin API.** The folder's plugin is `export default { id, setup }`. It follows `ctx.event.subscribe()` (`session.execution.*`, `session.inbox.enqueued`, `permission.asked`, `form.created`, …) and `ctx.tool.hook`, and reports each the way the OpenCode 1 plugin does.
- **Cursor** gets a plugin directory, `--plugin-dir`, holding only observing hooks: `sessionStart`, `beforeSubmitPrompt`, `postToolUse(Failure)`, `stop`, `sessionEnd`. None of Cursor's permission hooks (`preToolUse`, `beforeShellExecution`, …): their answer decides whether a tool runs, and trix never approves anything. `trix hook cursor` answers `{}` (and `{"continue": true}` before a prompt). cursor-agent 2026.11.11 runs only `sessionStart` and the tool hooks from a plugin, not `beforeSubmitPrompt` or `stop`. So Cursor's screen says when a turn starts and ends: it redraws while it thinks, streams and runs a command, and is silent at its prompt and while an approval waits. The tool hooks add what it ran (`Shell wc -l fruit.txt ✓`), and `sessionStart` names the conversation that restore resumes. Cursor has no hook for its approval prompts either, so trix reads those from the screen: `Run (once)`, `Skip (esc or n)`, `Waiting for approval...`, workspace trust. Only that wording counts, so a question in Cursor's reply doesn't hold the pane on needs-you. If your team's Cursor settings disallow local plugins, the hooks don't run: the state still comes from the screen, but there's no tool detail and restore has no conversation to resume.
- **Context gauge** stays Claude and Codex only.

### Not installed yet

When an agent's command isn't on trix's `PATH`, its pane installs the agent first, then starts it with the arguments it would have had (its first prompt, `--resume`, trix's hooks). You watch the install as it runs. Until it's done, the pane shows `installing grok (npm)` and counts as starting: `trix send` waits for it and `trix wait` keeps waiting, and the install output never reads as the agent working. If the install fails, its output stays on screen and Enter tries again. Two panes starting the same agent don't install it twice: the second one waits, then starts it.

trix uses the first of these you have:

| kind | mise (as on Omarchy) | Homebrew | npm | install script |
| --- | --- | --- | --- | --- |
| `claude` | `claude` | cask `claude-code` | `@anthropic-ai/claude-code` | `claude.ai/install.sh` |
| `codex` | `codex` | cask `codex` | `@openai/codex` | |
| `grok` | `npm:@xai-official/grok` | none (Homebrew's `grok` is another program) | `@xai-official/grok` | |
| `opencode` | `opencode` | `opencode` | `opencode-ai` | |
| `cursor` | | cask `cursor-cli` | | `cursor.com/install` |

- **mise** runs `mise use -g <package>`. If mise's shims aren't on trix's `PATH`, the agent runs through `mise x`.
- **The install scripts** are the vendors' own, fetched with curl and run by bash. They put the agent in `~/.local/bin`, which trix adds to that agent's `PATH` if it's missing.
- trix only reads its own `PATH` to decide, so an agent installed some other way, somewhere on your `PATH`, is never installed again. The server keeps the `PATH` of the terminal that started it: after installing a tool like mise or Homebrew, restart trix from a new terminal (`trix kill`, then `trix`).

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
| `▲` | needs you | `PermissionRequest`, `AskUserQuestion`, OpenCode `permission.asked` / `question.asked`, Grok's title, or an approval prompt seen on screen |
| `✓` | done | `Stop` hook / Codex `agent-turn-complete`, or work that went quiet |
| `○` | idle | at the prompt, or interrupted (Codex `Interrupt`, OpenCode abort) |
| `•` | unseen | it finished or asked something while you were elsewhere |

## Layout

```
src/main.rs    CLI, attach client, frame-paced server loop
src/session.rs detach/attach and viewers: control socket, framing, remote ratatui backend
src/search.rs  search across agent scrollbacks
src/layout.rs  saved layouts: save / open / ls / rm, TOML files
src/usage.rs   context gauge: follows Claude / Codex conversation logs
src/control.rs agent control: spawn / send / read / wait / close, ls --json, guard rails
src/events.rs  `trix events`: the live event stream, its subscribers and the client
src/config.rs  config.toml: keys, notifications, sounds
src/state.rs   saved workspace for restore
src/app.rs     workspace state, keys, mouse, modals, worktrees, notifications
src/agent.rs   PTY + emulator per agent, state machine, heuristics
src/hooks.rs   unix socket + `trix hook` reporter + Claude settings overlay, OpenCode / Cursor plugins + payloads
src/ui.rs      all rendering
src/input.rs   key → xterm byte encoding
src/theme.rs   theme loading (TRIX_THEME, ~/.config/trix/colors.toml, Omarchy) / live reload
src/platform.rs Linux / macOS differences: process liveness and folders, foreground job, notifications, sound, clipboard
themes/        Omarchy themes to use without Omarchy (macOS): Ristretto, Tokyo Night, Retro 82, Lupine
install-theme.sh copies one of them, or any colors.toml, to ~/.config/trix/colors.toml
```

`cargo test` runs the unit tests, which take well under a second. They cover agent control (argument parsing, name lookup, the send guard, when `wait` counts an agent as settled), the event stream (order, filtering, readers that fall behind or leave), the config file (keys, notifications, sounds and their errors), scrollback search (on a real emulator fed real output), the agent state machine (hooks and heuristics, on a real PTY running `sleep`), hook parsing, key and paste encoding, the session flag and wire framing, the saved workspace, and theme parsing and lookup, including the themes in `themes/` and `install-theme.sh`.

Environment: `TRIX_NO_NOTIFY=1` turns off desktop notifications. `TRIX_NO_SOUND=1` mutes every sound independently. Set these before starting the workspace server; an existing server keeps its original environment. `TRIX_THEME=path/to/colors.toml` overrides the theme, ahead of `~/.config/trix/colors.toml`. `TRIX_CLAUDE_CONTEXT=<tokens>` sets Claude's context window for the gauge. Inside a pane, `TRIX`, `TRIX_SOCKET`, `TRIX_AGENT_ID`, `TRIX_AGENT_NAME` and `TRIX_SESSION` are set, so any tool can report state with `trix hook <source>` and a hook-style JSON payload on stdin (e.g. `{"hook_event_name":"Stop"}`). Once a tool reports, trix trusts it and stops guessing from output.

## Next

Nothing queued. Ideas and requests welcome.

## License

MIT. See [LICENSE](LICENSE).
