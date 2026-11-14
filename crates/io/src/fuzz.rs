//! Fuzz tests for every file reader: thousands of damaged copies of good files, made from a
//! fixed seed so every run and every machine tries the same ones.
//!
//! What must hold for each: the reader returns a board or an error, never a panic or a hang; a
//! board it returns is valid; and that board survives a save and a reload unchanged, so nothing
//! an import produces is something our own format can't hold.

use crate::{eagle, kicad, native};
use circuitcraft_model::Board;

/// A small random number generator (xorshift64*), seeded per test.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `n` (`n` at least 1).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Text that readers tend to trip over: brackets and quotes of both formats, numbers at and past
/// every limit, words the formats give meaning to.
const SPLINTERS: &[&str] = &[
    "(",
    ")",
    "((((((((",
    "))))",
    "\"",
    "\\",
    "<",
    ">",
    "</",
    "/>",
    "&",
    "&amp;",
    "&#0;",
    "=",
    " ",
    "\n",
    "0",
    "-1",
    "1e308",
    "-1e308",
    "1e-320",
    "nan",
    "inf",
    "9999999999999",
    "-9223372036854775808",
    "18446744073709551616",
    "0.0000001",
    "360",
    "-90",
    "(at 0 0 45)",
    "(layer \"B.Cu\")",
    "(layers \"*.Cu\")",
    "(net 99)",
    "(pts (xy 0 0))",
    "(size 0 0)",
    "(drill oval 1 1)",
    "(angle 400)",
    "hide",
    "locked",
    "curve=\"359.9\"",
    "curve=\"-180\"",
    "rot=\"MR45\"",
    "layer=\"43\"",
    "layer=\"16\"",
    "width=\"0\"",
    "drill=\"0\"",
    "x=\"1e30\"",
    "extent=\"16-1\"",
    "smashed=\"yes\"",
    "<vertex x=\"0\" y=\"0\"/>",
    "<hole x=\"0\" y=\"0\" drill=\"1\"/>",
    "null",
    "{",
    "}",
    "[",
    "]",
    ":",
    ",",
    "\"id\"",
    "\"layer\": 999",
    "-0",
    "\u{feff}",
    "\u{e9}",
    "\u{0}",
];

/// Numbers to put where a file has a number: the readers' arithmetic is where a value at or past
/// a limit does its damage, and a changed number leaves the file well formed.
const NUMBERS: &[&str] = &[
    "0",
    "-0",
    "1",
    "-1",
    "0.000001",
    "90",
    "180",
    "270",
    "360",
    "-45",
    "9999.999999",
    "-9999.999999",
    "10000",
    "10000.000001",
    "1e30",
    "-1e30",
    "1e-30",
    "nan",
    "inf",
    "65536",
    "4294967296",
    "9223372036854775807",
    "-9223372036854775808",
];

