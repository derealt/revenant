# REVENANT - Cognitive Context Restoration via Ghost Annotations

## Purpose

REVENANT is an invisible daemon that captures your cognitive context when you leave a task and restores it as ghost annotations inside your existing tools when you return. It has no UI. No app to open. No dashboard. It haunts your workspace - leaving traces of your previous mental state exactly where you need them, in the tools you already use.

When you switch away from a project, REVENANT snapshots your working state: which files were open, recent edits, recent terminal commands, git status, cursor positions, recent searches. It compresses this into a "cognitive context card" (rule engine by default, optional LLM) - not what you DID, but what you were TRYING TO DO and what your NEXT STEP was.

When you return, the context card doesn't appear in a separate app. It appears as:
- A comment at the top of the file your cursor was last in: `// REVENANT: You were debugging the race condition in reconnect(). The timer isn't cleared on success. Next: add clearTimeout at line 47 and run test_concurrent_connect.`
- A motd in your terminal: `# You were here 4hrs ago. Last: running integration tests, 2 failing in payments module. Hypothesis: decimal rounding.`
- A pinned message in your project's Slack channel (if configured): `Picked up where you left: the API rate limiter was hitting false positives on burst traffic. You were reading the token bucket implementation.`

The ghost annotations self-destruct after a minute by default (the transience law; raisable per channel). They exist only long enough to bridge the gap. Then they vanish, because you don't need them anymore - you're back in flow.

## Why This Doesn't Exist

- **IDE session restore** reopens files - restores ARTIFACTS, not intent
- **Git stash** saves uncommitted changes - restores CODE STATE, not intent
- **Browser session restore** reopens tabs - restores ARTIFACTS, not intent
- **Note-taking** before switching requires MANUAL effort (and nobody does it)
- **Time tracking** (Toggl, RescueTime) records WHAT you did, not WHERE YOUR HEAD WAS

These tools restore the physical workspace. REVENANT answers "where was I?" without you having to ask, in whichever tool you open first.

## Why It Matters

