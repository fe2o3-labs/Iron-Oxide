//! Plate maths: which plates to put on each side of the bar to reach a target load.
//!
//! - [`PlateInventory`] is the user's set of plates: each plate size with the number of **pairs**
//!   available (a plate is always loaded on both sides, so pairs are what counts).
//! - [`calculate_plates`] finds the loadout for a target. When the target cannot be loaded exactly it
//!   returns the closest loadout at or below the target and the closest one above it.
//!
//! Every weight is an exact [`Weight`], so kg plates, lb plates and a mix of both are handled with
//! integer arithmetic and no tolerance.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::units::Unit;
use crate::weight::Weight;

/// A plate size and how many pairs of it are available. Serialized as
/// `{"plate": 20.0, "pairs": 4}`, with the plate as a kg number like every [`Weight`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlateStock {
    /// The weight of one plate.
    pub plate: Weight,
    /// The number of pairs available. Zero is allowed: the plate is listed but not usable.
    pub pairs: u32,
}

/// Why a plate inventory was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlateInventoryError {
    /// A plate of zero weight was listed.
    #[error("a plate must weigh more than zero")]
    ZeroPlate,
    /// The same plate size was listed twice.
    #[error("plate size {} is listed more than once", .0.display_in(Unit::Kg))]
    DuplicatePlate(Weight),
    /// More distinct plate sizes than [`PlateInventory::MAX_SIZES`].
    #[error("at most {max} plate sizes are allowed")]
    TooManySizes {
        /// The maximum number of distinct plate sizes.
        max: usize,
    },
}

/// The plates a user can load: plate size → number of pairs available.
///
/// Sizes are unique, non-zero and kept heaviest first. kg and lb plates may be mixed.
///
/// # Serde
///
/// A JSON array of [`PlateStock`], heaviest first: `[{"plate": 25.0, "pairs": 4}, ...]`. The same
/// validation runs on deserialization, and the input order does not matter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<PlateStock>", into = "Vec<PlateStock>")]
pub struct PlateInventory {
    /// Sorted heaviest first, sizes unique and non-zero.
    stock: Vec<PlateStock>,
}

impl PlateInventory {
    /// The most distinct plate sizes an inventory may hold. Real gyms have fewer than ten; the cap
    /// keeps the exact search in [`calculate_plates`] small.
    pub const MAX_SIZES: usize = 16;

    /// Builds an inventory from plate sizes and pair counts, in any order.
    ///
    /// # Errors
    /// [`PlateInventoryError::ZeroPlate`] for a zero plate, [`PlateInventoryError::DuplicatePlate`]
    /// for a size listed twice and [`PlateInventoryError::TooManySizes`] above
    /// [`PlateInventory::MAX_SIZES`].
    pub fn new(stock: impl IntoIterator<Item = PlateStock>) -> Result<Self, PlateInventoryError> {
        let mut stock: Vec<PlateStock> = stock.into_iter().collect();
        if stock.iter().any(|s| s.plate.is_zero()) {
            return Err(PlateInventoryError::ZeroPlate);
        }
        stock.sort_by_key(|s| std::cmp::Reverse(s.plate));
        if let Some(pair) = stock.windows(2).find(|w| w[0].plate == w[1].plate) {
            return Err(PlateInventoryError::DuplicatePlate(pair[0].plate));
        }
        if stock.len() > Self::MAX_SIZES {
            return Err(PlateInventoryError::TooManySizes {
                max: Self::MAX_SIZES,
            });
        }
        Ok(Self { stock })
    }

    /// An inventory with no plates: only the bar can be lifted.
    #[must_use]
    pub const fn empty() -> Self {
        Self { stock: Vec::new() }
    }

    /// A sensible starting inventory for a gym in `unit`:
    ///
    /// - kg: 25 × 4 pairs, 20 × 2, 15, 10, 5, 2.5 and 1.25 × 1 pair each.
    /// - lb: 45 × 6 pairs, 35 × 1, 25 × 1, 10 × 2, 5 × 1 and 2.5 × 1.
    ///
    /// The small plates reach every 2.5 kg (or 5 lb) step.
    #[must_use]
    pub fn default_for(unit: Unit) -> Self {
        let sizes: &[(f64, u32)] = match unit {
            Unit::Kg => &[
                (25.0, 4),
                (20.0, 2),
                (15.0, 1),
                (10.0, 1),
                (5.0, 1),
                (2.5, 1),
                (1.25, 1),
            ],
            Unit::Lb => &[
                (45.0, 6),
                (35.0, 1),
                (25.0, 1),
                (10.0, 2),
                (5.0, 1),
                (2.5, 1),
            ],
        };
        // Every size is a small, positive, distinct literal, so neither conversion can fail; the
        // tests check that all of them are present.
        let stock = sizes.iter().filter_map(|&(value, pairs)| {
            Weight::new(value, unit)
                .ok()
                .map(|plate| PlateStock { plate, pairs })
        });
        Self::new(stock).unwrap_or_default()
    }

    /// The plate sizes with their pair counts, heaviest first.
    #[must_use]
    pub fn stock(&self) -> &[PlateStock] {
        &self.stock
    }

