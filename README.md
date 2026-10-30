# OpenWare

An open, from-source toolkit for **CaseWare Working Papers** engagement files:
read the ledger, accounts, entries, groupings and documents; render CaseView
statements; and serve a zero-dependency web UI over an engagement folder.

The on-disk formats (`.ac_` packages, DBF/CDX/FPT tables, CaseView documents,
interlink, cube) are implemented directly from production engagement files —
no vendor runtime or installation is required at any point.

## Crates

| crate | binary | role |
|---|---|---|
| `cw-core`  | — | data kernel: package, DBF/memo/index, GUID, interlink, cube, inflate/lzma |
| `cw-app`   | — | domain: accounts, entries, groupings, consolidation, FX, review, lifecycle, import |
| `cw-cv`    | — | CaseView document model, expression engine, sheet renderer |
| `cw-cli`   | `opencw` | interface shell: TUI, `serve` web UI, document manager, verify/inspect tools |

## Quick start

```sh
cargo build --release -p cw-cli
./target/release/opencw serve <engagement-folder> --port 8078
# browse http://localhost:8078
```

Read-only inspection:

```sh
./target/release/opencw health <engagement-folder>
./target/release/opencw census <engagement-folder>
```

## Tests

```sh
cargo test --workspace
```

The suite is self-contained: `tests/golden/` carries the frozen fixtures
(structure captures and small corpus tables); handler tests build their own
temporaries. Expect ~500 tests across 50+ suites, green on macOS (aarch64)
and Windows (x86_64-gnu).

## Windows cross-build (from macOS)

```sh
rustup target add x86_64-pc-windows-gnu
RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu \
  cargo build --release --target x86_64-pc-windows-gnu -p cw-cli
```

requires `x86_64-w64-mingw32-gcc` (e.g. `brew install mingw-w64`).

## Repository layout

```
crates/           the four workspace crates (see table)
tests/golden/     frozen regression corpus (MANIFEST.toml is the index)
```

## Status

Pre-1.0, actively developed. The web UI, document manager and CaseView
renderer are the current focus; the read kernel and domain gates are stable
and regression-pinned.

## License

MIT — see [LICENSE](LICENSE).