Programmers take 10 to 15 minutes to start editing code again after an interruption, and when interrupted mid-edit only 10% resume in under a minute (Parnin, https://blog.ninlabs.com/blog/programmer-interrupted). Mark, Gonzalez and Harris (CHI 2005) found interrupted work is resumed after about 25 minutes on average, with about two other tasks in between, and that a visible cursor "can enable one to immediately reorient" to a document: the case for resume-at-cursor.

Do NOT quote "23 minutes to regain focus". The 23 min 15 s figure comes from a 2006 interview and measures time until people RETURNED to a task, not time to refocus. Launch research with sources: `docs/launch/PAIN_RESEARCH.md` (local only, gitignored).

REVENANT doesn't reduce context switches. It makes each return cheaper.

## Stack

- **Daemon**: Rust (file watchers via `notify`, inotify/FSEvents, minimal CPU/memory footprint)
- **Context capture**: Git state, file modification timestamps, editor state (recently edited files + VS Code workspace state), terminal history (zsh/bash history parsing), clipboard contents (opt-in). The Chrome extension reports nothing: it only fetches and draws the card (no `tabs` permission; store policy wants minimal permissions)
- **LLM compression**: Local model (Ollama) or API (Claude/OpenAI) - converts raw state signals into a natural-language context card. The prompt: "Given these signals about what the user was doing, write a 2-sentence note: what they were trying to accomplish, and what their next step was. Write it as if leaving a note for yourself." Default is the rule engine: no LLM, no network.
- **Ghost injection**: Plugin architecture per tool:
  - **VS Code**: Extension inserts a transient inline decoration (not a real comment - a visual overlay that doesn't modify the file)
  - **Terminal**: Every new shell POSTs its cwd to the daemon's `/shell-open` endpoint and is ALWAYS answered with the latest card, rendered as MOTD text: opening a terminal is asking "where was I?". The daemon's absence rules decide only the second question: whether that shell-open also counts as a return that wakes the other ghost channels. `~/.revenant/motd` remains as the live-ghost window and the fallback when the daemon is unreachable.
  - **Slack**: Go sidecar posts ephemeral message (visible only to you)
  - **Browser**: Extension polls the daemon's localhost HTTP server and shows a subtle banner. THE QUIET LAW: the banner is a push into a surface you are actively using, so it only wakes on a machine return (sleep, or a long gap in all activity) - never on mid-flow project hops or branch switches (`machine_return_only` in config, default true)
  - **Obsidian**: Plugin polls a state file and injects a transient callout block
- **Storage**: SQLite - context cards indexed by project directory, timestamped, auto-pruned after 30 days
- **Config**: TOML at `~/.config/revenant/config.toml`
- **Zero network by default**: Runs entirely local. LLM calls are optional (rule-based context cards work without them, just less eloquent).

## Architecture

```
revenant/
├── daemon/                  # Rust: the invisible core
│   ├── src/
│   │   ├── lib.rs           # Library target: exposes all modules so tests drive real code
│   │   ├── main.rs          # Daemon binary: lifecycle, signal handling, launchd integration
│   │   ├── cli.rs           # rvn binary: standalone CLI (talks via SQLite/launchctl/HTTP)
│   │   ├── watcher.rs       # File system event stream (filtered, transition-level events)
│   │   ├── signals/
│   │   │   ├── git.rs       # Git state: branch, status, recent commits, diff stats
│   │   │   ├── editor.rs    # Recently edited files + VS Code workspace state
│   │   │   ├── terminal.rs  # Recent commands from shell history, cwd
│   │   │   ├── clipboard.rs # Last clipboard content (text only, never images)
│   │   │   └── browser.rs   # Tab store for POST /tab (no current client sends it)
│   │   ├── snapshot.rs      # Aggregates all signals into a WorkingState struct
│   │   ├── detector.rs      # Context switch detection: project change, branch change, absence
│   │   ├── compressor.rs    # WorkingState to ContextCard (rule engine or LLM)
│   │   ├── store.rs         # SQLite persistence: save/load/prune context cards
│   │   └── ghost/           # Ghost injection: per-channel injectors + dispatcher
│   │       ├── mod.rs       # GhostDispatcher: routes cards to enabled channels
│   │       ├── http.rs      # localhost:7711 server: browser polling, /tab, /inject, /clear, /shell-open
│   │       ├── terminal.rs  # motd writer
│   │       ├── vscode.rs    # Unix socket client to the VS Code extension
│   │       ├── slack.rs     # Unix socket client to the Go sidecar
│   │       └── obsidian.rs  # State file the Obsidian plugin polls
│   ├── tests/               # Integration tests driving the REAL library code
│   │   ├── integration_test.rs  # capture/compress/store/motd/detector/http drives
│   │   └── card_quality.rs      # Corpus drive holding cards to the quality bar
│   ├── Cargo.toml
│   └── config.default.toml
├── ghosts/                  # Per-tool ghost injectors
│   ├── vscode/              # VS Code extension (TypeScript) - inline decorations
│   ├── terminal/            # Shell integration (zsh/bash/fish) - motd on session start
│   ├── slack/               # Slack bot (Go sidecar) - ephemeral messages to self
│   ├── browser/             # Chrome extension - banner; polls localhost:7711
│   └── obsidian/            # Obsidian plugin - transient callout block
├── models/                  # compress.txt: the LLM prompt template
├── docs/                    # demo-mock.html, img/ (README media), launch/ (local only, gitignored)
└── CLAUDE.md
```

## Key Files

| File | Purpose |
|------|---------|
| `daemon/src/detector.rs` | The brain - detects when a context switch happens |
| `daemon/src/compressor.rs` | Converts raw signals to human-readable context card (the rule engine lives here) |
| `daemon/src/ghost/mod.rs` | Dispatches context cards to the right tool at the right time |
| `daemon/src/snapshot.rs` | Aggregates all signal sources into unified working state |
| `daemon/tests/card_quality.rs` | Corpus drive for card quality; ignored printer test for eyeball review |
| `ghosts/vscode/src/extension.ts` | VS Code ghost - inline decoration that self-destructs |
| `ghosts/terminal/revenant.zsh` | Shell integration - prints context card on session start |
| `models/compress.txt` | LLM prompt template for context card generation |

## Domain Model

- **Signal** - a single observation about the user's state (file opened, command run, branch changed)
- **WorkingState** - aggregated snapshot of all signals at a point in time
- **ContextCard** - 2-3 sentence natural language summary: what you were doing + what's next
- **Ghost** - a transient annotation injected into a tool, carrying a ContextCard, with a TTL
- **Project** - identified by root directory (git root or configured). Each project accumulates its own context history.
- **Switch** - a detected context change event. Triggers: snapshot → compress → store. Types: departure (you left: project hop, branch switch, or >15min absence) and return (you came back)

## Conventions

- **Rust daemon**: async Tokio runtime, minimal dependencies, <10MB RSS, <1% CPU at idle
- **Ghost annotations NEVER modify real files** - VS Code decorations, terminal motd, ephemeral Slack messages. If REVENANT crashes, your workspace is unchanged.
- **Privacy**: all data local by default. LLM calls optional and configurable. Clipboard capture opt-in. No telemetry.
- **Self-destruct**: ghosts go dark after 1 minute by default (the transience law), raisable per channel. The goal is to bridge the gap, not to persist.
- **Context cards are for future-you**: written in second person ("You were debugging..."), informal, specific. Never generic ("You were working on the project").
- **THE CARD LAW**: a next step RESTORES what was in flight; it never orders, coaches, or judges. The only imperatives allowed are resume anchors to your own position ("Pick up at detector.rs:88").
- **THE NAMING LAW**: cards speak the file's own name ("compressor.rs", "export/mod.rs"), never a taxonomy label ("utility code"). A topic equal to the project name is no topic; the card says less instead of echoing.
- **Deterministic honesty**: the rule engine asserts only what the signals evidence (names, state, the search question you were asking). Restoring your HYPOTHESIS ("the timer isn't cleared on success") is the optional LLM layer's job - the rule engine never fabricates one.
