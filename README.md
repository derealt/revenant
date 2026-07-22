# REVENANT

**Cognitive context restoration.** An invisible daemon that captures what you were doing when you leave a task and restores it as ghost annotations in your existing tools when you return.

No UI. No app to open. No notes to write. The ghost appears in your terminal, your editor, your browser - exactly where you return first. It self-destructs after a few minutes of activity (the TTL scales with how long you were away).

## The Problem

You're deep in a bug fix. A meeting starts. You close your laptop. Two hours later you come back and stare at 47 open tabs. What was I doing? What was the bug? What was I about to try?

That 23 minutes of fumbling costs you every time you switch contexts. REVENANT eliminates it.

## How It Works

1. **You work.** REVENANT watches silently - git state, open files, terminal commands.
2. **You leave.** REVENANT captures your cognitive context and compresses it into a card.
3. **You return.** The ghost appears - in your terminal, your editor, your browser.
4. **You read it.** 10 seconds. You're back in flow.
5. **It vanishes.** 5 minutes later, the ghost is gone.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/derealt/revenant/dev/install.sh | sh
```

Or manually:

```bash
# Download the binary for your platform from Releases
# Move to your PATH:
mv revenant rvn ~/.local/bin/

# Initialize:
rvn init

# Open a new terminal. REVENANT is running.
```

## Ghost Channels

| Channel | How it appears | Setup |
|---------|---------------|-------|
| **Terminal** | MOTD on new shell tab | Automatic with `rvn init` |
| **Chrome** | Banner at top of page | `rvn setup browser` (build + load the extension; it polls the daemon on `localhost:7711`) |
| **VS Code** | Inline decoration | `rvn setup vscode` (build + install the extension) |
| **Obsidian** | Callout block | `rvn setup obsidian --vault /path/to/vault` |
| **Slack** | Ephemeral message | `rvn setup slack` (advanced: needs a Slack app and a manually built Go sidecar; see below) |

## Commands

```bash
rvn init              # First-time setup
rvn status            # Daemon health, channel status, latest card
rvn history           # View past context cards
rvn history -p DIR    #   ...for a specific project
rvn history -n 20     #   ...more of them
rvn test              # Inject a test ghost into every enabled channel
rvn clear             # Dismiss the current ghost everywhere, immediately
rvn digest            # Attention digest across projects (last 7 days)
rvn digest --days 30  #   ...further back
rvn setup llm         # Enable AI-powered cards (optional)
rvn setup browser     # Browser ghost setup instructions
rvn setup vscode      # VS Code ghost setup instructions
rvn setup obsidian    # Obsidian ghost setup (--vault enables it directly)
rvn setup slack       # Slack ghost setup (--token/--user/--channel-id enable it)
rvn off               # Stop the daemon
rvn on                # Start the daemon
rvn forget            # Delete context cards for the current project
rvn forget --all      # Delete ALL context cards
```

## What the Ghost Looks Like

**Terminal:**
```
┌─── REVENANT ─── harbor ─── 2h ago ───
│ You were fixing the WebSocket subscription format and
│ reconnection logic. 3 files changed, 45 lines added.
│ Next: run the reconnect tests to verify your fix holds.
└─── ghost expires in 5min of activity ───
```

**Chrome:**

A subtle banner at the top of any webpage with your context summary and next step. Dismiss with X or it auto-clears after 5 minutes. The extension polls the daemon at `http://127.0.0.1:7711/ghost`; nothing leaves your machine.

Optionally, the extension can report your active tab back to the daemon as a context signal. This is OFF by default; the daemon discards tab reports unless you set `browser = true` under `[signals]` in the config.

## Slack (advanced)

The Slack ghost posts ephemeral messages (visible only to you) via a small Go sidecar that listens on `~/.revenant/slack.sock`. It is intentionally manual:

1. Create a Slack app with `chat:write` scope, install it, and copy the bot token.
2. `rvn setup slack --token xoxb-... --user U0XXXXXXX --channel-id C0XXXXXXX` writes the config and enables the channel.
3. Build and run the sidecar yourself: `cd ghosts/slack && go build && ./revenant-slack`.

If you skip step 3, the channel stays quiet; nothing else breaks.

## LLM Compression (Optional)

By default, REVENANT uses a smart rule engine - 7 components that produce natural context cards from git signals, commit messages, and file paths. Zero network, zero cost, zero latency.

Optionally, enable LLM compression for richer cards:

```bash
rvn setup llm --provider claude --key YOUR_API_KEY
rvn off && rvn on
```

Supports: Claude (Anthropic), OpenAI, Ollama (local, free).

## Privacy

- **All data stays on your machine.** No server, no cloud, no telemetry.
- **LLM calls are opt-in** and go directly from your machine to the provider.
- **Ghost annotations never modify your files.** VS Code decorations, terminal MOTD, browser DOM injection - all ephemeral.
- **Clipboard and browser signals are opt-in** in the config.

## Architecture

```
~/.local/bin/revenant    - daemon (runs as LaunchAgent)
~/.local/bin/rvn         - CLI
~/.revenant/             - runtime data (db, pid, logs, ghost state)
~/.config/revenant/      - config + shell integration
```

## Requirements

- macOS (Linux coming soon)
- zsh, bash, or fish shell
- Chrome (for browser ghost)
- VS Code (for editor ghost)

## License

MIT
