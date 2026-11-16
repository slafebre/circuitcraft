# circuitcraft-import

Reads printed circuit boards that KiCad and EAGLE wrote, and schematics that KiCad wrote, into
validated models your program can map into its own. It is part of [CircuitCraft](https://github.com/slafebre/circuitcraft),
published on its own so that other design tools can open the boards people already have.

```rust
let text = "(kicad_pcb (version 20221018) (generator pcbnew))"; // the file's contents
let imported = circuitcraft_import::import_board(text, "My board")?;
let board = imported.board; // a circuitcraft_model::Board that has passed Board::validate
for line in imported.notes.lines() {
    eprintln!("left out: {line}"); // "3 × dimension left out"
}
```

A schematic is a tree of files; you hand over the top one and a function that gives the others
by the names its sheets use, so the crate itself never touches a file system:

```rust
let imported = circuitcraft_import::import_schematic(&top, "My project", &mut |name| {
    std::fs::read_to_string(folder.join(name)).ok() // or from memory, a zip, the network
})?;
let schematic = imported.schematic; // a circuitcraft_model::schematic::Schematic, validated
```

[`detect`] tells a file's format from its first bytes, so a renamed file is still recognised;
[`kicad::read_board`], [`kicad::read_schematic`] and [`eagle::read_board`] read one format each.

## What it reads

| Format | Versions |
|---|---|
| KiCad boards (`.kicad_pcb`) | the files KiCad 5 to 9 write; a newer file is read with a note |
| KiCad schematics (`.kicad_sch`) | the files KiCad 6 to 9 write, with every sheet below the top one; a newer file is read with a note |
| EAGLE boards (`.brd`) | the XML format of EAGLE 6 and later |

What comes out: the layer stack, the board outline and cutouts, nets, footprints with their
pads, texts and drawings already placed on the board, tracks (straight and arcs), vias, copper
zones, keepouts, holes, text and drawings, and the design settings the file states. A footprint
on the bottom is held as seen from the top, with its side recorded. EAGLE sizes that a file
leaves to its design rules (annular rings, mask and paste openings, via tenting) are worked out
from those rules.

From a schematic: every sheet, one copy of each symbol the sheets use (with its units, pins
and their electrical types), components with their designators, values, footprints and fields,
each drawn as one part per unit (on any sheet), wires, buses and bus entries, junctions, net
labels, global labels and hierarchical ports, power ports, no-connect markers, sheet symbols
with their entries, text and drawings. Designators are the ones KiCad records for each sheet's
place in the tree; a sheet used more than once is read once. Power symbols become power ports at
their supply pin; KiCad's power flag, which has no supply pin, stays an ordinary component.

An import is lossy by design: anything the model can't hold is left out and listed in
[`Notes`], with how often it happened, never dropped silently. KiCad 5 schematics (`.sch`) are
recognised and refused with a message; EAGLE schematics and libraries and KiCad libraries are
recognised by [`detect`] but not read yet.

## What it promises

- **No panics on any input.** No `unwrap`, `expect`, `panic!` or `unsafe` in the crate (the
  lints deny them). Allocations, recursion and loops are bounded, so a hostile file gets an
  error, not a crash or a hang. CircuitCraft's fuzz tests run each reader on thousands of
  damaged files from a fixed seed and check that everything that comes out is valid, and that
  every board survives a save and reload in CircuitCraft's own format.
- **Integer geometry.** Every length is a whole number of nanometres (`i64`), so results are
  the same on every machine.
- **Checked against real boards.** CircuitCraft's corpus check imports a set of openly
  licensed public KiCad and EAGLE boards and compares the counts of what comes out with the
  counts it expects.
- **Builds for WebAssembly** (`wasm32-unknown-unknown`): no file system access, no threads;
  you pass the file's text.

## Where the code comes from

The readers are clean-room work: written from the formats' public documentation (KiCad's
published file format description, EAGLE's document type definition) and from real board files,
never from KiCad's or EAGLE's source code. No GPL, LGPL or AGPL code is copied, linked or read.
The tests use synthetic files of our own (`samples/`).

## Versions

0.0.x: the model is still settling before CircuitCraft 0.1, and any 0.0.x release may change
the public API. Pin an exact version and read this section before you move to the next one.

| Version | Changes |
|---|---|
| 0.0.1 | First release: KiCad and EAGLE boards, KiCad schematics |

## Licence

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).

[`detect`]: https://docs.rs/circuitcraft-import/latest/circuitcraft_import/fn.detect.html
[`kicad::read_board`]: https://docs.rs/circuitcraft-import/latest/circuitcraft_import/kicad/fn.read_board.html
[`eagle::read_board`]: https://docs.rs/circuitcraft-import/latest/circuitcraft_import/eagle/fn.read_board.html
[`kicad::read_schematic`]: https://docs.rs/circuitcraft-import/latest/circuitcraft_import/kicad/fn.read_schematic.html
[`Notes`]: https://docs.rs/circuitcraft-import/latest/circuitcraft_import/struct.Notes.html
