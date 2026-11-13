# CircuitCraft roadmap

**Stage: 0.1 in progress** (workspace scaffold done) · next: the 0.1 gate (foundations and board viewer)

> **Last updated:** 2026-11-13 · **Target:** the current Altium Designer release (the exact
> version is recorded when black-box observation starts)

CircuitCraft aims at the Altium Designer workflow for schematic capture and PCB layout, starting
with what hobbyists and small teams need for 2–4 layer boards, plus what Altium doesn't have:
native builds for every platform, a browser build, agent control over MCP, a scriptable CLI,
git-friendly files and a free licence.

## What 1.0 means

1.0 ships when **real boards designed entirely in CircuitCraft have been fabricated, assembled
and shown to work**, and when the app includes the professional features chosen for it: 3D view
and STEP export, push-and-shove routing, our own autorouter, and SPICE simulation.

Every 0.x release before that is public and usable on its own. 0.5 is the one that matters most:
it is the first release you can design and order a real board with, and the one an open community
gathers around.

## Principles that hold from 0.1

These are not milestones. Every release keeps them true, and CI checks them where it can.

| Principle | What it means in practice |
|---|---|
| Never crash | No panics and no `unsafe` in production code; hostile-input tests for every file reader; a crash fix always lands with a test that crashed before |
| Everything is a command | Every user action is a command with a JSON form; menus, shortcuts, CLI, control channel and MCP all reach the same commands |
| Agents first-class | Each feature is reachable over CLI and MCP in the release that adds it, and covered by headless tests |
| Never break the web build | CI builds WebAssembly on every change; features that need external processes (simulation) are desktop-only and say so |
| Clean room | Behaviour from observation and public docs, formats from public specifications, no GPL/LGPL/AGPL code read or linked, every asset in `ATTRIBUTION.md` |
| Strict layering | `cargo xtask layers` enforces the crate layers; nothing below the UI layer depends on egui, winit or wgpu windowing |
| Deterministic files | Saving the same design twice gives byte-identical files |

## Milestones

Sizes are rough guesses made before any code exists (S: days, M: one to two weeks, L: a month,
XL: more), to be replaced by measured numbers once there is a build to calibrate against.

### 0.1 Foundations and board viewer

Proves the workspace, the data model, the renderer and the agent interface on real designs.

| Deliverable | Size |
|---|---|
| ✅ Workspace scaffold: crate layers, `cargo xtask ci` (fmt, clippy with deny lints, tests, layers, assets, wasm), licences, `AGENTS.md` with the house rules, `ATTRIBUTION.md`, command registry, CLI (`commands`, `exec`, `info`, `mcp`), MCP server, empty desktop and web shells | S |
| ✅ `units` and `geom`: `i64` nanometre coordinates, lines, arcs, polygons with arcs, booleans, offsets and track strokes (on `i_overlay`) | M |
| ✅ `model` v0: board outline, layer stack, pads, tracks, arcs, vias, copper polygons, text, footprints, nets; copy-on-write store with undo | M |
| Native format v0: read and write, deterministic, with a version field | S |
| EAGLE `.brd` and KiCad board import, read-only | M |
| Renderer: layered 2D GPU view, layer visibility and colours, single-layer mode, net highlight, pan and zoom | M |
| Stroke font for silkscreen text (reusable from the Crafting Apps) | S |
| `circuitcraft-cli` (`info`, `convert`, `run`) and MCP (`open`, `inspect`, `query`, `render`) | S |
| Desktop app shell and web build | S |

**Gate:** a corpus of permissively licensed public EAGLE and KiCad boards opens without a crash
and renders recognisably, from the app, the CLI and MCP.

### 0.3 Schematic capture

| Deliverable | Size |
|---|---|
| Schematic model: sheets, symbols, pins, wires, junctions, net labels, power ports, buses, hierarchical sheets and ports, parameters | M |
| Project compile: nets from sheets, one stable ID per component, messages panel | M |
| Electrical rules check with a configurable pin-connection matrix | S |
| Annotation (designators), bill of materials v0 (CSV) | S |
| Symbol library format and symbol editor | M |
| Altium-style interaction: two-key shortcut sequences, Tab to edit while placing, Space to rotate, X/Y to mirror, Properties, Projects and Messages panels; EAGLE and KiCad keymaps | M |
| EAGLE `.sch` and `.lbr`, KiCad schematic and symbol library import | M |
| Schematic PDF output | S |

