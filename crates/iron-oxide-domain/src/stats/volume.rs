//! Training volume (tonnage): the sum of weight × reps.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::PerformedSet;
use crate::{Reps, Unit, Weight};

const MAX_FORMAT_DECIMALS: u8 = 9;
const DEFAULT_DISPLAY_DECIMALS: u8 = 0;

/// Training volume: the sum of weight × reps over sets, exact.
///
/// # Representation
///
/// A `u128` count of **nanogram-reps**, the same storage unit as [`Weight`] times a rep count.
///
/// - It is not a [`Weight`]: a weight is a load capped at 2 000 kg, and a session's volume passes
///   that easily (5 × 5 × 140 kg is 3 500 kg).
/// - It is exact, like [`Weight`]: lb loads add up with no drift, and equal volumes compare equal.
/// - `u128` cannot overflow in practice. One set is at most 2 000 kg × 65 535 reps, about 1.3 × 10²⁰,
///   and `u128` holds 2.6 × 10¹⁸ such sets. A `u64` would overflow on a single absurd set.
///   Arithmetic is still checked ([`Volume::checked_add`]) or saturating ([`Volume::saturating_add`]),
///   never wrapping.
///
/// # Serde
///
/// A plain JSON integer of nanogram-reps, so values round-trip exactly. Use [`Volume::value_in`] or
/// [`Volume::display_in`] to show it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Volume(u128);

impl Volume {
    /// No volume.
    pub const ZERO: Self = Self(0);
    /// The largest representable volume.
    pub const MAX: Self = Self(u128::MAX);

    /// The volume of one set: `weight × reps`. Never overflows.
    #[must_use]
    pub fn of(weight: Weight, reps: Reps) -> Self {
        Self(u128::from(weight.as_nanograms()) * u128::from(reps.get()))
    }

    /// Wraps a count of nanogram-reps.
    #[must_use]
    pub const fn from_nanograms(nanograms: u128) -> Self {
        Self(nanograms)
    }

    /// The count of nanogram-reps.
    #[must_use]
    pub const fn as_nanograms(self) -> u128 {
        self.0
    }

    /// Whether the volume is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The sum, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    /// The sum, stopping at [`Volume::MAX`].
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    /// The volume as a number in `unit` (kg·reps or lb·reps), for charts and arithmetic at the edges.
    /// Very large values lose precision.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // display only
    pub fn value_in(self, unit: Unit) -> f64 {
        let per_unit = u128::from(unit.nanograms());
        (self.0 / per_unit) as f64 + (self.0 % per_unit) as f64 / per_unit as f64
    }

    /// The volume in kg·reps. See [`Volume::value_in`].
    #[must_use]
    pub fn as_kg(self) -> f64 {
        self.value_in(Unit::Kg)
    }

    /// The volume in lb·reps. See [`Volume::value_in`].
    #[must_use]
    pub fn as_lb(self) -> f64 {
        self.value_in(Unit::Lb)
    }

    /// The number in `unit`, rounded to at most `max_decimals` decimals (capped at 9; a halfway value
    /// rounds up) with trailing zeros trimmed: `3500`, `12345.5`. Integer arithmetic, so no float
    /// artefacts and no overflow.
    #[must_use]
    pub fn format_value(self, unit: Unit, max_decimals: u8) -> String {
        let decimals = max_decimals.min(MAX_FORMAT_DECIMALS);
        let scale = 10_u128.pow(u32::from(decimals));
        let per_unit = u128::from(unit.nanograms());
        let mut whole = self.0 / per_unit;
        // Both factors are below 10¹², so the product fits easily.
        let mut fraction = ((self.0 % per_unit) * scale + per_unit / 2) / per_unit;
        if fraction == scale {
            whole += 1;
            fraction = 0;
        }
        if fraction == 0 {
            return whole.to_string();
        }
        let digits = format!("{fraction:0width$}", width = usize::from(decimals));
        format!("{whole}.{}", digits.trim_end_matches('0'))
    }

    /// Displays the volume with its unit symbol: `3500 kg`. Whole units by default; the formatter
    /// precision overrides it (`format!("{:.1}", v.display_in(Unit::Lb))`).
    #[must_use]
    pub const fn display_in(self, unit: Unit) -> VolumeDisplay {
        VolumeDisplay { volume: self, unit }
    }
}

impl std::iter::Sum for Volume {
    /// Saturating sum (see [`Volume::saturating_add`]).
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Self::saturating_add)
    }
}

/// A [`Volume`] shown in a [`Unit`], built by [`Volume::display_in`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeDisplay {
    volume: Volume,
    unit: Unit,
}

impl fmt::Display for VolumeDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decimals = f.precision().map_or(DEFAULT_DISPLAY_DECIMALS, |precision| {
            u8::try_from(precision).unwrap_or(MAX_FORMAT_DECIMALS)
        });
        write!(
            f,
            "{} {}",
            self.volume.format_value(self.unit, decimals),
            self.unit.symbol()
        )
    }
}

