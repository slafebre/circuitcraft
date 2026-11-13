//! Integer nanometre coordinates and unit conversions.
//!
//! Every length in a CircuitCraft design is a whole number of nanometres ([`Nm`]), so geometry,
//! design-rule checks and copper pours give the same answer on every machine. Floating point is
//! used only at the edges: parsing user input and file formats, and drawing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};

/// Nanometres per millimetre.
pub const NM_PER_MM: i64 = 1_000_000;
/// Nanometres per mil (a thousandth of an inch).
pub const NM_PER_MIL: i64 = 25_400;

/// The largest length a design may use: ±10 metres, far beyond any board, small enough that sums
/// and differences of coordinates never overflow `i64`.
pub const MAX_ABS: i64 = 10_000 * NM_PER_MM;

/// A unit conversion or arithmetic step failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UnitError {
    /// The value was NaN or infinite.
    #[error("{0} is not a finite number")]
    NotFinite(f64),
    /// The value is outside ±[`MAX_ABS`] nanometres.
    #[error("{0} nm is outside the ±10 m design range")]
    OutOfRange(f64),
    /// Integer arithmetic overflowed.
    #[error("length arithmetic overflowed")]
    Overflow,
}

/// A length in whole nanometres.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nm(pub i64);

impl Nm {
    /// Zero length.
    pub const ZERO: Nm = Nm(0);

    /// Converts millimetres, rounding to the nearest nanometre.
    pub fn from_mm(mm: f64) -> Result<Nm, UnitError> {
        Self::from_f64_nm(mm * NM_PER_MM as f64)
    }

    /// Converts mils, rounding to the nearest nanometre.
    pub fn from_mil(mil: f64) -> Result<Nm, UnitError> {
        Self::from_f64_nm(mil * NM_PER_MIL as f64)
    }

    /// Converts a length in nanometres given as a float (from geometry maths or a file), rounding
    /// to the nearest nanometre. NaN, infinities and values outside the design range are errors.
    pub fn from_f64_nm(nm: f64) -> Result<Nm, UnitError> {
        if !nm.is_finite() {
            return Err(UnitError::NotFinite(nm));
        }
        let rounded = nm.round();
        if rounded.abs() > MAX_ABS as f64 {
            return Err(UnitError::OutOfRange(rounded));
        }
        // In range and finite, so the cast is exact.
        Ok(Nm(rounded as i64))
    }

    /// This length in millimetres.
    pub fn to_mm(self) -> f64 {
        self.0 as f64 / NM_PER_MM as f64
    }

    /// This length in mils.
    pub fn to_mil(self) -> f64 {
        self.0 as f64 / NM_PER_MIL as f64
    }

    /// `self + rhs`, or an error on overflow.
    pub fn checked_add(self, rhs: Nm) -> Result<Nm, UnitError> {
        self.0.checked_add(rhs.0).map(Nm).ok_or(UnitError::Overflow)
    }

    /// `self - rhs`, or an error on overflow.
    pub fn checked_sub(self, rhs: Nm) -> Result<Nm, UnitError> {
        self.0.checked_sub(rhs.0).map(Nm).ok_or(UnitError::Overflow)
    }
}

/// A point on the board, in nanometres. `y` grows upwards, as on a fabrication drawing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Horizontal position.
    pub x: Nm,
    /// Vertical position.
    pub y: Nm,
}

impl Point {
    /// The origin.
    pub const ORIGIN: Point = Point { x: Nm::ZERO, y: Nm::ZERO };

    /// A point from two lengths.
    pub const fn new(x: Nm, y: Nm) -> Point {
        Point { x, y }
    }

    /// A point from millimetres.
    pub fn from_mm(x: f64, y: f64) -> Result<Point, UnitError> {
        Ok(Point { x: Nm::from_mm(x)?, y: Nm::from_mm(y)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mm_and_mil_round_trip() {
        assert_eq!(Nm::from_mm(1.0), Ok(Nm(1_000_000)));
        assert_eq!(Nm::from_mil(1.0), Ok(Nm(25_400)));
        assert_eq!(Nm::from_mm(0.1), Ok(Nm(100_000)));
        assert!((Nm(254_000).to_mil() - 10.0).abs() < 1e-12);
        assert!((Nm(1_500_000).to_mm() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn rounds_to_nearest_nanometre() {
        assert_eq!(Nm::from_mm(0.000_000_4), Ok(Nm(0)));
        assert_eq!(Nm::from_mm(0.000_000_6), Ok(Nm(1)));
        assert_eq!(Nm::from_mm(-0.000_000_6), Ok(Nm(-1)));
    }

    #[test]
    fn hostile_values_are_errors_not_panics() {
        assert!(matches!(Nm::from_mm(f64::NAN), Err(UnitError::NotFinite(_))));
        assert!(matches!(Nm::from_mm(f64::INFINITY), Err(UnitError::NotFinite(_))));
        assert!(matches!(Nm::from_mm(1e12), Err(UnitError::OutOfRange(_))));
        assert_eq!(Nm(i64::MAX).checked_add(Nm(1)), Err(UnitError::Overflow));
        assert_eq!(Nm(i64::MIN).checked_sub(Nm(1)), Err(UnitError::Overflow));
    }
}
