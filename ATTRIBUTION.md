# Attribution

Every asset file in this repository (images, icons, fonts, sounds, 3D models) has a row here with
its author, source and licence. `cargo xtask assets` fails when an asset file is missing from this
table. List each file by its path from the repository root, in backticks.

Licences must be on the allowlist in [AGENTS.md](AGENTS.md#licence-allowlist).

| File | Author | Source | Licence |
|---|---|---|---|
| `crates/render/src/font.rs` (the glyph table) | The CADCraft contributors | "CADCraft Stroke", an original single-stroke font, from [storytold/cadcraft](https://github.com/storytold/cadcraft) `crates/fonts/src/stroke.rs` at commit `fbb083803bf8e81677a2552bd386c1dffae0e46a`; widths written in tenths of a grid unit here | CC0-1.0 |

There are no asset files yet. Icons are drawn in code, and so is the stroke font above: it lives
in source code, not in a font file, and is listed because it is a font. Font files come from
outside the repository.
