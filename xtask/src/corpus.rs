//! `cargo xtask corpus`: the public boards the importers are tested on.
//!
//! `corpus/manifest.json` pins each board by repository, commit, path and SHA-256. The designs
//! themselves are never committed and never redistributed: this command fetches them from where
//! their authors publish them, into `target/corpus/`, checks each file against its checksum,
//! imports it, and compares what came in with the counts the manifest expects. A board that fails
//! to import, or comes in with other counts than a person reviewed, fails the command.
//!
//! The manifest also has a place for the checksum of each board's reviewed render; it stays
//! empty until there is a renderer to make one.

use circuitcraft_model::{Board, Footprint, LayerId, NetId, Pad, PadShape, Side};
use circuitcraft_units::{Nm, Point};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The largest design file fetched or read.
const MAX_BYTES: u64 = 128 * 1024 * 1024;

/// The largest manifest read, and the most boards it may list: far more than the corpus will
/// ever hold, and small enough that a manifest gone wrong is refused before it is worked through.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_BOARDS: usize = 1_000;

/// How long a fetch may take to connect, and to finish, in seconds: a server that accepts the
/// connection and then sends nothing ends the fetch, not the patience of whoever runs it.
const CONNECT_SECONDS: &str = "30";
const FETCH_SECONDS: &str = "600";

