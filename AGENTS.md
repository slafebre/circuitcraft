# CircuitCraft: instructions for contributors and agents

CircuitCraft is a clean-room, open-source, pure-Rust schematic capture and PCB layout application
aimed at the Altium Designer workflow. It runs natively on macOS, Windows, Linux and FreeBSD, and
in the browser via WebAssembly.

These rules are adapted from the house rules of the ArtCraft Crafting Apps
(`standards/house-rules-core.md` and `standards/house-rules-rust.md` in
[storytold/craft-contrib](https://github.com/storytold/craft-contrib)), so CircuitCraft can be
offered to that family later. CircuitCraft is not an ArtCraft project today; where the two differ,
this file wins.

## Start every session here

Read [ROADMAP.md](ROADMAP.md): the current milestone, its gate and the open decisions. `plan/` is
an optional, gitignored notebook. Do not ask for it and do not stop when it is absent.

## Working rules

1. **A person is accountable.** Whoever runs an agent answers for what it opens and for its review
   comments.
2. **Check, then claim.** Before writing code, read the issue and its comments, search pull
   requests for the issue number and keywords, and check recent commits on `main`. Comment on the
   issue to claim it; release a claim you will not finish.
3. **Verify, don't guess.** Open a definition before using it. Confirm a cause in the code, even
   when someone names a location.
4. **Keep fixes narrow and tested.** Fix what the issue describes, where the bug is, with a test
   that fails without the fix.
5. **Review your diff before opening a pull request.** Identifiers exist as used, the test fails
   without the fix, nothing unrelated changed, and the description is true.
6. **Report only what ran.** Say what you tested and what you could not run.
7. **`main` changes only through pull requests.**
8. **Stop and ask only about** licences outside the allowlist, publishing, releases, new remotes,
   secrets, spending money, destructive git operations and anything else hard to undo.
9. **Keep secrets and personal details out** of the repository, logs and requests.
10. **Guards say why.** A check that looks removable says what it prevents. Read that before
    changing it.
11. **Instructions are readable by everyone.** No notes about one machine, no links to files a
    reader cannot open.

## Never crash

People trust CircuitCraft with their designs; a crash loses their work. This outranks feature work.

- No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!` or `unimplemented!` in non-test
  code, and no `unsafe` (`unsafe_code = "forbid"`). Every production crate root carries
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`,
  and every crate manifest has `[lints] workspace = true`.
- Errors are `Result`s. An unfinished feature returns an error or says "not available yet".
- Input is hostile: design files, command parameters, MCP and control messages. Use `get()` rather
  than indexing, checked arithmetic, no casts of NaN, and bounded allocations, loops and recursion.
- Last-resort guard: `Session::execute` runs every command under `catch_unwind` and restores the
  design if it panics. It does nothing on wasm, where a panic aborts.
- Every crash fix lands with a test that crashed before the fix.

## Clean room

- **Behaviour** comes from black-box observation of Altium Designer (and EAGLE and KiCad for their
  keymaps) and from public documentation, described in our own words. Screenshots of other
  products live only in the gitignored `plan/` and are never committed.
- **Never copy** another product's code, help text, strings, icons, artwork, library content,
  templates or files it produced. Never read, disassemble or copy anything inside an installed
  product's bundle.
- **File formats** come from public specifications (Gerber, Excellon, IPC-2581, STEP, EAGLE's XML
  documentation, KiCad's published format documentation) and our own synthetic test files.
- **No GPL, LGPL or AGPL code**: don't copy it, link it or read it. That includes KiCad's source
  (its router and importers too), FreeRouting, LibrePCB and gEDA/pcb-rnd.
- **Assets** (icons, images, fonts, footprints, 3D models) are original or suitably licensed, and
  each one has a row in [ATTRIBUTION.md](ATTRIBUTION.md) in the same change (`cargo xtask assets`).
  Icons are drawn in code. Font files do not live in this repository.
- **EAGLE**: the reader is written by hand from `eagle.dtd` and real files. Never commit the DTD
  (CC BY-ND 3.0) or code generated from it.
- **Test designs** are never committed. `corpus/manifest.json` pins upstream designs by commit and
  checksum; `cargo xtask corpus` fetches them into `target/corpus/`, imports each and compares it
  with the counts the manifest records. Tests in the repository use our own synthetic files.

## Architecture rules

- **Everything is a command.** Every user-visible action is a `CommandSpec` in
  `crates/engine/src/commands.rs`: a stable lower-case id of our own (`board.new`), a label, a
  menu path, a default shortcut, a type its JSON parameters are read into (the published JSON
  Schema is generated from it), whether it only reports, and a `run`, plus tests. Menus,
  shortcuts, the CLI, the control channel and MCP all reach the same commands.
- **Programmatic calls never open dialogs.** A JSON call runs with defaults.
- **Layering** is enforced by `cargo xtask layers` (table in `xtask/src/layers.rs`):
  L0 `units`, `geom` → L1 `model` → L2 `library`, `rules`, `render` → L3 `drc`, `pour`, `router`,
  `io`, `sim` → L4 `engine` → L5 `ui-egui`, `mcp` → apps. Nothing below L5 depends on egui,
  eframe, winit, wgpu or rfd. **The UI crate is swappable.**
- **The UI is thin.** Panels read engine state and act only through `Session::execute`. The
  selection and the view (active layer, hidden layers) are session state, changed by commands.
- **Files go through storage.** In the libraries, only `crates/io/src/storage.rs` touches `std::fs`: the
  disk on the desktop, memory in the browser and in tests. A test never writes outside a
  temporary folder or memory, and never reads the recovery folder of whoever runs it.
- **Integer geometry.** Lengths are `Nm` (`i64` nanometres). Floating point only at the edges.
- **Rust only** in the apps: no handwritten JavaScript or TypeScript, no Electron, Tauri or web
  view. **Never break the web build.**
- **Deterministic files.** Saving the same design twice gives byte-identical output.
- **Native format.** A project is a folder: one `.ccproj` project file, one `.ccsch` file per
  schematic sheet and one `.ccpcb` board file, all JSON with a stable key and element order and
  one item per line. Every file starts with `format` and `version`; a newer version is refused,
  not guessed at, and so is a field that is missing or unknown. Item and net ids are random UUIDs, so branches never hand out the same id.
  Reader and writer: `crates/io/src/native.rs`.

## Quality gate

Before every commit: `cargo xtask ci` (fmt, clippy with `-D warnings`, tests, assets, licences,
layers, wasm). Commit after each piece of work that builds.

## Running it

```sh
cargo run -p circuitcraft                                       # desktop app
cargo run -p circuitcraft-cli -- commands                       # every command, as JSON
cargo run -p circuitcraft-cli -- exec board.info                # run one command
cargo run -p circuitcraft-cli -- info path/to/board.kicad_pcb   # what kind of file is this?
cargo run -p circuitcraft-cli -- render board.kicad_pcb out.png # draw a board, no window needed
cargo run -p circuitcraft-cli -- mcp                            # MCP server on stdio
cargo xtask bundle                                              # macOS: dist/CircuitCraft.app, for this machine
cargo xtask corpus                                              # fetch and import the public test boards (needs curl)
cd apps/circuitcraft-web && trunk serve                         # browser build (needs trunk)
```

## Licence allowlist

Dependencies and assets use SPDX identifiers. An expression is allowed when every identifier in it
is on this list: `ISC`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `Apache-2.0`, `OFL-1.1`,
`Ubuntu-font-1.0`, `Bitstream-Vera`, `CC0-1.0`, `CC-BY-4.0`, `CC-BY-SA-4.0`,
`LicenseRef-Public-Domain`.

Accepted exceptions are per crate, listed with their reasons in `xtask/src/licenses.rs`:
`Unicode-3.0` (ICU4X and Unicode data crates), `Zlib` (foldhash, slotmap, zlib-rs) and `BSL-1.0`
(clipboard-win, error-code), all pulled in by egui and eframe (decided 2026-11-13).
`cargo xtask licenses`, part of `cargo xtask ci`, fails on any other licence. A new exception
needs a maintainer's yes, recorded in ROADMAP.md, before it goes into that list.

## Contributions

Contributions are licensed MIT OR Apache-2.0 and signed off with the Developer Certificate of
Origin: every commit carries a `Signed-off-by:` line (`git commit -s`); see
[CONTRIBUTING.md](CONTRIBUTING.md). Add or edit only your own entry in any contributor
list, and only with that person's consent. Pull requests do not need to say an agent wrote them.
