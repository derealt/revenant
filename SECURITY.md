# Security

REVENANT runs on your machine and reads what you work on, so its security model is short and strict.

## What it touches

- **Reads:** git state of your projects, file modification times, your shell history file, the editor extension's state file (open files, cursor line), and, only if you turn them on, clipboard text and the active browser tab.
- **Writes:** its own files under `~/.revenant/` and `~/.config/revenant/`, one line in your shell rc file (added by `rvn init`), and a LaunchAgent (macOS) or systemd user unit (Linux). It never modifies your project files.
- **Network:** none by default. If you enable LLM cards, the card prompt goes from your machine to the provider you chose (Anthropic, OpenAI, or a local Ollama).

## The local server

The daemon listens on `127.0.0.1:7711` (loopback only) for the browser extension, `rvn`, and the shell hook. It refuses:

- any request that carries a web page origin, so a site you visit cannot read your card or write into your banner;
- any `Host` other than `127.0.0.1:7711` or `localhost:7711`, which blocks DNS rebinding;
- request bodies over 16 KB.

It sends no CORS headers. The extension reaches it through its `host_permissions`.

The editor extension and the optional Slack sidecar talk over Unix sockets in `~/.revenant/`.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting on this repository (Security tab, "Report a vulnerability"). Do not open a public issue for security problems. You will get a reply within a few days.