    /// The number of pairs of `plate`, zero when the size is not in the inventory.
    #[must_use]
    pub fn pairs_of(&self, plate: Weight) -> u32 {
        self.stock
            .iter()
            .find(|s| s.plate == plate)
            .map_or(0, |s| s.pairs)
    }

    /// Whether no plate can be loaded (no sizes, or zero pairs of every size).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stock.iter().all(|s| s.pairs == 0)
    }
}

impl TryFrom<Vec<PlateStock>> for PlateInventory {
    type Error = PlateInventoryError;

    fn try_from(stock: Vec<PlateStock>) -> Result<Self, Self::Error> {
        Self::new(stock)
    }
}

impl From<PlateInventory> for Vec<PlateStock> {
    fn from(inventory: PlateInventory) -> Self {
        inventory.stock
    }
}

/// A number of plates of one size on **each** side of the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlateCount {
    /// The weight of one plate.
    pub plate: Weight,
    /// How many of them go on each side.
    pub per_side: u32,
}

/// One way to load the bar: the plates on each side and the resulting total.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Loadout {
    plates: Vec<PlateCount>,
    per_side: Weight,
    total: Weight,
}

impl Loadout {
    /// The bar with no plates.
    const fn bar_only(bar: Weight) -> Self {
        Self {
            plates: Vec::new(),
            per_side: Weight::ZERO,
            total: bar,
        }
    }

    /// The plates on each side, heaviest first, one entry per size used (never a zero count).
    #[must_use]
    pub fn plates(&self) -> &[PlateCount] {
        &self.plates
    }

    /// Every plate on one side, one item per plate, heaviest first (the order they go on the bar).
    pub fn plates_one_by_one(&self) -> impl Iterator<Item = Weight> + '_ {
        self.plates
            .iter()
            .flat_map(|c| std::iter::repeat_n(c.plate, c.per_side as usize))
    }

    /// The number of plates on one side.
    #[must_use]
    pub fn plate_count_per_side(&self) -> u32 {
        self.plates.iter().map(|c| c.per_side).sum()
    }

    /// The weight of the plates on one side.
    #[must_use]
    pub const fn per_side(&self) -> Weight {
        self.per_side
    }

    /// The achieved total: bar plus both sides.
    #[must_use]
    pub const fn total(&self) -> Weight {
        self.total
    }
}

/// How the closest loadout compares with the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlateOutcome {
    /// The target is loaded exactly.
    Exact,
    /// The closest loadout is lighter than the target by this much.
    Under(Weight),
    /// The closest loadout is heavier than the target by this much.
    Over(Weight),
}

/// The answer of [`calculate_plates`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlateResult {
    target: Weight,
    loadouts: Loadouts,
}

/// Which loadouts exist. At least one always does, so [`PlateResult::closest`] never fails.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Loadouts {
    Exact(Loadout),
    /// Both a lighter and a heavier loadout exist.
    Between {
        below: Loadout,
        above: Loadout,
    },
    /// The target is beyond what the inventory can load.
    BelowOnly(Loadout),
    /// The target is lighter than the bar.
    AboveOnly(Loadout),
}

impl PlateResult {
    /// The requested total.
    #[must_use]
    pub const fn target(&self) -> Weight {
        self.target
    }

    /// Whether the target can be loaded exactly.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self.loadouts, Loadouts::Exact(_))
    }

    /// The loadout that hits the target exactly, if there is one.
    #[must_use]
    pub const fn exact(&self) -> Option<&Loadout> {
        match &self.loadouts {
            Loadouts::Exact(exact) => Some(exact),
            _ => None,
        }
    }

    /// The heaviest loadout at or below the target (the exact one when it exists). `None` when
    /// the target is lighter than the bar.
    #[must_use]
    pub const fn below(&self) -> Option<&Loadout> {
        match &self.loadouts {
            Loadouts::Exact(below)
            | Loadouts::Between { below, .. }
            | Loadouts::BelowOnly(below) => Some(below),
            Loadouts::AboveOnly(_) => None,
        }
    }

    /// The lightest loadout strictly above the target. `None` when the target is exact or beyond
    /// what the inventory can load.
    #[must_use]
    pub const fn above(&self) -> Option<&Loadout> {
        match &self.loadouts {
            Loadouts::Between { above, .. } | Loadouts::AboveOnly(above) => Some(above),
            Loadouts::Exact(_) | Loadouts::BelowOnly(_) => None,
        }
    }

    /// The loadout nearest to the target. On a tie the lighter one wins: it is the safer miss.
    #[must_use]
    pub fn closest(&self) -> &Loadout {
        match &self.loadouts {
            Loadouts::Exact(only) | Loadouts::BelowOnly(only) | Loadouts::AboveOnly(only) => only,
            Loadouts::Between { below, above } => {
                let under = self.target.abs_diff(below.total);
                let over = self.target.abs_diff(above.total);
                if over < under { above } else { below }
            }
        }
    }

    /// How the [closest](Self::closest) loadout compares with the target.
    #[must_use]
    pub fn outcome(&self) -> PlateOutcome {
        let total = self.closest().total;
        match total.cmp(&self.target) {
            Ordering::Equal => PlateOutcome::Exact,
            Ordering::Less => PlateOutcome::Under(self.target.abs_diff(total)),
            Ordering::Greater => PlateOutcome::Over(self.target.abs_diff(total)),
        }
    }
}

