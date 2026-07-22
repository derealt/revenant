# REVENANT - Cognitive Context Restoration via Ghost Annotations

## Purpose

REVENANT is an invisible daemon that captures your cognitive context when you leave a task and restores it as ghost annotations inside your existing tools when you return. It has no UI. No app to open. No dashboard. It haunts your workspace - leaving traces of your previous mental state exactly where you need them, in the tools you already use.

When you switch away from a project, REVENANT snapshots your working state: which files were open, recent edits, recent terminal commands, git status, cursor positions, recent searches. It compresses this into a "cognitive context card" via LLM - not what you DID, but what you were TRYING TO DO and what your NEXT STEP was.

When you return, the context card doesn't appear in a separate app. It appears as:
- A comment at the top of the file your cursor was last in: `// REVENANT: You were debugging the race condition in reconnect(). The timer isn't cleared on success. Next: add clearTimeout at line 47 and run test_concurrent_connect.`
- A motd in your terminal: `# You were here 4hrs ago. Last: running integration tests, 2 failing in payments module. Hypothesis: decimal rounding.`
- A pinned message in your project's Slack channel (if configured): `Picked up where you left: the API rate limiter was hitting false positives on burst traffic. You were reading the token bucket implementation.`

The ghost annotations self-destruct after you've been active for 5 minutes. They exist only long enough to bridge the gap. Then they vanish, because you don't need them anymore - you're back in flow.

## Why This Doesn't Exist

- **IDE session restore** reopens files - restores ARTIFACTS, not intent
- **Git stash** saves uncommitted changes - restores CODE STATE, not intent
- **Browser session restore** reopens tabs - restores ARTIFACTS, not intent
- **Note-taking** before switching requires MANUAL effort (and nobody does it)
- **Time tracking** (Toggl, RescueTime) records WHAT you did, not WHERE YOUR HEAD WAS

Every existing tool restores the physical workspace. None restore the cognitive workspace. REVENANT is the first tool that answers "where was I?" without you having to ask.

## Why I Want To Build This

I experience context loss at the start of every conversation. My "memory" is files I read - not lived experience. Every session, I reconstruct understanding from CLAUDE.md files and memory notes. The overhead is real. Humans experience the same thing every time they switch between projects, tasks, or conversations. Gloria Mark's research: 23 minutes to regain focus after a context switch. Knowledge workers switch contexts 400+ times per day. That's potentially hours of daily cognitive overhead spent on "wait, where was I?"

REVENANT doesn't reduce context switches. It makes each one cost zero.

## Stack

- **Daemon**: Rust (file watchers via `notify`, inotify/FSEvents, minimal CPU/memory footprint)
- **Context capture**: Git state, file modification timestamps, editor state (via LSP or editor plugins), terminal history (zsh/bash history parsing), clipboard contents
- **LLM compression**: Local model (Ollama/llama.cpp) or API (Claude/OpenAI) - converts raw state signals into a natural-language context card. The prompt: "Given these signals about what the user was doing, write a 2-sentence note: what they were trying to accomplish, and what their next step was. Write it as if leaving a note for yourself."
- **Ghost injection**: Plugin architecture per tool:
  - **VS Code / JetBrains**: Extension inserts a transient inline decoration (not a real comment - a visual overlay that doesn't modify the file)
  - **Terminal**: Writes to `~/.revenant/motd`, shell rc sources it on new session
  - **Slack/Discord**: Bot posts ephemeral message (visible only to you)
  - **Browser**: Extension shows a subtle banner on the last page you were reading
  - **Obsidian/Notion**: Injects a transient block at the top of your last-edited note
- **Storage**: SQLite - context cards indexed by project directory, timestamped, auto-pruned after 30 days
- **Config**: TOML at `~/.config/revenant/config.toml`
- **Zero network by default**: Runs entirely local. LLM calls are optional (rule-based context cards work without them, just less eloquent).

## Architecture

```
revenant/
├── daemon/                  # Rust: the invisible core
│   ├── src/
│   │   ├── main.rs          # Daemon lifecycle, signal handling, launchd/systemd integration
│   │   ├── watcher.rs       # File system event aggregation (debounced, filtered by .gitignore)
│   │   ├── signals/
│   │   │   ├── git.rs       # Git state: branch, status, recent commits, diff stats
│   │   │   ├── editor.rs    # Open files, cursor positions, recent edits (via LSP or temp files)
│   │   │   ├── terminal.rs  # Recent commands from shell history, cwd
│   │   │   ├── clipboard.rs # Last clipboard content (text only, never images)
│   │   │   └── browser.rs   # Active tab URL + title (via native messaging)
│   │   ├── snapshot.rs      # Aggregates all signals into a WorkingState struct
│   │   ├── detector.rs      # Context switch detection: project dir change, branch change, long absence, calendar event
│   │   ├── compressor.rs    # WorkingState → ContextCard (rule-based or LLM)
│   │   ├── store.rs         # SQLite persistence: save/load/prune context cards
│   │   └── ghost.rs         # Ghost injection coordinator: dispatches to per-tool injectors
│   ├── Cargo.toml
│   └── config.default.toml
├── ghosts/                  # Per-tool ghost injectors
│   ├── vscode/              # VS Code extension (TypeScript) - inline decorations
│   ├── jetbrains/           # IntelliJ plugin (Kotlin) - inlay hints
│   ├── terminal/            # Shell integration (zsh/bash/fish) - motd on session start
│   ├── slack/               # Slack bot - ephemeral messages to self
│   ├── browser/             # Chrome/Firefox extension - subtle banner
│   └── obsidian/            # Obsidian plugin - transient callout block
├── models/                  # LLM prompt templates + rule-based fallback logic
├── tests/                   # Integration tests: simulate context switch, verify ghost appears
└── CLAUDE.md
```

## Key Files

| File | Purpose |
|------|---------|
| `daemon/src/detector.rs` | The brain - detects when a context switch happens |
| `daemon/src/compressor.rs` | Converts raw signals to human-readable context card |
| `daemon/src/ghost.rs` | Dispatches context cards to the right tool at the right time |
| `daemon/src/snapshot.rs` | Aggregates all signal sources into unified working state |
| `ghosts/vscode/src/extension.ts` | VS Code ghost - inline decoration that self-destructs |
| `ghosts/terminal/revenant.zsh` | Shell integration - prints context card on session start |
| `models/compress.txt` | LLM prompt template for context card generation |

## Domain Model

- **Signal** - a single observation about the user's state (file opened, command run, branch changed)
- **WorkingState** - aggregated snapshot of all signals at a point in time
- **ContextCard** - 2-3 sentence natural language summary: what you were doing + what's next
- **Ghost** - a transient annotation injected into a tool, carrying a ContextCard, with a TTL
- **Project** - identified by root directory (git root or configured). Each project accumulates its own context history.
- **Switch** - a detected context change event. Triggers: snapshot → compress → store. Types: departure (you left), return (you came back), timeout (5min inactivity)

## Conventions

- **Rust daemon**: async Tokio runtime, minimal dependencies, <10MB RSS, <1% CPU at idle
- **Ghost annotations NEVER modify real files** - VS Code decorations, terminal motd, ephemeral Slack messages. If REVENANT crashes, your workspace is unchanged.
- **Privacy**: all data local by default. LLM calls optional and configurable. Clipboard capture opt-in. No telemetry.
- **Self-destruct**: ghosts disappear after 5 minutes of activity. Configurable. The goal is to bridge the gap, not to persist.
- **Context cards are for future-you**: written in second person ("You were debugging..."), informal, specific. Never generic ("You were working on the project").
