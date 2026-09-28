# Contributing

## Build and test

```bash
cd daemon
cargo test                                             # the whole suite
cargo test --test card_quality -- --ignored --nocapture # print the corpus cards to read them
cargo build --release                                  # target/release/revenant and rvn
```

Extensions:

```bash
cd ghosts/browser && npm ci && npm run package   # load dist/ as an unpacked extension
cd ghosts/vscode && npm ci && npm run build && npx @vscode/vsce package
```

Tests drive the real library code (`daemon/src/lib.rs`). A bug fix comes with a test that fails without the fix.

## The rules cards follow

These are enforced by tests in `daemon/tests/card_quality.rs`. Please keep them.

1. **A card restores, it never coaches.** The next step says what was in flight ("the changes were already staged"). It never tells you what to do ("commit them", "run the tests"). The only instruction allowed is a pointer back to your own position ("Pick up at detector.rs:88").
2. **Real names only.** Cards name the actual file ("compressor.rs", "export/mod.rs"), never a category ("utility code").
3. **Say less rather than invent.** The rule engine only states what the signals show. If there is no clear topic, the card is shorter. Guessing what you believed or intended is left to the optional LLM mode.
4. **Ghosts never touch your files** and disappear after about a minute.

## Wrong cards

If a card was wrong, open a "The card was wrong" issue with the card and what you were really doing. Those reports improve the rule engine more than anything else.
