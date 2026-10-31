# Working on ClubCraft

These rules are for everyone who changes the code, people and AI coding agents alike. The
crates and how they fit together are described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build and check

```sh
cargo build -p clubcraft                                    # the app (binary "ClubCraft")
cargo test --workspace                                      # tests that need models skip or are #[ignore]d
cargo clippy --workspace --all-targets -- -D warnings       # must pass before every commit
```

Builds and stem separations are memory-heavy. On a 16 GB machine, run one at a time and limit
cargo's parallel jobs (`-j 4`).

## Never crash

A DJ app that crashes in the middle of a set has failed, so the code must not panic.

- **No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` or `unreachable!` outside tests.**
  The workspace lints in `Cargo.toml` deny them, and `clippy.toml` allows them in tests. Use `?`,
  `let … else`, `if let`, `ok_or_else`, or a sensible fallback value.
- **Locks:** `lock().unwrap_or_else(PoisonError::into_inner)` (or `parking_lot`, which has no
  poisoning).
- **Bad input degrades, it doesn't stop.** Corrupt files, missing models or a failed thread
  spawn are logged and reported in the UI. The rest of the app keeps working.
- **The audio thread** never allocates, locks or does I/O.
- Examples (`crates/*/examples`) are developer tools and may fail loudly. Each file opts out with
  a crate-level `allow`.

## Unsafe code

`unsafe_code` is denied for the whole workspace. Only these modules allow it, each for a stated
reason:

| Module | Why |
|---|---|
| `crates/stems/src/backend/coreml.rs` | CoreML (objc2) FFI and reading its output buffers |
| `crates/stems/src/backend/onnx.rs` | ONNX Runtime allocator registration |
| `crates/engine/src/stretch.rs` | FFI to the vendored Signalsmith Stretch |
| `crates/engine/src/sys.rs` | OS calls: page-fault counter, `mlock`, thread priority |
| `crates/app/src/window_chrome.rs` | AppKit view hierarchy for the window buttons |

Every `unsafe` block carries a `// SAFETY:` comment saying why it is sound
(`clippy::undocumented_unsafe_blocks` is denied). Don't add another unsafe module without a
good reason; prefer extending one of these.

## Clean room

ClubCraft is an independent implementation. Other DJ software is studied only through its data
files, its output and public documentation, **never through its program code**. Read
[docs/clean-room.md](docs/clean-room.md) before working on an importer, an exporter or anything
that imitates another application's behaviour. In short:

- Allowed: the other application's project files and exports, screenshots, measurements of its
  output, and public manuals, help pages and format specifications.
- Not allowed: reading, decompiling or extracting its binaries, bundled scripts, shaders or
  models, or copying its assets. Notes written by someone who did read such code must not reach
  the implementer.
- Write down where each fact comes from (see [docs/interop/djstudio.md](docs/interop/djstudio.md)).
- Never read or log credentials or tokens from another application's configuration files.

## Third-party code and models

Everything ClubCraft ships that it didn't write itself is listed in
[ATTRIBUTION.md](ATTRIBUTION.md). A new dependency, vendored file, model or asset gets a line
there, with its licence. A licence that restricts commercial use, like the MoisesDB dataset
behind COCOLA, needs a note.

## Style

- Match the surrounding code: naming, comment density, idiom.
- Comments explain why, not what.
- Real-time DSP code may index buffers in loops (`needless_range_loop` is allowed for that).
- UI colours and sizes come from `crates/app/src/theme.rs`, not literals spread through views.
- Tests that play audio do it with the master volume at 0, and test data lives in temporary
  folders, never in the user's real library.