**Gate:** the schematic of each reference board is drawn in CircuitCraft (or imported) and
compiles with a clean electrical rules check.

### 0.5 Layout and fabrication: first boards

| Deliverable | Size |
|---|---|
| Schematic-to-board sync (design changes in both directions where it makes sense), cross-probing and cross-selection | M |
| Footprint editor and parametric footprint generator: chip passives, SOT, SOIC, SSOP, QFP, QFN, BGA, through-hole, pin headers | L |
| Layer stack manager for 2 and 4 layers: materials, thicknesses, copper weights | S |
| Placement: move, rotate, flip to the other side, align and distribute, ratsnest | M |
| Interactive routing with walkaround, corner modes, width and via changes while routing, layer changes with vias | L |
| Copper pours: clearances, thermal reliefs, island removal, incremental repour | L |
| Design rules with the query language: clearance, width, via style, plane and pour connection, solder mask and paste expansion, hole sizes, board-edge clearance; presets matching common low-cost fabs' published capabilities | M |
| Online and batch design-rule checks, JSON report, violations browser | M |
| Fabrication outputs: Gerber X2, Excellon, IPC-2581, bill of materials, pick-and-place, fabrication and assembly PDF drawings | L |
| Our own Gerber and Excellon reader, used to verify outputs round-trip | M |
| EAGLE and KiCad board import, now editable | M |

**Gate:** reference boards 1 and 2 are designed in CircuitCraft, fabricated, assembled and
working.

### 0.7 3D and advanced routing

| Deliverable | Size |
|---|---|
| 3D board view: board body, copper, mask, silkscreen, component bodies generated from footprint parameters | L |
| STEP export of the assembled board; STEP import for component models (a permissively licensed STEP library, or our own) | XL |
| Push-and-shove routing, dragging of routed tracks and vias | XL |
| Differential pairs: rules, routing, gap control | L |
| Length tuning (accordion and trombone patterns) against length rules | M |
| Blind and buried vias, up to 8 layers | M |

**Gate:** reference board 3, with at least one differential pair routed with push-and-shove, is
fabricated and working; its STEP export opens correctly in a mechanical CAD tool.

### 0.9 Autorouter and simulation

| Deliverable | Size |
|---|---|
| Our own Rust autorouter on the rule engine: fanout, maze routing, rip-up and reroute, respects fixed routes | XL |
| Simulator interface; ngspice backend, run as a separate program the user installs (desktop only) | M |
| SPICE models attached to symbols, operating point, DC sweep, AC and transient analyses, waveform viewer | L |

**Gate:** reference board 1 autoroutes to a clean design-rule check; simulations of the
reference circuits agree with known or measured results.

### 1.0

Stability, documentation, signed installers for every platform, the first interface languages,
and every earlier gate still passing on the current build.

### After 1.0

Altium file import (once its legal footing is checked), variants, multi-channel design,
multi-board projects, harness design, impedance from the layer stack with a field solver,
panelization, our own simulator behind the simulator interface.

## Open decisions

| Decision | Options or notes | Needed by |
|---|---|---|
| Reference boards | Candidates: a 2-layer microcontroller board; a 4-layer ESP32 board with USB-C; a board with a USB high-speed differential pair. Prefer open designs so the community can rebuild them | 0.3 |
| Item ids across branches | Ids are a per-board counter (`next_id`), which keeps files small and stable but lets two branches hand out the same id for different items, so a git merge of two edited boards can collide. Options: random 128-bit ids, or ids made of an author or branch prefix plus a counter | native format v0 (0.1) |
| KiCad library content | CC-BY-SA-4.0 with an exception: check against the licence allowlist before importing it as bundled content | 0.5 |
| ngspice | Check its licence for running it as an external program | 0.9 |
| STEP library | Find a permissively licensed Rust STEP reader/writer and B-rep tessellator, or plan our own | 0.7 |
| Altium format work | Legal advice on studying Altium files made with your own licence, before any work starts | after 1.0 |
| Altium reference install | A licence for black-box observation, with screenshots kept out of the repository | 0.3 |

## Decided