/// A copy of `sample` with one to four random changes. Half of them put another number where a
/// number stands; the rest cut a piece out, double one, or put a splinter in or over something;
/// now and then the end is cut off.
fn damaged(sample: &str, random: &mut Random) -> String {
    let mut bytes = sample.as_bytes().to_vec();
    for _ in 0..=random.below(4) {
        let at = random.below(bytes.len());
        let longest = if random.below(4) == 0 { 400 } else { 12 };
        let length = 1 + random.below(longest);
        let end = (at + length).min(bytes.len());
        let splinter = SPLINTERS.get(random.below(SPLINTERS.len())).copied().unwrap_or_default().as_bytes();
        match random.below(12) {
            0..=5 => renumber(&mut bytes, at, random),
            6 | 7 => drop(bytes.drain(at..end)),
            8 => {
                let piece = bytes.get(at..end).unwrap_or_default().to_vec();
                bytes.splice(at..at, piece);
            }
            9 => drop(bytes.splice(at..at, splinter.iter().copied())),
            10 => drop(bytes.splice(at..end, splinter.iter().copied())),
            _ => bytes.truncate(at),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Replaces the first number at or after `at` with one of [`NUMBERS`].
fn renumber(bytes: &mut Vec<u8>, at: usize, random: &mut Random) {
    let numeric = |b: &u8| b.is_ascii_digit() || matches!(b, b'.' | b'-');
    let Some(start) = bytes.iter().skip(at).position(|b| b.is_ascii_digit()).map(|i| at + i) else { return };
    let end = bytes.iter().skip(start).position(|b| !numeric(b)).map_or(bytes.len(), |i| start + i);
    let number = NUMBERS.get(random.below(NUMBERS.len())).copied().unwrap_or("0").as_bytes();
    bytes.splice(start..end, number.iter().copied());
}

/// A board that came out of a reader: valid, and the same after a save and a reload.
fn holds_up(board: &Board, what: &str) {
    assert_eq!(board.validate(), Ok(()), "{what}: an invalid board came out");
    let saved = native::write_board(board).unwrap_or_else(|e| panic!("{what}: can't be saved: {e}"));
    let back = native::read_board(&saved).unwrap_or_else(|e| panic!("{what}: saved but can't be reloaded: {e}"));
    assert!(back == *board, "{what}: changed by a save and a reload");
}

/// Runs `read` on `rounds` damaged copies of each sample and returns how many still gave a board.
fn storm(samples: &[&str], seed: u64, rounds: usize, read: impl Fn(&str) -> Option<Board>) -> usize {
    let mut random = Random(seed);
    let mut read_fine = 0;
    for (which, sample) in samples.iter().enumerate() {
        holds_up(&read(sample).unwrap_or_else(|| panic!("sample {which} doesn't read")), "the sample itself");
        for round in 0..rounds {
            let text = damaged(sample, &mut random);
            if let Some(board) = read(&text) {
                read_fine += 1;
                holds_up(&board, &format!("sample {which}, round {round}"));
            }
        }
    }
    read_fine
}

#[test]
fn damaged_kicad_boards_are_read_or_refused() {
    let read_fine =
        storm(&[kicad::tests::BOARD, kicad::tests::OLD], 0x5eed_0001, 3_000, |text| kicad::read_board(text, "fuzz").ok().map(|i| i.board));
    // The reader leaves out what it can't read, so many damaged files still give a board. If few
    // did, the test would be exercising the parser alone: a third of them must.
    assert!(read_fine > 2_000, "only {read_fine} of 6,000 damaged files still read");
}

#[test]
fn damaged_eagle_boards_are_read_or_refused() {
    let read_fine = storm(&[eagle::tests::BOARD], 0x5eed_0002, 6_000, |text| eagle::read_board(text, "fuzz").ok().map(|i| i.board));
    // An XML document is spoilt by smaller damage than a list of items is: a quarter must read.
    assert!(read_fine > 1_500, "only {read_fine} of 6,000 damaged files still read");
}

#[test]
fn damaged_native_boards_are_read_or_refused() {
    // Our own format, written from the boards the importers make: the widest mix of items.
    let samples: Vec<String> =
        [kicad::read_board(kicad::tests::BOARD, "a"), kicad::read_board(kicad::tests::OLD, "b"), eagle::read_board(eagle::tests::BOARD, "c")]
            .into_iter()
            .map(|imported| native::write_board(&imported.expect("import").board).expect("write"))
            .collect();
    let samples: Vec<&str> = samples.iter().map(String::as_str).collect();
    // A strict format refuses nearly every damaged file; the ones it reads must still hold up.
    storm(&samples, 0x5eed_0003, 2_000, |text| native::read_board(text).ok());
    let mut random = Random(0x5eed_0004);
    let project = native::write_project(&native::Project { name: "P".to_owned(), board: "P.ccpcb".to_owned() }).expect("write");
    for _ in 0..2_000 {
        if let Ok(read) = native::read_project(&damaged(&project, &mut random)) {
            assert!(!read.name.trim().is_empty() && read.board.ends_with(".ccpcb"), "{read:?}");
        }
    }
}

#[test]
fn damaged_files_of_any_kind_are_told_apart_or_not_at_all() {
    let mut random = Random(0x5eed_0005);
    for sample in [kicad::tests::BOARD, eagle::tests::BOARD] {
        for _ in 0..3_000 {
            let text = damaged(sample, &mut random);
            // Whatever `detect` makes of it, importing by content agrees or refuses.
            let _ = crate::detect(text.as_bytes());
            if let Ok(imported) = crate::import_board(&text, "fuzz") {
                assert_eq!(imported.board.validate(), Ok(()));
            }
        }
    }
}
