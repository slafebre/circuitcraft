<h1 align="center">CircuitCraft</h1>

<p align="center">
  <b>Schematic capture and PCB layout: an open-source, modern electronics design application, built in pure Rust.</b>
</p>

<p align="center">
  The unified schematic-to-board workflow of a professional EDA suite, free, native on macOS,
  Windows, Linux and FreeBSD, in the browser via WebAssembly, and drivable by agents.
</p>

> [!IMPORTANT]
> **Status: scaffold.** The workspace, crate layers, checks, an empty app window, the CLI and an
> MCP server exist; nothing can open or edit a design yet. This README describes what CircuitCraft
> will be; [ROADMAP.md](ROADMAP.md) says in what order it gets built and what "done" means for
> each release.

## Quick start

CircuitCraft requires Rust 1.95 or later.

```sh
cargo run -p circuitcraft                              # the desktop app (an empty board for now)
cargo run -p circuitcraft-cli -- commands              # every command, as JSON
cargo run -p circuitcraft-cli -- exec board.info       # run one command
cargo run -p circuitcraft-cli -- info design.brd       # recognise an EAGLE or KiCad file
cargo xtask ci                                         # fmt, clippy, tests, assets, layers, wasm
cargo xtask bundle                                     # macOS: dist/CircuitCraft.app, for this machine
```