/// The total volume of a session (or of any group of sets): weight × reps summed over working sets.
/// Warm-ups are left out; failed attempts (0 reps) add nothing.
///
/// Saturates at [`Volume::MAX`], which no real input can reach (see [`Volume`]).
pub fn session_volume(sets: impl IntoIterator<Item = PerformedSet>) -> Volume {
    sets.into_iter().map(PerformedSet::volume).sum()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::stats::test_support::{kg, reps, warm, work};

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    #[test]
    fn session_volume_sums_working_sets() {
        let sets = [
            warm(20.0, 10),
            warm(60.0, 5),
            work(140.0, 5),
            work(140.0, 5),
            work(140.0, 5),
            work(140.0, 5),
            work(140.0, 5),
        ];
        let volume = session_volume(sets);
        assert_eq!(volume, Volume::of(kg(3_500.0 / 25.0), reps(25)));
        assert_eq!(volume.as_kg(), 3_500.0);
        assert_eq!(volume.display_in(Unit::Kg).to_string(), "3500 kg");
    }

    #[test]
    fn session_volume_edge_cases() {
        assert_eq!(session_volume([]), Volume::ZERO);
        assert_eq!(session_volume([warm(60.0, 5), warm(80.0, 3)]), Volume::ZERO);
        assert_eq!(session_volume([work(100.0, 0)]), Volume::ZERO);
        assert_eq!(session_volume([work(0.0, 20)]), Volume::ZERO);
        assert!(session_volume([]).is_zero());
        assert!(!session_volume([work(20.0, 1)]).is_zero());
    }

    #[test]
    fn exceeds_the_weight_cap_without_overflow() {
        let one_set = Volume::of(Weight::MAX, Reps::MAX);
        assert_eq!(one_set.as_nanograms(), 2_000_000_000_000_000 * 65_535);
        let many = session_volume(std::iter::repeat_n(
            PerformedSet::working(Weight::MAX, Reps::MAX),
            1_000,
        ));
        assert_eq!(many.as_nanograms(), 2_000_000_000_000_000 * 65_535 * 1_000);
        assert_eq!(many.as_kg(), 2_000.0 * 65_535.0 * 1_000.0);
    }

    #[test]
    fn checked_and_saturating_addition() {
        let one = Volume::from_nanograms(1);
        assert_eq!(Volume::MAX.checked_add(one), None);
        assert_eq!(Volume::MAX.checked_add(Volume::ZERO), Some(Volume::MAX));
        assert_eq!(one.checked_add(one), Some(Volume::from_nanograms(2)));
        assert_eq!(Volume::MAX.saturating_add(one), Volume::MAX);
        assert_eq!([Volume::MAX, one].into_iter().sum::<Volume>(), Volume::MAX);
    }

    #[test]
    fn pounds_are_exact() {
        // 3 × 5 × 225 lb = 3 375 lb, with no drift.
        let sets = [PerformedSet::working(lb(225.0), reps(5)); 3];
        let volume = session_volume(sets);
        assert_eq!(volume, Volume::of(lb(3_375.0 / 5.0), reps(5)));
        assert_eq!(volume.as_lb(), 3_375.0);
        assert_eq!(volume.display_in(Unit::Lb).to_string(), "3375 lb");
    }

    #[test]
    fn formats_like_weight() {
        let volume = Volume::of(kg(102.5), reps(3));
        assert_eq!(volume.format_value(Unit::Kg, 2), "307.5");
        assert_eq!(volume.format_value(Unit::Kg, 0), "308");
        assert_eq!(format!("{:.1}", volume.display_in(Unit::Kg)), "307.5 kg");
        assert_eq!(format!("{:.40}", volume.display_in(Unit::Kg)), "307.5 kg");
        // 0.999 999 999 6 kg rounds up into the whole part at 9 decimals.
        let almost_one = Volume::from_nanograms(999_999_999_999);
        assert_eq!(almost_one.format_value(Unit::Kg, 9), "1");
        assert_eq!(almost_one.format_value(Unit::Kg, 12), "1");
        assert_eq!(Volume::ZERO.format_value(Unit::Kg, 2), "0");
        assert_eq!(
            Volume::MAX.format_value(Unit::Kg, 2),
            (u128::MAX / 1_000_000_000_000).to_string() + ".43"
        );
    }

    #[test]
    fn serde_is_an_exact_integer() {
        let volume = Volume::of(lb(225.0), reps(5));
        let json = serde_json::to_string(&volume).unwrap();
        assert_eq!(json, (453_592_370_000_u128 * 225 * 5).to_string());
        assert_eq!(serde_json::from_str::<Volume>(&json).unwrap(), volume);
        let max = serde_json::to_string(&Volume::MAX).unwrap();
        assert_eq!(serde_json::from_str::<Volume>(&max).unwrap(), Volume::MAX);
        assert!(serde_json::from_str::<Volume>("-1").is_err());
        assert!(serde_json::from_str::<Volume>("1.5").is_err());
    }

    proptest! {
        #[test]
        fn session_volume_is_the_sum_of_working_set_volumes(
            sets in prop::collection::vec(
                (0..=2_000_000_000_000_000_u64, any::<u16>(), any::<bool>()),
                0..20,
            ),
        ) {
            let sets: Vec<PerformedSet> = sets
                .into_iter()
                .map(|(ng, count, warmup)| PerformedSet {
                    weight: Weight::from_nanograms(ng).unwrap(),
                    reps: Reps::new(count),
                    warmup,
                })
                .collect();
            let expected: u128 = sets
                .iter()
                .filter(|set| !set.warmup)
                .map(|set| u128::from(set.weight.as_nanograms()) * u128::from(set.reps.get()))
                .sum();
            prop_assert_eq!(session_volume(sets).as_nanograms(), expected);
        }
    }
}