/// A partial or complete choice of pair counts during the search.
#[derive(Debug, Clone)]
struct Candidate {
    /// Weight of one side, in nanograms.
    side: u64,
    /// Plates on one side.
    plates: u64,
    /// Pairs used per inventory entry, in inventory order (heaviest first).
    counts: Vec<u32>,
}

impl Candidate {
    /// Among loadouts of the same weight: fewer plates first, then more of the heavier plates.
    fn is_nicer_than(&self, other: &Self) -> bool {
        match self.plates.cmp(&other.plates) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => self.counts > other.counts,
        }
    }
}

/// Finds how to load `target` on a bar of weight `bar` with the plates in `inventory`.
///
/// The same plates go on both sides, so each side carries `(target - bar) / 2`. The search is
/// exact: it never uses more pairs than the inventory has, and it finds the true closest loadouts
/// where a greedy heaviest-first pick can miss (e.g. 40 kg per side from one 25 and two 20s).
///
/// - Exact target: [`PlateResult::exact`].
/// - Otherwise: the heaviest loadout at or below the target and the lightest one above it. The
///   first is missing when the target is lighter than the bar, the second when the target is
///   beyond the inventory.
/// - Among loadouts of the same weight, the one with the fewest plates wins, then the one with the
///   most heavy plates (30 kg per side is 25 + 5, not 15 + 15).
///
/// Runs a dynamic programme over the reachable per-side weights, one plate size at a time. Only
/// weights up to half the target are kept, so the work is bounded by the number of distinct
/// reachable side weights (a few hundred for a real gym) times the pair counts.
#[must_use]
pub fn calculate_plates(target: Weight, bar: Weight, inventory: &PlateInventory) -> PlateResult {
    let Some(plates_total) = target.as_nanograms().checked_sub(bar.as_nanograms()) else {
        return PlateResult {
            target,
            loadouts: Loadouts::AboveOnly(Loadout::bar_only(bar)),
        };
    };
    // A side may weigh at most this much for the total to stay at or below the target.
    let side_limit = plates_total / 2;
    let empty = Candidate {
        side: 0,
        plates: 0,
        counts: vec![0; inventory.stock.len()],
    };

    // Reachable side weights up to the limit, with the nicest way to reach each.
    let mut reachable = BTreeMap::from([(0, empty)]);
    let mut above: Option<Candidate> = None;
    for (index, stock) in inventory.stock.iter().enumerate() {
        if stock.pairs == 0 {
            continue;
        }
        let plate = stock.plate.as_nanograms();
        let mut next: BTreeMap<u64, Candidate> = BTreeMap::new();
        for base in reachable.values() {
            let mut side = base.side;
            let mut count = 0_u32;
            loop {
                let candidate = Candidate {
                    side,
                    plates: base.plates + u64::from(count),
                    counts: with_count(&base.counts, index, count),
                };
                if side > side_limit {
                    // Too heavy for "at or below". Adding more only makes it heavier, so this is
                    // an "above" candidate and the loop stops here.
                    offer_above(&mut above, candidate);
                    break;
                }
                offer_reachable(&mut next, candidate);
                if count == stock.pairs {
                    break;
                }
                count += 1;
                // side <= side_limit <= Weight::MAX and plate <= Weight::MAX, so no overflow.
                side += plate;
            }
        }
        reachable = next;
    }

    let below = reachable
        .into_values()
        .next_back()
        .and_then(|c| loadout(&c, bar, inventory));
    let above = above.and_then(|c| loadout(&c, bar, inventory));
    let loadouts = match (below, above) {
        (Some(below), _) if below.total == target => Loadouts::Exact(below),
        (Some(below), Some(above)) => Loadouts::Between { below, above },
        (Some(below), None) => Loadouts::BelowOnly(below),
        (None, Some(above)) => Loadouts::AboveOnly(above),
        // Unreachable: the empty loadout (side 0) is always within the limit and its total is the
        // bar. Fall back to the bar alone rather than failing.
        (None, None) => Loadouts::BelowOnly(Loadout::bar_only(bar)),
    };
    PlateResult { target, loadouts }
}

fn with_count(counts: &[u32], index: usize, count: u32) -> Vec<u32> {
    let mut counts = counts.to_vec();
    if let Some(slot) = counts.get_mut(index) {
        *slot = count;
    }
    counts
}

fn offer_reachable(reachable: &mut BTreeMap<u64, Candidate>, candidate: Candidate) {
    match reachable.get_mut(&candidate.side) {
        Some(current) => {
            if candidate.is_nicer_than(current) {
                *current = candidate;
            }
        }
        None => {
            reachable.insert(candidate.side, candidate);
        }
    }
}

fn offer_above(above: &mut Option<Candidate>, candidate: Candidate) {
    let better = above
        .as_ref()
        .is_none_or(|current| match candidate.side.cmp(&current.side) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => candidate.is_nicer_than(current),
        });
    if better {
        *above = Some(candidate);
    }
}