The browser build: `cd apps/circuitcraft-web && trunk serve` (needs [trunk](https://github.com/trunk-rs/trunk)).

<p align="center">
  <a href="#why-circuitcraft">Why CircuitCraft</a> ·
  <a href="#who-its-for-first">Who it's for first</a> ·
  <a href="#planned-scope">Planned scope</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#drive-it-from-agents-mcp-and-the-cli">Agents, MCP and the CLI</a> ·
  <a href="#clean-room">Clean room</a> ·
  <a href="#contributing">Contributing</a> ·
  <a href="#license">License</a>
</p>

## Why CircuitCraft

- **The professional workflow.** CircuitCraft follows the way of working of Altium Designer: one
  project holding schematics and a board, a compile step that turns sheets into nets, design
  changes pushed from schematic to board, cross-probing between them, a context-sensitive
  Properties panel, scoped design rules, and two-key shortcut sequences. Users of EAGLE or KiCad
  can switch to a keymap that matches their habits.
- **Every platform.** Professional EDA is mostly Windows-only. CircuitCraft is one native binary
  per platform (macOS universal, Windows, Linux, FreeBSD) plus a WebAssembly build, with no
  Electron and no web view.
- **Open, git-friendly files.** CircuitCraft saves to its own text format, written
  deterministically so designs diff and merge cleanly under version control. EAGLE and KiCad
  designs open directly. Fabrication outputs use open standards: Gerber X2, Excellon, IPC-2581.
- **Built for agents.** Every menu item, tool and shortcut is a command. Agents and scripts call
  the same commands over MCP, a JSON control channel or the CLI, inspect the design to check their
  work, and render it. Design-rule checks and fabrication outputs run headless, so hardware
  projects get CI the way software does.
- **Free.** MIT OR Apache-2.0, with no account, no subscription and no cloud lock-in.

## Who it's for first

Hobbyists, makers and small teams designing 2–4 layer boards for low-cost fabrication and
assembly services, including people moving off Autodesk EAGLE, which reached end of support in
June 2026. Professional features (push-and-shove routing, differential pairs, length tuning, 3D,
simulation) follow in the 0.x releases on the way to 1.0.

## Planned scope

| Area | Arrives in | Notes |
|---|---|---|
| Board viewer | 0.1 | Open EAGLE and KiCad boards read-only, layer by layer; render to PNG/SVG headless |
| Schematic capture | 0.3 | Symbols, wires, buses, net labels, power ports, hierarchical sheets, annotation, electrical rules check, symbol editor, PDF output |
| EAGLE and KiCad import | 0.3 (schematics), 0.5 (boards) | From the published format documentation only |
| PCB layout | 0.5 | Schematic-to-board sync, cross-probing, layer stack, placement, interactive routing (walkaround), vias, copper pours, scoped design rules, online and batch checks |
| Footprints | 0.5 | Footprint editor and a parametric generator for standard packages |
| Fabrication outputs | 0.5 | Gerber X2, Excellon, IPC-2581, bill of materials, pick-and-place, fabrication and assembly drawings |
| 3D and STEP | 0.7 | 3D board view with component bodies, STEP export for enclosure design |
| Advanced routing | 0.7 | Push-and-shove, differential pairs, length tuning |
| Autorouter | 0.9 | Our own, in Rust, on the same rule engine as the interactive router |
| Simulation | 0.9 | SPICE through a pluggable simulator interface; ngspice (installed separately) first, desktop only |
| Altium file import | After 1.0 | Only once its legal footing has been checked |

## Architecture

A Rust workspace in strict layers, the same shape as the ArtCraft Crafting Apps. Nothing below
the UI layer knows about egui, so the front end can be replaced.

```text
L0  units · geom · parsers         i64 nanometre coordinates, arcs, polygon booleans; s-expression and XML readers
L1  model                          project, sheets, components, nets, board, layer stack; copy-on-write store (cheap undo)
L2  library · rules · render       symbols, footprints, 3D bodies · rule query language · layered GPU display lists
L3  drc · pour · router · io · sim checks with a spatial index, copper pours, routing, file formats, simulator interface
L4  engine                         sessions, commands, project compile, schematic↔board sync, selection, undo
L5  ui-egui · mcp                  the swappable egui front end · MCP server
    apps: circuitcraft · circuitcraft-cli · circuitcraft-web
```

Decisions that shape everything else:

- **Integer coordinates.** Geometry is stored in `i64` nanometres, so design-rule checks and
  copper pours give exactly the same answer on every machine.
- **One stable ID per component**, shared by its schematic symbol and its footprint. Design
  changes, cross-probing and imports rely on it.
- **Rules are scoped by queries** in a small query language of our own (for example
  `net("GND") and layer(top)`), evaluated in priority order.
- **Everything is a command**, with a JSON form and an optional interactive form. Programmatic
  calls never open dialogs.
- **The UI is thin.** Panels read engine state and act only through commands.

## Drive it from agents, MCP and the CLI

`circuitcraft-cli commands`, `exec`, `info` and `mcp` (with the `list_commands` and `execute`
tools) work today, and so do the `file.open` and `file.save` commands for project folders. The
rest below is the 0.1 design sketch.

```sh
circuitcraft-cli info board.ccpcb                     # layers, nets, components, extents
circuitcraft-cli convert design.brd board.png         # open an EAGLE board, render it
circuitcraft-cli drc board.ccpcb --json               # design-rule check, machine-readable
circuitcraft-cli fab board.ccpcb --out fab/           # Gerber X2, drill, BOM, pick-and-place
circuitcraft-cli mcp                                  # MCP server on stdio, headless
circuitcraft-cli mcp --connect 127.0.0.1:7979         # bridged to the running app
```

MCP tools will include running any command with JSON, typing a shortcut sequence, inspecting and
querying the design, running checks, and rendering a view or a single layer to PNG.

## Clean room

CircuitCraft is written from scratch, from observed behaviour and public specifications:

- **Behaviour** comes from using Altium Designer as a black box and from its public
  documentation, described in our own words. No Altium code, help text, strings, icons, library
  content or artwork is copied, and nothing produced by Altium is committed.
- **File formats** come from public specifications (Gerber, Excellon, IPC-2581, STEP, EAGLE's
  XML, KiCad's published format documentation) and our own synthetic test files.
- **No GPL, LGPL or AGPL code** is copied, linked or read. That rules out KiCad's source, its
  router and importers, and FreeRouting.
- **Every asset** (icons, fonts, footprints, 3D bodies) is original or suitably licensed and
  recorded in `ATTRIBUTION.md` in the same change.
- **Never crash.** No panics and no `unsafe` in production code; every file and every parameter
  is treated as hostile input.

The full rules for contributors and agents will live in `AGENTS.md`, based on the ArtCraft
Crafting Apps house rules.

## Contributing

CircuitCraft is developed in the open from the start. Right now the most useful contributions are
on [ROADMAP.md](ROADMAP.md): its open decisions, the choice of reference boards,
and public EAGLE and KiCad designs with permissive licences for the test corpus. Contribution
guidelines are in [CONTRIBUTING.md](CONTRIBUTING.md); every commit is signed off with the
Developer Certificate of Origin (`git commit -s`).

## Relationship to the Crafting Apps

CircuitCraft borrows the architecture and conventions of the
[ArtCraft](https://getartcraft.com/) Crafting Apps (pure Rust, strict crate layering, everything
a command, agent control, clean room) and is built to their standards, so it can be offered to
the family later. It is not an ArtCraft project today.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in CircuitCraft shall be dual licensed as above, without any additional terms or
conditions.

CircuitCraft is an independent project. It is not affiliated with or endorsed by Altium Limited,
Autodesk, the KiCad project or the ArtCraft team. Altium Designer, EAGLE and KiCad are trademarks
of their respective owners.