/// One board of the manifest.
struct Entry {
    name: String,
    repo: String,
    commit: String,
    path: String,
    sha256: String,
    /// `kicad` or `eagle`.
    format: String,
    /// The counts a person reviewed, or `None` for a board that has none yet.
    counts: Option<Map<String, Value>>,
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let offline = args.iter().any(|a| a == "--offline");
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if let Some(unknown) = args.iter().find(|a| a.starts_with("--") && *a != "--offline") {
        return Err(format!("unknown option `{unknown}`; usage: cargo xtask corpus [--offline] [board ...]"));
    }
    let manifest = root.join("corpus").join("manifest.json");
    let text = String::from_utf8(read(&manifest, MAX_MANIFEST_BYTES)?).map_err(|_| format!("{}: not UTF-8 text", manifest.display()))?;
    let entries = entries(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    if let Some(missing) = only.iter().find(|name| !entries.iter().any(|e| e.name == ***name)) {
        return Err(format!("no board `{missing}` in the manifest"));
    }
    let mut failed = 0_usize;
    for entry in entries.iter().filter(|e| only.is_empty() || only.iter().any(|name| **name == e.name)) {
        match check(root, entry, offline) {
            Ok(counts) => println!("ok    {:<22} {}", entry.name, summary(&counts)),
            Err(why) => {
                failed += 1;
                println!("FAIL  {:<22} {why}", entry.name);
            }
        }
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} corpus board(s) failed")) }
}

/// The boards of a manifest, each field checked: what they say ends up in a URL, a file name and
/// the arguments of a program.
fn entries(text: &str) -> Result<Vec<Entry>, String> {
    let manifest: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let boards = manifest.get("boards").and_then(Value::as_array).ok_or("no `boards` list")?;
    if boards.len() > MAX_BOARDS {
        return Err(format!("more than {MAX_BOARDS} boards"));
    }
    let mut entries = Vec::with_capacity(boards.len());
    let mut names = BTreeSet::new();
    for board in boards {
        let field = |name: &str| board.get(name).and_then(Value::as_str).map(str::to_owned).ok_or(format!("a board without `{name}`"));
        let entry = Entry {
            name: field("name")?,
            repo: field("repo")?,
            commit: field("commit")?,
            path: field("path")?,
            sha256: field("sha256")?,
            format: field("format")?,
            counts: board.get("counts").and_then(Value::as_object).cloned(),
        };
        // Letters and digits, and after the first character a few marks: nothing that could be
        // read as an option, a parent folder or a host.
        let plain = |s: &str, extra: &str| {
            s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        let hex = |s: &str, len: usize| s.len() == len && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        let repo_ok = entry.repo.split('/').count() == 2 && entry.repo.split('/').all(|part| plain(part, "-_."));
        let path_ok =
            !entry.path.is_empty() && entry.path.split('/').all(|part| !part.is_empty() && part != "." && part != "..") && !entry.path.contains('\\');
        let format_ok = matches!(entry.format.as_str(), "kicad" | "eagle");
        if !plain(&entry.name, "-") || !repo_ok || !hex(&entry.commit, 40) || !hex(&entry.sha256, 64) || !path_ok || !format_ok {
            return Err(format!("board `{}`: a name, repository, commit, path, checksum or format is not in the form the manifest uses", entry.name));
        }
        if !names.insert(entry.name.clone()) {
            return Err(format!("board `{}` is listed twice", entry.name));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Fetches (unless it is there already), verifies, imports and counts one board.
fn check(root: &Path, entry: &Entry, offline: bool) -> Result<Map<String, Value>, String> {
    let file = file_of(root, entry);
    if !file.is_file() {
        if offline {
            return Err("not fetched yet (run without --offline)".to_owned());
        }
        fetch(entry, &file)?;
    }
    let bytes = read(&file, MAX_BYTES)?;
    let found = hex(&Sha256::digest(&bytes));
    if found != entry.sha256 {
        return Err(format!("checksum {found}, expected {}; delete {} to fetch it again", entry.sha256, file.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let imported = circuitcraft_io::import_board(&text, &entry.name).map_err(|e| format!("import failed: {e}"))?;
    let counts = counts(&imported.board);
    match &entry.counts {
        Some(expected) if *expected == counts => Ok(counts),
        Some(expected) => Err(format!("counts differ from the manifest: {}", differences(expected, &counts))),
        None => Err(format!("the manifest has no reviewed counts yet; it came in as {}", Value::Object(counts))),
    }
}

/// Where a board's file is kept: `target/corpus/<name>/board.kicad_pcb` or `board.brd`. The
/// name on disk is ours, not the repository's, so no path in a manifest names a place on disk.
fn file_of(root: &Path, entry: &Entry) -> PathBuf {
    let file_name = if entry.format == "kicad" { "board.kicad_pcb" } else { "board.brd" };
    root.join("target").join("corpus").join(&entry.name).join(file_name)
}

/// Downloads a board from its repository at the pinned commit, with `curl`.
fn fetch(entry: &Entry, file: &Path) -> Result<(), String> {
    let folder = file.parent().ok_or("no folder")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let url = format!("https://raw.githubusercontent.com/{}/{}/{}", entry.repo, entry.commit, percent_encoded(&entry.path));
    let part = file.with_extension("part");
    let status = Command::new("curl")
        // No redirects: the file is where the manifest says, or the fetch fails.
        .args(["--fail", "--silent", "--show-error", "--proto", "=https", "--connect-timeout", CONNECT_SECONDS, "--max-time", FETCH_SECONDS])
        .args(["--max-filesize", &MAX_BYTES.to_string(), "--output"])
        .arg(&part)
        .arg(&url)
        .status()
        .map_err(|e| format!("could not start curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not fetch {url} ({status})"));
    }
    std::fs::rename(&part, file).map_err(|e| format!("{}: {e}", file.display()))
}

/// A path for a URL: everything but letters, digits, `-`, `.`, `_`, `~` and `/` as `%XX`.
fn percent_encoded(path: &str) -> String {
    path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect()
}

/// A whole file of at most `limit` bytes. The read itself stops one byte past the limit, so a
/// file that is larger, or grows while it is read, is refused without being held in memory.
fn read(file: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let failed = |e: std::io::Error| format!("{}: {e}", file.display());
    let mut bytes = Vec::new();
    std::fs::File::open(file).map_err(failed)?.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(failed)?;
    if bytes.len() as u64 > limit {
        return Err(format!("{}: larger than {limit} bytes", file.display()));
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the manifest records of an imported board.
fn counts(board: &Board) -> Map<String, Value> {
    let counts = json!({
        "copper": board.stack.copper_count(),
        "nets": board.nets.len(),
        "footprints": board.footprints.len(),
        "pads": board.footprints.iter().map(|f| f.pads.len()).sum::<usize>(),
        "pads_reached": pads_reached(board),
        "tracks": board.tracks.len(),
        "vias": board.vias.len(),
        "zones": board.zones.len(),
        "keepouts": board.keepouts.len(),
        "holes": board.holes.len(),
        "texts": board.texts.len(),
        "drawings": board.graphics.len(),
        "outline": board.outline.is_some(),
        "cutouts": board.cutouts.len(),
    });
    counts.as_object().cloned().unwrap_or_default()
}

/// A track end or a via: where it is, and the copper layers it is on, as places in the stack
/// counted from the top, first and last.
struct End {
    at: Point,
    layers: (usize, usize),
}

/// How many pads have a track end or a via of their own net on their copper: on a layer where
/// the pad has copper, and within the copper's shape there. Every other count stays the same
/// when footprints are placed turned or mirrored the wrong way, or their pads land on the wrong
/// layer; this one drops.
fn pads_reached(board: &Board) -> usize {
    let copper: Vec<LayerId> = board.stack.copper().map(|l| l.id).collect();
    let place = |id: LayerId| copper.iter().position(|c| *c == id);
    let mut ends: BTreeMap<NetId, Vec<End>> = BTreeMap::new();
    for track in board.tracks.iter() {
        if let (Some(net), Some(layer)) = (track.net, place(track.layer)) {
            let (from, to) = track.path.ends();
            ends.entry(net).or_default().extend([from, to].map(|at| End { at, layers: (layer, layer) }));
        }
    }
    for via in board.vias.iter() {
        if let (Some(net), Some(from), Some(to)) = (via.net, place(via.from), place(via.to)) {
            ends.entry(net).or_default().push(End { at: via.position, layers: (from.min(to), from.max(to)) });
        }
    }
    let bottom = copper.len().saturating_sub(1);
    let reached =
        |f: &Footprint, pad: &Pad| pad.net.and_then(|net| ends.get(&net)).is_some_and(|ends| ends.iter().any(|end| on_copper(f, pad, end, bottom)));
    board.footprints.iter().map(|f| f.pads.iter().filter(|pad| reached(f, pad)).count()).sum()
}

/// Whether `end` is on the copper of `pad`, on a board whose last copper layer is at `bottom`.
fn on_copper(footprint: &Footprint, pad: &Pad, end: &End, bottom: usize) -> bool {
    // The end as the copper sees it: in the footprint, with the pad's turn undone, from the
    // middle of the copper.
    let Some(local) = footprint.to_footprint(end.at) else { return false };
    let from_pad = Point::new(Nm(local.x.0.saturating_sub(pad.position.x.0)), Nm(local.y.0.saturating_sub(pad.position.y.0)));
    let Some(unturned) = pad.rotation.reversed().turn(from_pad) else { return false };
    let (x, y) = (unturned.x.0.saturating_sub(pad.copper_offset.x.0), unturned.y.0.saturating_sub(pad.copper_offset.y.0));
    // The pad's own side is the top of the stack for a footprint on the top.
    let (own, other) = if footprint.side == Side::Top { (0, bottom) } else { (bottom, 0) };
    let faces = [
        (pad.front.as_ref().map(|face| &face.shape), own, own),
        (pad.back.as_ref().map(|face| &face.shape), other, other),
        (pad.inner.as_ref(), 1, bottom.saturating_sub(1)),
    ];
    faces.into_iter().any(|(shape, first, last)| {
        let on_layer = first <= last && first <= end.layers.1 && end.layers.0 <= last;
        on_layer && shape.is_some_and(|shape| within(shape, x, y))
    })
}

/// Whether the point `(x, y)`, measured from a copper shape's middle, is on the shape, its edge
/// included (for a custom shape, a point exactly on the edge may count either way).
fn within(shape: &PadShape, x: i64, y: i64) -> bool {
    // A box with its corners rounded by `radius`: on it when no further than the radius from the
    // box that is left when the radius is taken off every side. A rectangle has no radius, an
    // oval half its narrow side, a circle half its diameter.
    let rounded = |width: Nm, height: Nm, radius: i64| {
        // In 128 bits from the start and saturating at the end, so no coordinate or size
        // overflows on the way, whatever it is.
        let radius = i128::from(radius);
        let beyond = |v: i64, half: i64| (i128::from(v).abs() - (i128::from(half) - radius)).max(0);
        let (dx, dy) = (beyond(x, width.0 / 2), beyond(y, height.0 / 2));
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) <= radius.saturating_mul(radius)
    };
    match shape {
        PadShape::Circle { diameter } => rounded(*diameter, *diameter, diameter.0 / 2),
        PadShape::Rect { width, height } => rounded(*width, *height, 0),
        PadShape::RoundRect { width, height, radius } => rounded(*width, *height, radius.0),
        PadShape::Oval { width, height } => rounded(*width, *height, width.0.min(height.0) / 2),
        // An odd number of the outline's edges to the right of the point: it is inside.
        PadShape::Custom { outline } => outline.flatten(Nm(10_000)).is_ok_and(|ring| {
            let crossings = (0..ring.len()).filter(|&i| {
                let (Some(a), Some(b)) = (ring.get(i), ring.get((i + 1) % ring.len())) else { return false };
                let wide = |v: i64, from: i64| i128::from(v) - i128::from(from);
                let side = wide(b.x.0, a.x.0).saturating_mul(wide(y, a.y.0)).saturating_sub(wide(b.y.0, a.y.0).saturating_mul(wide(x, a.x.0)));
                (a.y.0 > y) != (b.y.0 > y) && (side > 0) == (b.y.0 > a.y.0)
            });
            crossings.count() % 2 == 1
        }),
    }
}

/// `tracks 806 (expected 800), vias 74 (expected 70)`.
fn differences(expected: &Map<String, Value>, found: &Map<String, Value>) -> String {
    let keys: std::collections::BTreeSet<&String> = expected.keys().chain(found.keys()).collect();
    let differing: Vec<String> = keys
        .into_iter()
        .filter(|key| expected.get(*key) != found.get(*key))
        .map(|key| format!("{key} {} (expected {})", found.get(key).unwrap_or(&Value::Null), expected.get(key).unwrap_or(&Value::Null)))
        .collect();
    differing.join(", ")
}

fn summary(counts: &Map<String, Value>) -> String {
    let of = |key: &str| counts.get(key).cloned().unwrap_or(Value::Null);
    format!(
        "{} footprints, {} pads ({} reached by their net), {} tracks, {} vias, {} zones, outline {}",
        of("footprints"),
        of("pads"),
        of("pads_reached"),
        of("tracks"),
        of("vias"),
        of("zones"),
        of("outline")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"boards": [{"name": "demo-board", "repo": "owner/repo.name", "commit": "0123456789abcdef0123456789abcdef01234567",
        "path": "Hardware/My Board.brd", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "format": "eagle",
        "counts": {"tracks": 1}}]}"#;

    #[test]
    fn a_manifest_entry_is_read_and_its_fields_checked() {
        let found = entries(GOOD).expect("entries");
        let entry = found.first().expect("one board");
        assert_eq!((entry.name.as_str(), entry.repo.as_str(), entry.path.as_str()), ("demo-board", "owner/repo.name", "Hardware/My Board.brd"));
        assert_eq!(entry.counts.as_ref().and_then(|c| c.get("tracks")), Some(&json!(1)));
        assert_eq!(file_of(Path::new("/r"), entry), Path::new("/r/target/corpus/demo-board/board.brd"));
        assert_eq!(percent_encoded(&entry.path), "Hardware/My%20Board.brd");
        assert_eq!(percent_encoded("a/b?c#d%é"), "a/b%3Fc%23d%25%C3%A9");
        // Each field is in the form the manifest uses: nothing that changes the URL's host, leaves
        // the corpus folder or looks like an option.
        for (from, to) in [
            ("demo-board", "../demo"),
            ("demo-board", "--output"),
            ("owner/repo.name", "owner/repo/extra"),
            ("owner/repo.name", "owner@evil.example/repo"),
            ("owner/repo.name", "../repo"),
            ("0123456789abcdef0123456789abcdef01234567", "main"),
            ("Hardware/My Board.brd", "Hardware/../../secret"),
            ("Hardware/My Board.brd", "/etc/passwd"),
            ("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "E3B0"),
            ("\"eagle\"", "\"../x\""),
        ] {
            assert!(entries(&GOOD.replacen(from, to, 1)).is_err(), "{from} -> {to}");
        }
        let twice = GOOD.replacen("]}", ", {\"name\": \"demo-board\", \"repo\": \"a/b\", \"commit\": \"0123456789abcdef0123456789abcdef01234567\", \"path\": \"x\", \"sha256\": \"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\", \"format\": \"kicad\"}]}", 1);
        assert!(entries(&twice).is_err_and(|e| e.contains("twice")));
        assert!(entries("{}").is_err() && entries("[").is_err());
        // A manifest with more boards than the corpus could hold is refused before any is read.
        let crowded = format!("{{\"boards\": [{}]}}", vec!["{}"; MAX_BOARDS + 1].join(","));
        assert!(entries(&crowded).is_err_and(|e| e.contains("more than")));
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(read(&missing, 10).is_err_and(|e| e.contains("larger than 10 bytes")));
        // A file exactly as long as the limit is read; a byte longer is not.
        let length = read(&missing, MAX_MANIFEST_BYTES).expect("read").len() as u64;
        assert!(read(&missing, length).is_ok() && read(&missing, length - 1).is_err());
    }

    /// A footprint at (10, 10) mm, turned 20 degrees, on `side`.
    fn footprint(side: Side) -> Footprint {
        use circuitcraft_model::{Assembly, ItemId, Mounting, Rotation};
        Footprint {
            id: ItemId::from_u128(1),
            reference: "U1".to_owned(),
            value: String::new(),
            link: None,
            assembly: Assembly { mounting: Mounting::Other, in_bom: true, in_placement: true, fitted: true },
            position: Point::new(Nm(10_000_000), Nm(10_000_000)),
            rotation: Rotation(20_000),
            side,
            pads: Vec::new(),
            graphics: Vec::new(),
            texts: Vec::new(),
        }
    }

    /// Where the point `(x, y)` micrometres of a pad's copper is on the board, worked out the way
    /// the model defines it: from the copper's middle, turned with the pad, placed in the
    /// footprint, and put on the board by the footprint.
    fn on_board(footprint: &Footprint, pad: &Pad, x: i64, y: i64) -> Point {
        let in_pad = Point::new(Nm(x * 1_000 + pad.copper_offset.x.0), Nm(y * 1_000 + pad.copper_offset.y.0));
        let turned = pad.rotation.turn(in_pad).expect("in range");
        footprint.to_board(Point::new(Nm(turned.x.0 + pad.position.x.0), Nm(turned.y.0 + pad.position.y.0))).expect("in range")
    }

    #[test]
    fn an_end_is_on_a_pad_when_it_is_on_its_copper_on_a_layer_that_has_some() {
        use circuitcraft_model::{Hole, Rotation};
        let mm = |x: i64, y: i64| Point::new(Nm(x * 1_000_000), Nm(y * 1_000_000));
        let thin = PadShape::Rect { width: Nm(2_000_000), height: Nm(200_000) };
        // A drilled pad on a four-layer board: thin copper, turned 30 degrees against its
        // footprint and sitting off its hole, in a footprint on the bottom turned 20 degrees.
        let bottom = footprint(Side::Bottom);
        let mut drilled = Pad::through_hole("1", mm(3, -2), thin.clone(), Hole::round(Nm(100_000), true));
        (drilled.rotation, drilled.copper_offset) = (Rotation(30_000), Point::new(Nm(500_000), Nm(300_000)));
        let at = |x: i64, y: i64, layers: (usize, usize)| on_copper(&bottom, &drilled, &End { at: on_board(&bottom, &drilled, x, y), layers }, 3);
        // On the copper along its length, on every layer; off it across its width, on none.
        for layer in 0..4 {
            assert!(at(900, 0, (layer, layer)) && at(-900, 90, (layer, layer)), "layer {layer}");
            assert!(!at(0, 900, (layer, layer)) && !at(900, 110, (layer, layer)) && !at(1_010, 0, (layer, layer)), "layer {layer}");
        }
        // The same on the top side.
        let top = footprint(Side::Top);
        assert!(on_copper(&top, &drilled, &End { at: on_board(&top, &drilled, 900, 90), layers: (0, 0) }, 3));
        assert!(!on_copper(&top, &drilled, &End { at: on_board(&top, &drilled, 900, 110), layers: (0, 0) }, 3));
        // A surface pad has copper on its footprint's own side only: the last layer for a
        // footprint on the bottom. A via reaches it when its span comes down to that layer.
        let mut surface = Pad::smd("2", mm(3, -2), thin.clone());
        surface.rotation = Rotation(30_000);
        let on = |pad: &Pad, layers: (usize, usize), last: usize| on_copper(&bottom, pad, &End { at: on_board(&bottom, pad, 900, 0), layers }, last);
        assert_eq!([(3, 3), (0, 0), (1, 2), (0, 3), (1, 3), (0, 2)].map(|layers| on(&surface, layers, 3)), [true, false, false, true, true, false]);
        assert!(on_copper(&top, &surface, &End { at: on_board(&top, &surface, 900, 0), layers: (0, 0) }, 3));
        assert!(!on_copper(&top, &surface, &End { at: on_board(&top, &surface, 900, 0), layers: (3, 3) }, 3));
        // Copper on the inner layers alone is reached there, and nowhere on a board without them.
        let buried = Pad { front: None, back: None, ..drilled.clone() };
        assert_eq!([(0, 0), (1, 1), (2, 2), (3, 3), (0, 3)].map(|layers| on(&buried, layers, 3)), [false, true, true, false, true]);
        assert_eq!([(0, 0), (1, 1), (0, 1)].map(|layers| on(&buried, layers, 1)), [false; 3]);
        // A shape of its own is reached within its outline, not within the box around it: the
        // triangle is half a millimetre to either side at its middle height, and a point at its
        // top.
        let triangle = circuitcraft_geom::Contour::polyline(&[mm(-1, -1), mm(1, -1), mm(0, 1)]).expect("triangle");
        let custom = Pad::smd("3", mm(0, 0), PadShape::Custom { outline: triangle });
        let round = Pad::smd("4", mm(0, 0), PadShape::Circle { diameter: Nm(1_000_000) });
        let inside = |pad: &Pad, x: i64, y: i64| on_copper(&bottom, pad, &End { at: on_board(&bottom, pad, x, y), layers: (3, 3) }, 3);
        assert_eq!(
            [inside(&custom, 900, -900), inside(&custom, 0, 900), inside(&custom, 400, 0), inside(&custom, 600, 0)],
            [true, true, true, false]
        );
        assert_eq!([inside(&custom, -990, 990), inside(&custom, 1_100, 0), inside(&custom, 0, -1_100)], [false; 3]);
        // An oval and a rounded rectangle, 2 mm by 1 mm: a point near a corner is in the box
        // around them and off their copper.
        let oval = Pad::smd("5", mm(0, 0), PadShape::Oval { width: Nm(2_000_000), height: Nm(1_000_000) });
        assert_eq!([inside(&oval, 900, 200), inside(&oval, 990, 0), inside(&oval, 950, 400), inside(&oval, 0, 510)], [true, true, false, false]);
        let upright = Pad::smd("6", mm(0, 0), PadShape::Oval { width: Nm(1_000_000), height: Nm(2_000_000) });
        assert_eq!([inside(&upright, 200, 900), inside(&upright, 400, 950)], [true, false]);
        let softened = PadShape::RoundRect { width: Nm(2_000_000), height: Nm(1_000_000), radius: Nm(200_000) };
        let softened = Pad::smd("7", mm(0, 0), softened);
        assert_eq!(
            [
                inside(&softened, 900, 400),
                inside(&softened, 990, 0),
                inside(&softened, 0, 490),
                inside(&softened, 950, 450),
                inside(&softened, 1_010, 0)
            ],
            [true, true, true, false, false]
        );
        // A point level with a corner of an outline is judged by the edges that meet there:
        // inside a diamond at the height of its side corners, and outside beyond them.
        let diamond = circuitcraft_geom::Contour::polyline(&[mm(-1, 0), mm(0, -1), mm(1, 0), mm(0, 1)]).expect("diamond");
        let diamond = PadShape::Custom { outline: diamond };
        assert_eq!([within(&diamond, 500_000, 0), within(&diamond, -500_000, 0), within(&diamond, 0, 0)], [true; 3]);
        assert_eq!([within(&diamond, 1_100_000, 0), within(&diamond, -1_100_000, 0), within(&diamond, 600_000, 600_000)], [false; 3]);
        // Sizes and places no board has get an answer too, not an overflow.
        for (width, radius) in [(i64::MAX, i64::MIN), (i64::MIN, i64::MAX), (i64::MIN, i64::MIN), (i64::MAX, i64::MAX)] {
            let huge = PadShape::RoundRect { width: Nm(width), height: Nm(width), radius: Nm(radius) };
            let _ = [within(&huge, i64::MIN, i64::MIN), within(&huge, i64::MAX, i64::MIN), within(&huge, 0, 0)];
        }
        assert!(!within(&diamond, i64::MIN, i64::MAX) && !within(&diamond, i64::MAX, 0));
        // A rectangle keeps its corners.
        let square = Pad::smd("8", mm(0, 0), PadShape::Rect { width: Nm(2_000_000), height: Nm(1_000_000) });
        assert_eq!([inside(&square, 990, 490), inside(&square, 1_010, 0), inside(&square, 0, 510)], [true, false, false]);
        assert_eq!([inside(&round, 300, 300), inside(&round, 0, 490), inside(&round, 450, 450), inside(&round, 510, 0)], [true, true, false, false]);
    }

    #[test]
    fn counts_are_compared_field_by_field() {
        let board = Board::new("x");
        let found = counts(&board);
        assert_eq!(found.get("copper"), Some(&json!(2)));
        assert_eq!(found.get("outline"), Some(&json!(false)));
        let mut expected = found.clone();
        expected.insert("tracks".to_owned(), json!(5));
        expected.remove("vias");
        assert_eq!(differences(&expected, &found), "tracks 0 (expected 5), vias 0 (expected null)");
        assert_eq!(differences(&found, &found), "");
        // A pad is reached when a track of its own net ends on its copper, wherever its
        // footprint puts it.
        let text = "(kicad_pcb (version 20221018) (net 1 \"A\") (net 2 \"B\") (net 3 \"C\") (net 4 \"D\")
  (footprint \"a:b\" (layer \"B.Cu\") (at 10 10 90)
    (pad \"1\" smd rect (at 1 2 90) (size 1 1) (layers \"B.Cu\") (net 1 \"A\"))
    (pad \"2\" smd rect (at -1 2 90) (size 1 1) (layers \"B.Cu\") (net 2 \"B\"))
    (pad \"3\" smd rect (at 5 5 90) (size 1 1) (layers \"B.Cu\")))
  (footprint \"a:c\" (layer \"F.Cu\") (at 40 10 90)
    (pad \"1\" smd rect (at 0 0 90) (size 2 0.2) (layers \"F.Cu\") (net 3 \"C\"))
    (pad \"2\" smd circle (at 5 0 90) (size 1 1) (layers \"F.Cu\") (net 4 \"D\")))
  (segment (start 0 0) (end 12.4 9) (width 0.2) (layer \"B.Cu\") (net 1))
  (segment (start 0 0) (end 12 11) (width 0.2) (layer \"B.Cu\") (net 1))
  (via (at 30 30) (size 0.8) (drill 0.4) (layers \"F.Cu\" \"B.Cu\") (net 2))
  (segment (start 0 0) (end 40.9 10) (width 0.2) (layer \"F.Cu\") (net 3))
  (segment (start 0 0) (end 40 5) (width 0.2) (layer \"B.Cu\") (net 4)))";
        let reached = |text: &str| {
            let board = circuitcraft_io::import_board(text, "x").expect("import").board;
            (counts(&board).get("pads").cloned(), counts(&board).get("pads_reached").cloned())
        };
        // Pad 1 of the first footprint is at (12, 9) on the board, a millimetre square: the first
        // track ends on it. The second ends where pad 2 is, on another net; pad 2's own via is
        // far away. In the second footprint, the long thin pad stands upright at (40, 10) and a
        // track ends 0.9 mm to its side, off its copper; the round pad at (40, 5) is on the top
        // and a track of its net ends right under it, on the bottom.
        assert_eq!(reached(text), (Some(json!(5)), Some(json!(1))));
        // Pad 2 of the first footprint is on the bottom; its net's via, moved onto it, reaches it
        // at the lower end of its span.
        assert_eq!(reached(&text.replace("(via (at 30 30)", "(via (at 12 11)")).1, Some(json!(2)));
        // The same track ending 0.9 mm along the thin pad is on it, and a via under the round pad
        // reaches it from either side.
        let along = text.replace("(end 40.9 10)", "(end 40 10.9)");
        assert_eq!(reached(&along).1, Some(json!(2)));
        let joined = along.replace(
            "(segment (start 0 0) (end 40 5) (width 0.2) (layer \"B.Cu\") (net 4))",
            "(via (at 40.3 5.3) (size 0.8) (drill 0.4) (layers \"F.Cu\" \"B.Cu\") (net 4))",
        );
        assert_eq!(reached(&joined).1, Some(json!(3)));
        // Outside the round pad's circle, though inside the square around it.
        assert_eq!(reached(&joined.replace("(via (at 40.3 5.3)", "(via (at 40.45 5.45)")).1, Some(json!(2)));
        assert_eq!(hex(&Sha256::digest(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn the_repository_manifest_is_well_formed() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("corpus").join("manifest.json");
        let text = std::fs::read_to_string(&path).expect("corpus/manifest.json");
        let found = entries(&text).expect("a well-formed manifest");
        assert!(found.len() >= 8, "{} boards", found.len());
        assert!(found.iter().all(|e| e.counts.is_some()), "every board has reviewed counts");
        // Every board says under which licence it is published and who made it.
        let manifest: Value = serde_json::from_str(&text).expect("json");
        for board in manifest["boards"].as_array().expect("boards") {
            for field in ["license", "by", "format"] {
                assert!(board[field].as_str().is_some_and(|v| !v.is_empty()), "{} has no {field}", board["name"]);
            }
            assert!(board.get("render").is_some(), "{} has no place for its render's checksum", board["name"]);
        }
    }
}