/// Turns pair counts into a loadout. `None` when the total would exceed [`Weight::MAX`].
fn loadout(candidate: &Candidate, bar: Weight, inventory: &PlateInventory) -> Option<Loadout> {
    let per_side = Weight::from_nanograms(candidate.side).ok()?;
    let total = per_side.checked_mul(2).ok()?.checked_add(bar).ok()?;
    let plates = inventory
        .stock
        .iter()
        .zip(&candidate.counts)
        .filter(|&(_, &count)| count > 0)
        .map(|(stock, &count)| PlateCount {
            plate: stock.plate,
            per_side: count,
        })
        .collect();
    Some(Loadout {
        plates,
        per_side,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    fn stock(plate: Weight, pairs: u32) -> PlateStock {
        PlateStock { plate, pairs }
    }

    fn inventory(entries: &[(Weight, u32)]) -> PlateInventory {
        PlateInventory::new(entries.iter().map(|&(plate, pairs)| stock(plate, pairs))).unwrap()
    }

    /// The plates of a loadout as `(plate, per side)` pairs.
    fn plates_of(loadout: &Loadout) -> Vec<(Weight, u32)> {
        loadout
            .plates()
            .iter()
            .map(|c| (c.plate, c.per_side))
            .collect()
    }

    // ---------------------------------------------------------------- inventory

    #[test]
    fn inventory_sorts_heaviest_first() {
        let inv = inventory(&[(kg(5.0), 1), (kg(25.0), 2), (kg(10.0), 3)]);
        let plates: Vec<Weight> = inv.stock().iter().map(|s| s.plate).collect();
        assert_eq!(plates, vec![kg(25.0), kg(10.0), kg(5.0)]);
        assert_eq!(inv.pairs_of(kg(10.0)), 3);
        assert_eq!(inv.pairs_of(kg(20.0)), 0);
        assert!(!inv.is_empty());
    }

    #[test]
    fn inventory_rejects_zero_plates() {
        let err = PlateInventory::new([stock(kg(20.0), 1), stock(Weight::ZERO, 1)]).unwrap_err();
        assert_eq!(err, PlateInventoryError::ZeroPlate);
        assert_eq!(err.to_string(), "a plate must weigh more than zero");
    }

    #[test]
    fn inventory_rejects_duplicate_sizes() {
        let err = PlateInventory::new([stock(kg(20.0), 1), stock(kg(5.0), 1), stock(kg(20.0), 3)])
            .unwrap_err();
        assert_eq!(err, PlateInventoryError::DuplicatePlate(kg(20.0)));
        assert_eq!(err.to_string(), "plate size 20 kg is listed more than once");
        // 10 lb and 4.5359237 kg are the same plate.
        assert_eq!(
            PlateInventory::new([stock(lb(10.0), 1), stock(kg(4.535_923_7), 1)]).unwrap_err(),
            PlateInventoryError::DuplicatePlate(lb(10.0))
        );
    }

    #[test]
    fn inventory_caps_the_number_of_sizes() {
        let sizes = |n: u32| (1..=n).map(|i| stock(kg(f64::from(i)), 1));
        assert!(PlateInventory::new(sizes(16)).is_ok());
        let err = PlateInventory::new(sizes(17)).unwrap_err();
        assert_eq!(err, PlateInventoryError::TooManySizes { max: 16 });
        assert_eq!(err.to_string(), "at most 16 plate sizes are allowed");
    }

    #[test]
    fn inventory_allows_zero_pairs() {
        let inv = inventory(&[(kg(20.0), 0)]);
        assert!(inv.is_empty());
        assert_eq!(inv.stock().len(), 1);
    }

    #[test]
    fn empty_inventory() {
        let inv = PlateInventory::empty();
        assert!(inv.is_empty());
        assert!(inv.stock().is_empty());
        assert_eq!(inv, PlateInventory::default());
    }

    #[test]
    fn kg_default_inventory() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let expected = [
            (25.0, 4),
            (20.0, 2),
            (15.0, 1),
            (10.0, 1),
            (5.0, 1),
            (2.5, 1),
            (1.25, 1),
        ];
        let actual: Vec<(Weight, u32)> = inv.stock().iter().map(|s| (s.plate, s.pairs)).collect();
        let expected: Vec<(Weight, u32)> = expected.iter().map(|&(v, p)| (kg(v), p)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn lb_default_inventory() {
        let inv = PlateInventory::default_for(Unit::Lb);
        let expected = [
            (45.0, 6),
            (35.0, 1),
            (25.0, 1),
            (10.0, 2),
            (5.0, 1),
            (2.5, 1),
        ];
        let actual: Vec<(Weight, u32)> = inv.stock().iter().map(|s| (s.plate, s.pairs)).collect();
        let expected: Vec<(Weight, u32)> = expected.iter().map(|&(v, p)| (lb(v), p)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn default_inventories_reach_every_small_step() {
        // Every 2.5 kg from 20 to 100 kg on a 20 kg bar.
        let inv = PlateInventory::default_for(Unit::Kg);
        for step in 0..=32_u32 {
            let target = kg(20.0 + 2.5 * f64::from(step));
            assert!(
                calculate_plates(target, kg(20.0), &inv).is_exact(),
                "{target:?}"
            );
        }
        // Every 5 lb from 45 to 225 lb on a 45 lb bar.
        let inv = PlateInventory::default_for(Unit::Lb);
        for step in 0..=36_u32 {
            let target = lb(45.0 + 5.0 * f64::from(step));
            assert!(
                calculate_plates(target, lb(45.0), &inv).is_exact(),
                "{target:?}"
            );
        }
    }

    #[test]
    fn inventory_serde_is_a_list_of_plates_in_kg() {
        let inv = inventory(&[(kg(1.25), 2), (kg(20.0), 4)]);
        let json = serde_json::to_string(&inv).unwrap();
        assert_eq!(
            json,
            r#"[{"plate":20.0,"pairs":4},{"plate":1.25,"pairs":2}]"#
        );
        assert_eq!(serde_json::from_str::<PlateInventory>(&json).unwrap(), inv);
        // Order does not matter on input.
        let unordered = r#"[{"plate":1.25,"pairs":2},{"plate":20,"pairs":4}]"#;
        assert_eq!(
            serde_json::from_str::<PlateInventory>(unordered).unwrap(),
            inv
        );
        assert_eq!(
            serde_json::from_str::<PlateInventory>("[]").unwrap(),
            PlateInventory::empty()
        );
    }

    #[test]
    fn inventory_serde_validates() {
        let zero = r#"[{"plate":0,"pairs":1}]"#;
        let err = serde_json::from_str::<PlateInventory>(zero).unwrap_err();
        assert!(err.to_string().contains("more than zero"), "{err}");
        let duplicate = r#"[{"plate":20,"pairs":1},{"plate":20.0,"pairs":2}]"#;
        assert!(serde_json::from_str::<PlateInventory>(duplicate).is_err());
        let negative_pairs = r#"[{"plate":20,"pairs":-1}]"#;
        assert!(serde_json::from_str::<PlateInventory>(negative_pairs).is_err());
        let negative_plate = r#"[{"plate":-20,"pairs":1}]"#;
        assert!(serde_json::from_str::<PlateInventory>(negative_plate).is_err());
    }

    #[test]
    fn lb_inventory_serde_round_trips_exactly() {
        let inv = PlateInventory::default_for(Unit::Lb);
        let json = serde_json::to_string(&inv).unwrap();
        assert_eq!(serde_json::from_str::<PlateInventory>(&json).unwrap(), inv);
    }

    // ---------------------------------------------------------------- calculator

    #[test]
    fn exact_kg_target() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(102.5), kg(20.0), &inv);
        assert!(result.is_exact());
        assert_eq!(result.target(), kg(102.5));
        assert_eq!(result.outcome(), PlateOutcome::Exact);
        let exact = result.exact().unwrap();
        assert_eq!(result.below(), Some(exact));
        assert_eq!(result.above(), None);
        assert_eq!(result.closest(), exact);
        assert_eq!(exact.total(), kg(102.5));
        assert_eq!(exact.per_side(), kg(41.25));
        // Fewest plates: 25 + 15 + 1.25.
        assert_eq!(
            plates_of(exact),
            vec![(kg(25.0), 1), (kg(15.0), 1), (kg(1.25), 1)]
        );
        assert_eq!(exact.plate_count_per_side(), 3);
        assert_eq!(
            exact.plates_one_by_one().collect::<Vec<_>>(),
            vec![kg(25.0), kg(15.0), kg(1.25)]
        );
    }

    #[test]
    fn prefers_fewest_then_heaviest_plates() {
        let inv = inventory(&[(kg(25.0), 2), (kg(15.0), 2), (kg(5.0), 6)]);
        // 30 per side: 25 + 5 and 15 + 15 both use two plates; the heavier first plate wins.
        let result = calculate_plates(kg(80.0), kg(20.0), &inv);
        assert_eq!(
            plates_of(result.exact().unwrap()),
            vec![(kg(25.0), 1), (kg(5.0), 1)]
        );
        // 20 per side: 15 + 5 (two plates), not four 5s.
        let result = calculate_plates(kg(60.0), kg(20.0), &inv);
        assert_eq!(
            plates_of(result.exact().unwrap()),
            vec![(kg(15.0), 1), (kg(5.0), 1)]
        );
    }

    #[test]
    fn multiple_plates_of_a_size_are_listed_once() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(220.0), kg(20.0), &inv);
        let exact = result.exact().unwrap();
        assert_eq!(plates_of(exact), vec![(kg(25.0), 4)]);
        assert_eq!(exact.plates_one_by_one().count(), 4);
    }

    #[test]
    fn impossible_target_returns_both_neighbours() {
        let inv = PlateInventory::default_for(Unit::Kg);
        // 101 kg on a 20 kg bar: 100 and 102.5 are the neighbours.
        let result = calculate_plates(kg(101.0), kg(20.0), &inv);
        assert!(!result.is_exact());
        assert_eq!(result.exact(), None);
        let below = result.below().unwrap();
        let above = result.above().unwrap();
        assert_eq!(below.total(), kg(100.0));
        assert_eq!(plates_of(below), vec![(kg(25.0), 1), (kg(15.0), 1)]);
        assert_eq!(above.total(), kg(102.5));
        // 1 kg under beats 1.5 kg over.
        assert_eq!(result.closest(), below);
        assert_eq!(result.outcome(), PlateOutcome::Under(kg(1.0)));
    }

    #[test]
    fn closest_can_be_above() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(102.0), kg(20.0), &inv);
        assert_eq!(result.closest().total(), kg(102.5));
        assert_eq!(result.outcome(), PlateOutcome::Over(kg(0.5)));
    }

    #[test]
    fn closest_tie_prefers_the_lighter_loadout() {
        let inv = PlateInventory::default_for(Unit::Kg);
        // 101.25 is halfway between 100 and 102.5.
        let result = calculate_plates(kg(101.25), kg(20.0), &inv);
        assert_eq!(result.closest().total(), kg(100.0));
        assert_eq!(result.outcome(), PlateOutcome::Under(kg(1.25)));
    }

    #[test]
    fn odd_nanogram_difference_is_never_exact() {
        // Plates go on both sides, so the plate total must be even.
        let inv = inventory(&[(Weight::from_nanograms(1).unwrap(), 10)]);
        let result = calculate_plates(Weight::from_nanograms(5).unwrap(), Weight::ZERO, &inv);
        assert_eq!(result.below().unwrap().total().as_nanograms(), 4);
        assert_eq!(result.above().unwrap().total().as_nanograms(), 6);
    }

    #[test]
    fn limited_inventory_where_greedy_fails() {
        // 40 kg per side from one 25 and two 20s. Greedy takes the 25 and is stuck at 25 per side
        // (70 kg); the exact answer is two 20s.
        let inv = inventory(&[(kg(25.0), 1), (kg(20.0), 2)]);
        let result = calculate_plates(kg(100.0), kg(20.0), &inv);
        let exact = result.exact().unwrap();
        assert_eq!(plates_of(exact), vec![(kg(20.0), 2)]);
        assert_eq!(exact.total(), kg(100.0));

        // Same trap for the closest below: 30 per side from 25 × 1, 15 × 2, target 82.
        let inv = inventory(&[(kg(25.0), 1), (kg(15.0), 2)]);
        let result = calculate_plates(kg(82.0), kg(20.0), &inv);
        assert_eq!(result.below().unwrap().total(), kg(80.0));
        assert_eq!(plates_of(result.below().unwrap()), vec![(kg(15.0), 2)]);
        assert_eq!(result.above().unwrap().total(), kg(100.0));
    }

    #[test]
    fn respects_pair_counts() {
        // Only one pair of 20s: 60 kg per side is out of reach.
        let inv = inventory(&[(kg(20.0), 1), (kg(10.0), 1)]);
        let result = calculate_plates(kg(140.0), kg(20.0), &inv);
        assert!(!result.is_exact());
        let below = result.below().unwrap();
        assert_eq!(below.total(), kg(80.0));
        assert_eq!(plates_of(below), vec![(kg(20.0), 1), (kg(10.0), 1)]);
        // Nothing heavier exists.
        assert_eq!(result.above(), None);
        assert_eq!(result.closest(), below);
        assert_eq!(result.outcome(), PlateOutcome::Under(kg(60.0)));
    }

    #[test]
    fn zero_pair_sizes_are_not_used() {
        let inv = inventory(&[(kg(20.0), 0), (kg(10.0), 2)]);
        let result = calculate_plates(kg(60.0), kg(20.0), &inv);
        assert_eq!(plates_of(result.exact().unwrap()), vec![(kg(10.0), 2)]);
    }

    #[test]
    fn lb_plates() {
        let inv = PlateInventory::default_for(Unit::Lb);
        let result = calculate_plates(lb(225.0), lb(45.0), &inv);
        let exact = result.exact().unwrap();
        assert_eq!(plates_of(exact), vec![(lb(45.0), 2)]);
        assert_eq!(exact.total(), lb(225.0));
        assert_eq!(exact.total().format_value(Unit::Lb, 2), "225");

        let result = calculate_plates(lb(185.0), lb(45.0), &inv);
        assert_eq!(
            plates_of(result.exact().unwrap()),
            vec![(lb(45.0), 1), (lb(25.0), 1)]
        );

        // 137 lb: 135 below, 140 above.
        let result = calculate_plates(lb(137.0), lb(45.0), &inv);
        assert_eq!(result.below().unwrap().total(), lb(135.0));
        assert_eq!(result.above().unwrap().total(), lb(140.0));
        assert_eq!(result.outcome(), PlateOutcome::Under(lb(2.0)));
    }

    #[test]
    fn mixed_kg_and_lb_plates() {
        // A kg gym with a pair of 2.5 lb change plates.
        let inv = inventory(&[(kg(20.0), 2), (lb(2.5), 1)]);
        let target = kg(60.0).checked_add(lb(5.0)).unwrap();
        let result = calculate_plates(target, kg(20.0), &inv);
        assert_eq!(
            plates_of(result.exact().unwrap()),
            vec![(kg(20.0), 1), (lb(2.5), 1)]
        );
    }

    #[test]
    fn target_below_the_bar() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(15.0), kg(20.0), &inv);
        assert!(!result.is_exact());
        assert_eq!(result.below(), None);
        let above = result.above().unwrap();
        assert_eq!(above.total(), kg(20.0));
        assert!(above.plates().is_empty());
        assert_eq!(above.per_side(), Weight::ZERO);
        assert_eq!(result.closest(), above);
        assert_eq!(result.outcome(), PlateOutcome::Over(kg(5.0)));
    }

    #[test]
    fn target_equal_to_the_bar() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(20.0), kg(20.0), &inv);
        let exact = result.exact().unwrap();
        assert!(exact.plates().is_empty());
        assert_eq!(exact.plate_count_per_side(), 0);
        assert_eq!(exact.total(), kg(20.0));
    }

    #[test]
    fn target_just_above_the_bar() {
        let inv = PlateInventory::default_for(Unit::Kg);
        let result = calculate_plates(kg(21.0), kg(20.0), &inv);
        assert_eq!(result.below().unwrap().total(), kg(20.0));
        assert_eq!(result.above().unwrap().total(), kg(22.5));
    }

    #[test]
    fn empty_inventory_only_loads_the_bar() {
        let inv = PlateInventory::empty();
        let result = calculate_plates(kg(20.0), kg(20.0), &inv);
        assert!(result.is_exact());

        let result = calculate_plates(kg(60.0), kg(20.0), &inv);
        assert_eq!(result.below().unwrap().total(), kg(20.0));
        assert_eq!(result.above(), None);
        assert_eq!(result.outcome(), PlateOutcome::Under(kg(40.0)));

        let result = calculate_plates(kg(10.0), kg(20.0), &inv);
        assert_eq!(result.below(), None);
        assert_eq!(result.above().unwrap().total(), kg(20.0));
    }

    #[test]
    fn no_bar() {
        // A zero bar (e.g. a loading pin measured separately) works like any other bar.
        let inv = inventory(&[(kg(10.0), 2)]);
        let result = calculate_plates(kg(40.0), Weight::ZERO, &inv);
        assert_eq!(plates_of(result.exact().unwrap()), vec![(kg(10.0), 2)]);
        let result = calculate_plates(Weight::ZERO, Weight::ZERO, &inv);
        assert!(result.exact().unwrap().plates().is_empty());
    }

    #[test]
    fn loadouts_never_exceed_the_weight_cap() {
        // Near Weight::MAX, the heavier neighbour would pass 2000 kg, so it is not offered.
        let inv = inventory(&[(kg(600.0), 2)]);
        let result = calculate_plates(Weight::MAX, kg(20.0), &inv);
        assert_eq!(result.below().unwrap().total(), kg(1220.0));
        assert_eq!(result.above(), None);
        // A plate heavier than half the cap can never be loaded.
        let inv = inventory(&[(Weight::MAX, 1)]);
        let result = calculate_plates(kg(1500.0), Weight::ZERO, &inv);
        assert_eq!(result.below().unwrap().total(), Weight::ZERO);
        assert_eq!(result.above(), None);
    }

    #[test]
    fn huge_pair_counts_stay_cheap() {
        let inv = inventory(&[(kg(1.25), u32::MAX)]);
        let result = calculate_plates(Weight::MAX, kg(20.0), &inv);
        let exact = result.exact().unwrap();
        assert_eq!(
            exact.plates(),
            &[PlateCount {
                plate: kg(1.25),
                per_side: 792
            }]
        );
    }

    // ---------------------------------------------------------------- brute force comparison

    /// A reference loadout: its total in nanograms and its plates per side.
    type Reference = Option<(u64, Vec<(Weight, u32)>)>;

    /// Checks every combination of pair counts and returns the reference `(below, above)`, with
    /// the same tie-break as the calculator (fewest plates, then most heavy plates).
    fn brute_force(target: Weight, bar: Weight, inv: &PlateInventory) -> (Reference, Reference) {
        let stock = inv.stock();
        let mut counts = vec![0_u32; stock.len()];
        // (total, plates, counts) of the best loadout found so far on each side of the target.
        let mut below: Option<(u64, u64, Vec<u32>)> = None;
        let mut above: Option<(u64, u64, Vec<u32>)> = None;
        loop {
            let side: u64 = stock
                .iter()
                .zip(&counts)
                .map(|(s, &c)| s.plate.as_nanograms() * u64::from(c))
                .sum();
            let plates: u64 = counts.iter().map(|&c| u64::from(c)).sum();
            let total = bar.as_nanograms() + 2 * side;
            if total <= Weight::MAX.as_nanograms() {
                let key = (total, plates, counts.clone());
                if total <= target.as_nanograms() {
                    let better = below.as_ref().is_none_or(|b| {
                        total > b.0
                            || (total == b.0 && (plates < b.1 || (plates == b.1 && key.2 > b.2)))
                    });
                    if better {
                        below = Some(key);
                    }
                } else {
                    let better = above.as_ref().is_none_or(|a| {
                        total < a.0
                            || (total == a.0 && (plates < a.1 || (plates == a.1 && key.2 > a.2)))
                    });
                    if better {
                        above = Some(key);
                    }
                }
            }
            // Next combination (odometer).
            let mut i = 0;
            loop {
                if i == counts.len() {
                    let reference = |k: (u64, u64, Vec<u32>)| {
                        let plates = stock
                            .iter()
                            .zip(&k.2)
                            .filter(|&(_, &c)| c > 0)
                            .map(|(s, &c)| (s.plate, c))
                            .collect::<Vec<_>>();
                        (k.0, plates)
                    };
                    return (below.map(reference), above.map(reference));
                }
                if counts[i] < stock[i].pairs {
                    counts[i] += 1;
                    break;
                }
                counts[i] = 0;
                i += 1;
            }
        }
    }

    /// Plate sizes to draw from: kg and lb, including awkward ones that defeat greedy.
    fn plate_pool() -> Vec<Weight> {
        vec![
            kg(25.0),
            kg(20.0),
            kg(15.0),
            kg(10.0),
            kg(7.5),
            kg(5.0),
            kg(2.5),
            kg(1.25),
            kg(0.5),
            lb(45.0),
            lb(35.0),
            lb(25.0),
            lb(10.0),
            lb(5.0),
            lb(2.5),
        ]
    }

    fn any_inventory() -> impl Strategy<Value = PlateInventory> {
        proptest::sample::subsequence(plate_pool(), 0..=5)
            .prop_flat_map(|plates| {
                let n = plates.len();
                (Just(plates), proptest::collection::vec(0_u32..=3, n))
            })
            .prop_map(|(plates, pairs)| {
                PlateInventory::new(plates.into_iter().zip(pairs).map(|(p, c)| stock(p, c)))
                    .unwrap()
            })
    }

    fn any_bar() -> impl Strategy<Value = Weight> {
        prop_oneof![
            Just(Weight::ZERO),
            Just(kg(15.0)),
            Just(kg(20.0)),
            Just(lb(45.0)),
            (0_u64..=30_000_000_000_000).prop_map(|n| Weight::from_nanograms(n).unwrap()),
        ]
    }

    fn check_against_brute_force(
        target: Weight,
        bar: Weight,
        inv: &PlateInventory,
    ) -> Result<(), TestCaseError> {
        let result = calculate_plates(target, bar, inv);
        let (below, above) = brute_force(target, bar, inv);
        let as_reference = |l: &Loadout| (l.total().as_nanograms(), plates_of(l));
        let exact = below.as_ref().map(|b| b.0) == Some(target.as_nanograms());
        prop_assert_eq!(result.is_exact(), exact);
        prop_assert_eq!(result.below().map(as_reference), below);
        if result.is_exact() {
            prop_assert_eq!(result.above(), None);
        } else {
            prop_assert_eq!(result.above().map(as_reference), above);
        }
        // Internal consistency of every returned loadout.
        for loadout in result.below().into_iter().chain(result.above()) {
            let side: u64 = loadout
                .plates()
                .iter()
                .map(|c| c.plate.as_nanograms() * u64::from(c.per_side))
                .sum();
            prop_assert_eq!(loadout.per_side().as_nanograms(), side);
            prop_assert_eq!(
                loadout.total().as_nanograms(),
                bar.as_nanograms() + 2 * side
            );
            prop_assert!(loadout.plates().windows(2).all(|w| w[0].plate > w[1].plate));
            for count in loadout.plates() {
                prop_assert!(count.per_side > 0);
                prop_assert!(count.per_side <= inv.pairs_of(count.plate));
            }
        }
        // The closest really is the closest, lighter on a tie.
        let closest = result.closest().total();
        for other in result.below().into_iter().chain(result.above()) {
            let (d_closest, d_other) = (closest.abs_diff(target), other.total().abs_diff(target));
            prop_assert!(d_closest < d_other || (d_closest == d_other && closest <= other.total()));
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn matches_brute_force_on_random_targets(
            inv in any_inventory(),
            bar in any_bar(),
            target in 0_u64..=300_000_000_000_000,
        ) {
            let target = Weight::from_nanograms(target).unwrap();
            check_against_brute_force(target, bar, &inv)?;
        }

        #[test]
        fn matches_brute_force_on_round_targets(
            inv in any_inventory(),
            bar in any_bar(),
            steps in 0_u32..=240,
            in_lb in any::<bool>(),
        ) {
            // Multiples of 1.25 kg or 2.5 lb, where exact hits are common.
            let target = if in_lb { lb(2.5 * f64::from(steps)) } else { kg(1.25 * f64::from(steps)) };
            check_against_brute_force(target, bar, &inv)?;
        }

        #[test]
        fn inventory_serde_round_trips(inv in any_inventory()) {
            let json = serde_json::to_string(&inv).unwrap();
            prop_assert_eq!(serde_json::from_str::<PlateInventory>(&json).unwrap(), inv);
        }
    }
}
