# Privacy

Revenant has two parts: the Revenant app, which runs on your computer, and optional extensions for Chrome and for VS Code or Cursor. Everything stays on your computer. Syntaxe receives no data from either part.

## The Chrome extension

**What it does.** It shows the note the Revenant app wrote about where you left off in your code, as a banner at the top of the page you are on and in the extension's popup.

**What it accesses.**

- It asks the Revenant app on your own computer, at `http://127.0.0.1:7711`, for the current note. This is the only address the extension ever contacts.
- It adds the banner to web pages and removes it. It does not read the content, address or title of any page, and it does not read your browsing history, bookmarks, cookies, passwords or form data.
- It keeps the current note, and whether you dismissed it, in the browser's session storage. Chrome clears this when the browser closes.

**What it sends.** Nothing, to anyone. It has no analytics, no tracking and no remote code. Everything it runs is in the package you install.

## The Revenant app

**What it reads:** the git state of your projects, the names and change times of files, your shell history file, and the file and line your cursor is on (reported by the editor extension). Clipboard text only if you turn that on.

**Where it keeps it:** in a SQLite file in `~/.revenant/` on your computer. Notes older than 30 days are deleted automatically. `rvn forget` deletes the notes for a project now, and `rvn forget --all` deletes all of them.

**What it sends:** nothing by default. If you turn on fuller notes from a language model, the note's inputs go from your computer to the provider you choose (Anthropic, OpenAI or a local Ollama), under that provider's terms.

It never records your screen or your keystrokes. Details of every read, write and network call: [SECURITY.md](SECURITY.md).

## Contact

Questions about privacy: hello@syntaxeltd.com, or open an issue at https://github.com/derealt/revenant/issues.