| Date | Decision |
|---|---|
| 2026-11-13 | The name is CircuitCraft: the repository is public under it, and the check found only small, unrelated projects using it, no EDA product or trademark |
| 2026-11-13 | Polygon booleans and offsets use `i_overlay` (MIT OR Apache-2.0, pure Rust) with its `i64` engine on `Nm` coordinates: union, intersection, difference and XOR with fill rules, round-join offsets, deterministic integer results, builds for wasm. Its `i32` engine would cap designs at ±1.07 m. Arcs are approximated by segments; `cavalier_contours` stays an option for exact arc offsets |
| 2026-11-13 | EAGLE import is a hand-written, read-only XML reader, written from `eagle.dtd` and real files. The DTD is CC BY-ND 3.0 with a grant for programs that read or write the format; neither the DTD nor code generated from it goes into the repository |
| 2026-11-13 | The test corpus is a manifest of upstream designs pinned by commit and checksum, fetched on demand by a planned `cargo xtask corpus` command (part of 0.1, not written yet) and never redistributed by us. First candidates: SparkFun Qwiic Shield, RedBoard Turbo and MicroMod ESP32 (EAGLE, CC-BY-SA-4.0); Corne keyboard (KiCad, CC-BY-4.0); solo-hw (KiCad, CC-BY-SA-4.0); Bus Pirate 5 (KiCad, MIT); Olimex ESP32-POE and Antmicro Jetson Nano baseboard (KiCad, Apache-2.0) |
| 2026-11-13 | Native format: a project folder with one file per document (`.ccproj` project, `.ccsch` schematic sheet, `.ccpcb` board), written as pretty-printed JSON with a stable order so a re-save is byte-identical. The browser build opens and saves a zipped project folder |
| 2026-11-13 | Contributions are signed off with the Developer Certificate of Origin (`git commit -s`), described in CONTRIBUTING.md |
| 2026-11-13 | Accept the licences egui and eframe pull in outside the allowlist, per crate: `Unicode-3.0` (ICU4X and Unicode data crates), `Zlib` (foldhash, slotmap, zlib-rs), `BSL-1.0` (clipboard-win, error-code). `cargo xtask licenses` enforces the list in `xtask/src/licenses.rs` |

## Risks

| Risk | Why it matters | Mitigation |
|---|---|---|
| Router quality | Push-and-shove decides whether experienced designers take the tool seriously, and no open implementation can be used | Walkaround first; design the geometry and rule engine for shoving from 0.1; build from published research |
| Scope | 1.0 includes autorouting and simulation, which makes the road long | Public 0.x releases; 0.5 must be good enough on its own |
| Performance on large boards | Pours and checks on boards with thousands of nets must stay interactive | Integer geometry, spatial indexing, incremental checks and repour from the start |
| Parts content | Without footprints and symbols nobody can design anything | Parametric generator in 0.5; imports from EAGLE and KiCad libraries; an open decision on bundling KiCad's libraries |
| Import fidelity | EAGLE and KiCad users judge the app by how their existing designs open | Corpus-driven tests, round-trip checks through our own Gerber reader |
| Altium format work | Legal uncertainty and large effort | Deferred until after 1.0 and a legal check |

## Measuring progress

Once code exists, progress is measured the way the Crafting Apps measure it:

- `docs/parity-checklist.md`: Altium Designer features relevant to the target audience, one row
  each, built from black-box observation and the public documentation and written in our own
  words.
- `docs/file-format-parity.md`: what each importer and exporter covers.
- `docs/ui-parity.md`: interaction and layout fidelity.
- `docs/gaps.md`: everything still missing.

## Revision history

| Date | Change |
|---|---|
| 2026-11-12 | First version: audience, milestones 0.1 to 1.0, open decisions and risks |
| 2026-11-13 | Workspace scaffold landed; stage moved to 0.1 in progress |
| 2026-11-13 | egui licence exceptions accepted; licence check added to CI |
| 2026-11-13 | 0.1 decisions settled: name, polygon library, EAGLE reader terms, test corpus, native format, DCO sign-off |
| 2026-11-13 | Geometry landed: arcs, contours, polygon booleans, offsets and track strokes |
| 2026-11-13 | Board model v0 landed: nets, footprints and pads, tracks, vias, zones, text and drawings in copy-on-write collections, with validation and undo/redo |
