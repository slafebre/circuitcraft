# circuitcraft-units

Integer nanometre coordinates and unit conversions, from
[CircuitCraft](https://github.com/slafebre/circuitcraft).

Every length in a CircuitCraft design is a whole number of nanometres (`Nm`, an `i64`), so
geometry, design-rule checks and copper pours give the same answer on every machine. Floating
point is used only at the edges: parsing input and file formats, and drawing. Conversions from
millimetres and mils refuse what is not a number or lies outside the ±10 m design range, and
arithmetic is checked, never panicking. No
`unsafe`. Builds for WebAssembly.

## Versions

0.0.x: any 0.0.x release may change the public API. Pin an exact version and read this section
before you move to the next one.

| Version | Changes |
|---|---|
| 0.0.1 | First release |

## Licence

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
