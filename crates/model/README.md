# circuitcraft-model

The design database of [CircuitCraft](https://github.com/slafebre/circuitcraft): a printed
circuit board and a schematic as plain Rust data, with validation, and undo for the board.

A `Board` holds its design settings, layer stack, outline and cutouts, nets, and its items:
footprints with pads, tracks, vias, copper zones, keepouts, holes, text and drawings. The
board's fields are public, its items are read through `get` and `iter`, and its types derive
`serde`'s `Serialize` and `Deserialize`, so a program can read a board directly or map it into
its own types. `Board::validate` checks a whole board,
for example one read from a file; `Document` keeps a board with undo and redo, recording what
each edit changed rather than a copy of the board.

A `schematic::Schematic` holds a project's sheets and one copy of each symbol they use: on
each sheet, components drawn as one part per unit, wires, buses, junctions, net labels and
ports, power ports, no-connect markers, sheet symbols, text and drawings, checked by
`Schematic::validate`.

It is published on its own as the type that
[`circuitcraft-import`](https://crates.io/crates/circuitcraft-import) returns.

- **No panics.** No `unwrap`, `expect`, `panic!` or `unsafe` (the lints deny them); a board's
  collections and names have limits, and going past one is an error.
- **Integer geometry.** Lengths are whole nanometres (`i64`), from
  [`circuitcraft-units`](https://crates.io/crates/circuitcraft-units); shapes come from
  [`circuitcraft-geom`](https://crates.io/crates/circuitcraft-geom).
- **Builds for WebAssembly.** New item ids are random UUIDs; in the browser their random bits
  come from the Web Crypto API.

## Versions

0.0.x: the model is still settling before CircuitCraft 0.1, and any 0.0.x release may change
the public API. Pin an exact version and read this section before you move to the next one.

| Version | Changes |
|---|---|
| 0.0.1 | First release |

## Licence

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
