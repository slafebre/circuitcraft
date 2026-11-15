# circuitcraft-geom

Planar geometry on integer nanometre coordinates, from
[CircuitCraft](https://github.com/slafebre/circuitcraft): bounding boxes, circular arcs,
closed contours of straight and arc edges, polygons with holes, outlines assembled from loose
lines and arcs, and polygon booleans, clearance offsets and track shapes on
[`i_overlay`](https://crates.io/crates/i_overlay)'s 64-bit integer engine, deterministic on every
platform.

Every coordinate is a whole number of nanometres; anything outside the ±10 m design range is an
error, never a panic. No `unwrap`, `expect`, `panic!` or `unsafe` (the lints deny them). Builds
for WebAssembly.

## Versions

0.0.x: any 0.0.x release may change the public API. Pin an exact version and read this section
before you move to the next one.

| Version | Changes |
|---|---|
| 0.0.1 | First release |

## Licence

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
