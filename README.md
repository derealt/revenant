# REVENANT

**Cognitive context restoration.** An invisible daemon that captures what you were doing when you leave a task and restores it as ghost annotations in your existing tools when you return.

No UI. No app to open. No notes to write. The ghost appears in your terminal, your editor, your browser — exactly where you return first. It self-destructs after 5 minutes of activity.

## The Problem

You're deep in a bug fix. A meeting starts. You close your laptop. Two hours later you come back and stare at 47 open tabs. What was I doing? What was the bug? What was I about to try?

That 23 minutes of fumbling costs you every time you switch contexts. REVENANT eliminates it.

## How It Works

1. **You work.** REVENANT watches silently — git state, open files, terminal commands.
2. **You leave.** REVENANT captures your cognitive context and compresses it into a card.
3. **You return.** The ghost appears — in your terminal, your editor, your browser.
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
| **Chrome** | Banner at top of page | `rvn setup browser` |
| **VS Code** | Inline decoration | `rvn setup vscode` |
| **Obsidian** | Callout block | `rvn setup obsidian` |
| **Slack** | Ephemeral message | `rvn setup slack` |

## Commands

```bash
rvn init              # First-time setup
rvn status            # Daemon health + latest card
rvn history           # View past context cards
rvn test              # Inject a test ghost
rvn setup llm         # Enable AI-powered cards (optional)
rvn setup browser     # Install Chrome extension
rvn setup vscode      # Install VS Code extension
rvn off               # Stop the daemon
rvn on                # Start the daemon
rvn forget            # Delete context cards
```

## What the Ghost Looks Like

**Terminal:**
```
┌─── REVENANT ─── arbiter ─── 2h ago ───
│ You were fixing the WebSocket subscription format and
│ reconnection logic. 3 files changed, 45 lines added.
│ Next: run the reconnect tests to verify your fix holds.
└─── ghost expires in 5min of activity ───
```

**Chrome:**

A subtle banner at the top of any webpage with your context summary and next step. Dismiss with X or it auto-clears after 5 minutes.

## LLM Compression (Optional)

By default, REVENANT uses a smart rule engine — 7 components that produce natural context cards from git signals, commit messages, and file paths. Zero network, zero cost, zero latency.

Optionally, enable LLM compression for richer cards:

```bash
rvn setup llm --provider claude --key YOUR_API_KEY
rvn off && rvn on
```

Supports: Claude (Anthropic), OpenAI, Ollama (local, free).

## Privacy

- **All data stays on your machine.** No server, no cloud, no telemetry.
- **LLM calls are opt-in** and go directly from your machine to the provider.
- **Ghost annotations never modify your files.** VS Code decorations, terminal MOTD, browser DOM injection — all ephemeral.
- **Clipboard and browser signals are opt-in** in the config.

## Architecture

```
~/.local/bin/revenant    — daemon (runs as LaunchAgent)
~/.local/bin/rvn         — CLI
~/.revenant/             — runtime data (db, pid, logs, ghost state)
~/.config/revenant/      — config + shell integration
```

## Requirements

- macOS (Linux coming soon)
- zsh, bash, or fish shell
- Chrome (for browser ghost)
- VS Code (for editor ghost)

## License

MIT
