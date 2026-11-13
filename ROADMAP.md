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
| ✅ Native format v0: read and write, deterministic, with a version field | S |
| Undo by change set: an edit records what it added, removed and changed, in place of a snapshot of the whole board, once per command; every edit returns its change set and a revision number, for the renderer, the spatial index and incremental checks; a benchmark in CI holds the cost (see Risks) | M |
| ✅ `model` v1, what real boards contain: slotted and unplated holes, pad shapes per layer, solder mask and paste expansion, via tenting; footprint text (designators and values), library link and assembly attributes; courtyard, fabrication, keepout and mechanical layers, with top and bottom layers paired for flipping; zone fills, holes, keepouts and thermal settings; board design settings; an outline assembled from loose lines and arcs; holes placed on the board outside a footprint | M |
| ✅ Native format hardening: one item per line, unknown and missing fields refused, a value that comes in kinds written under its kind so it is read without holding the rest of its object in memory, the temporary file synced to disk before it replaces the old one. The format is unstable until 0.1 ships; from then on golden files pin it and every change comes with a migration | S |
| Sessions hold a project: its documents, the selection and the view state | M |
| Typed command parameters with a generated JSON Schema, and flags for read-only and desktop-only commands; MCP publishes the schemas | M |
| Storage behind a trait: `io` reads and writes bytes, so the browser build can open and save | S |
| EAGLE `.brd` and KiCad board import, read-only | M |
| Renderer: display lists in `render`; layered 2D GPU view, layer visibility and colours, single-layer mode, net highlight, pan and zoom; SVG and PNG output without a GPU, for the CLI, MCP and CI | M |
| `cargo xtask corpus`: fetches the pinned designs; the manifest records each board's expected item counts and the checksum of its reviewed render | S |
| Fuzz and property tests for every file reader and for the geometry operations | S |
| Autosave and recovery (a panic in the browser aborts, so the panic guard can't help there) | S |
| Stroke font for silkscreen text (reusable from the Crafting Apps) | S |
| `circuitcraft-cli` (`info`, `convert`, `run`) and MCP (`open`, `inspect`, `query`, `render`) | S |
| Desktop app shell and web build | S |

**Gate:** every board in a corpus of permissively licensed public EAGLE and KiCad boards opens
without a crash from the app, the CLI and MCP; its item counts match the corpus manifest; and its
headless render matches the checksum of a render a person has reviewed.

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
| Reference boards | Candidates: a 2-layer microcontroller board; a 4-layer ESP32 board with USB-C; a board with a USB high-speed differential pair. Prefer open designs so the community can rebuild them. They decide what `model` v1 has to hold (a USB-C connector needs slotted holes, for example) | 0.1 |
| A 0.2 release: check and order an imported board | Proposed: pull pour fill, Gerber X2 and Excellon output, our own Gerber and Excellon reader and basic checks against fab presets forward from 0.5. EAGLE and KiCad users get something useful before schematic capture, and import fidelity gets an objective test against the reference Gerbers many corpus repositories ship. The cost: schematic capture starts later | before 0.1 ships |
| What 1.0 includes | Keep the autorouter and SPICE simulation in 1.0, or move them to 1.x so that 1.0 means fabricated boards plus push-and-shove routing | 0.5 |
| Hosted web demo | Publish the browser build with 0.1, the cheapest way to show the project. Publishing needs the maintainer's yes | 0.1 |
| FreeBSD | The README names it and CI doesn't build it: add a CI job or drop the claim | 0.1 |
| KiCad library content | CC-BY-SA-4.0 with an exception: check against the licence allowlist before importing it as bundled content. Schematic capture needs symbols, so this is due with 0.3, not 0.5 | 0.3 |
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
| 2026-11-13 | Native format: a project folder with one file per document (`.ccproj` project, `.ccsch` schematic sheet, `.ccpcb` board), written as JSON with a stable order so a re-save is byte-identical. The browser build opens and saves a zipped project folder |
| 2026-11-13 | Item and net ids are random version 4 UUIDs, written in the usual hyphenated form, so items added on two git branches never share an id. They replace the per-board counter, which collided across branches and would have conflicted on every parallel edit. `uuid` (formatting and parsing only) and `getrandom` (MIT OR Apache-2.0) supply them; in the browser the random bits come from the Web Crypto API |
| 2026-11-13 | Native files put every item on a line of its own (a net, a track, a pad, a corner of a zone), so a change to one item changes one line and branches that change different items merge. A value that comes in kinds is written under its kind (`{"line": {"from": …, "to": …}}`, `"rules"`) instead of carrying a `kind` field, so the reader knows the kind before it reads the rest. A field that is missing or unknown is refused; a field that may be empty is written as `null` |
| 2026-11-13 | Contributions are signed off with the Developer Certificate of Origin (`git commit -s`), described in CONTRIBUTING.md |
| 2026-11-13 | Accept the licences egui and eframe pull in outside the allowlist, per crate: `Unicode-3.0` (ICU4X and Unicode data crates), `Zlib` (foldhash, slotmap, zlib-rs), `BSL-1.0` (clipboard-win, error-code). `cargo xtask licenses` enforces the list in `xtask/src/licenses.rs` |

## Risks

| Risk | Why it matters | Mitigation |
|---|---|---|
| Router quality | Push-and-shove decides whether experienced designers take the tool seriously, and no open implementation can be used | Walkaround first; design the geometry and rule engine for shoving from 0.1; build from published research |
| Scope | 1.0 includes autorouting and simulation, which makes the road long | Public 0.x releases; 0.5 must be good enough on its own |
| Performance on large boards | Pours and checks on boards with thousands of nets must stay interactive. Measured on 2026-11-13 with whole-board undo snapshots: adding 100,000 tracks, one command each, took 134 s and 2.3 GB of memory (20,000 tracks: 4 s and 457 MB) | Undo by change set and a benchmark budget in CI (0.1); integer geometry, spatial indexing, incremental checks and repour from the start |
| Time to a useful release | Nothing helps an EAGLE or KiCad user until 0.5, after all of schematic capture | Public 0.x releases; the proposed 0.2 release (open decision) |
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
| 2026-11-13 | Native format v0 landed: project folders with `.ccproj` and `.ccpcb` files, random UUID item ids, `file.open` and `file.save` commands |
| 2026-11-13 | Architecture, hardware and product review: 0.1 gains undo by change set, `model` v1, format hardening, project sessions, typed command parameters, storage behind a trait, rendering without a GPU, the corpus command, fuzz tests and autosave, and its gate is now measurable. A 0.2 release, the scope of 1.0, a web demo and FreeBSD are open decisions; the reference boards are due with 0.1 and KiCad library content with 0.3 |
| 2026-11-13 | `model` v1, first part: a pad has a front face, inner copper, a back face and an optional hole, which may be a slot or unplated, with the copper off centre around it; pads and vias carry solder mask openings, and pads paste openings. Every rotation in a board file is checked. Holes outside a footprint (EAGLE boards have them) joined the row's remaining work |
| 2026-11-13 | `model` v1, second part: courtyard, fabrication, keepout and mechanical layers; a layer on a side of the board is paired with its counterpart on the other side, and copper layers are mirrored by their place in the stack, so a footprint's drawings can flip to the other side. Refusing missing fields joined the format hardening row |
| 2026-11-13 | `model` v1, third part: a footprint holds its texts, each showing the reference, the value or words of its own, on a layer, at a place, visible or hidden; a link to the library footprint it was placed from; and what assembly needs: how it is mounted, and whether it is in the bill of materials, in the pick-and-place file and fitted. Reading a value tagged with `kind` without holding the rest of its object in memory joined the format hardening row |
| 2026-11-13 | `model` v1, last part, and native format hardening. A zone holds its holes, its fill (solid or hatched), how it joins pads (solid, thermal relief or not at all), its narrowest copper, whether islands stay, and the copper it was last poured with. A keepout says which copper layer it is on and whether it keeps out tracks, vias or zones. The board holds design settings (mask and paste expansion, via tenting, clearance, track and via sizes, the origin outputs measure from) and holes that belong to no footprint. `outline.assemble` turns the drawings on the outline layer into the outline and its cutouts. Files have one item per line and refuse unknown and missing fields. Left for the importers: keepouts and outline drawings held by a footprint |
