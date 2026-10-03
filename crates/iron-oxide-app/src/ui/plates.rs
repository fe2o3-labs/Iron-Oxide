//! The plate calculator (#31): which plates go on each side of the bar for a weight.
//!
//! The maths is the domain's [`calculate_plates`] (exact weights, bounded search). The bar weight
//! and the plates come from the user's settings ([`get_settings`] answers with the defaults for a
//! user who never saved any); until they load, or if they cannot be loaded, the defaults for the
//! user's unit are used.
//!
//! Three ways in:
//! - [`PlateCalculator`]: the calculator for a weight, inline (loads the settings itself).
//! - [`PlateCalculatorSheet`]: the same in a bottom sheet, opened from the session screen (#28).
//! - [`PlateTool`]: the `/tools/plates` page, with a weight stepper.

use dioxus::prelude::*;
use iron_oxide_domain::{PlateInventory, PlateOutcome, Unit, Weight, calculate_plates};

use super::components::{Chip, Sheet, WeightStepper};
use super::errors::use_errors;
use super::weight::{use_unit, weight_number, weight_text};
use crate::api::settings::{Settings, get_settings};

/// The bar and the plates the calculator loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlateSetup {
    pub bar: Weight,
    pub inventory: PlateInventory,
}

impl PlateSetup {
    /// The user's bar and plates.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            bar: settings.bar_weight,
            inventory: settings.plate_inventory.clone(),
        }
    }

    /// A standard bar (20 kg or 45 lb) and the domain's default plates for `unit`.
    #[must_use]
    pub fn defaults_for(unit: Unit) -> Self {
        let bar = match unit {
            Unit::Kg => Weight::from_kg(20.0),
            Unit::Lb => Weight::from_lb(45.0),
        };
        Self {
            bar: bar.unwrap_or(Weight::ZERO),
            inventory: PlateInventory::default_for(unit),
        }
    }
}

/// What the calculator shows for a target weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlateView {
    /// The weight asked for.
    pub target: Weight,
    /// The weight loaded: the target when it is exact, else the nearest loadable weight.
    pub total: Weight,
    pub bar: Weight,
    /// The plates on each side, heaviest (innermost) first, one entry per plate.
    pub per_side: Vec<Weight>,
    /// Exact, or how far under or over the target `total` is.
    pub outcome: PlateOutcome,
    /// When the target is not exact: the loadable weight on the other side of it, if any.
    pub other: Option<Weight>,
}

impl PlateView {
    /// Whether the target can be loaded exactly.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self.outcome, PlateOutcome::Exact)
    }
}

/// The plates for `target` with `setup`: the exact loadout, or the nearest one (the domain picks
/// the lighter one on a tie).
#[must_use]
pub fn plate_view(target: Weight, setup: &PlateSetup) -> PlateView {
    let result = calculate_plates(target, setup.bar, &setup.inventory);
    let closest = result.closest();
    let other = if result.is_exact() {
        None
    } else {
        [result.below(), result.above()]
            .into_iter()
            .flatten()
            .map(iron_oxide_domain::Loadout::total)
            .find(|&total| total != closest.total())
    };
    PlateView {
        target,
        total: closest.total(),
        bar: setup.bar,
        per_side: closest.plates_one_by_one().collect(),
        outcome: result.outcome(),
        other,
    }
}

/// The line under the total when it is not the target: `"Target 101 kg · 1 kg under · or
/// 102.5 kg"`. `None` when the target is exact.
#[must_use]
pub fn miss_text(view: &PlateView, unit: Unit) -> Option<String> {
    let (gap, side) = match view.outcome {
        PlateOutcome::Exact => return None,
        PlateOutcome::Under(gap) => (gap, "under"),
        PlateOutcome::Over(gap) => (gap, "over"),
    };
    let mut text = format!(
        "Target {} · {} {side}",
        weight_text(view.target, unit),
        weight_text(gap, unit)
    );
    if let Some(other) = view.other {
        text.push_str(&format!(" · or {}", weight_text(other, unit)));
    }
    Some(text)
}

/// The height of a plate in the stack drawing, in px: from 32 px for the smallest plates to 96 px
/// for 25 kg and up, so heavier plates read as bigger.
#[must_use]
pub fn plate_height(plate: Weight) -> u32 {
    const MIN: u64 = 32;
    const MAX: u64 = 96;
    let full = Weight::from_kg(25.0).map_or(1, Weight::as_nanograms);
    let share = plate.as_nanograms().min(full);
    let height = MIN + (MAX - MIN) * share / full;
    u32::try_from(height).unwrap_or(96)
}

/// The step of the `/tools/plates` stepper: the smallest common jump in `unit` (2.5 kg, 5 lb).
#[must_use]
pub fn tool_step(unit: Unit) -> Weight {
    match unit {
        Unit::Kg => Weight::from_kg(2.5),
        Unit::Lb => Weight::from_lb(5.0),
    }
    .unwrap_or(Weight::ZERO)
}

/// The weight `/tools/plates` opens with: 100 kg or 225 lb.
#[must_use]
pub fn tool_start(unit: Unit) -> Weight {
    match unit {
        Unit::Kg => Weight::from_kg(100.0),
        Unit::Lb => Weight::from_lb(225.0),
    }
    .unwrap_or(Weight::ZERO)
}

/// The user's bar and plates: the defaults for their unit until the settings load, and if they
/// cannot be loaded (the error is reported).
pub fn use_plate_setup() -> PlateSetup {
    let unit = use_unit();
    let errors = use_errors();
    let settings = use_resource(move || async move {
        match get_settings().await {
            Ok(settings) => Some(PlateSetup::from_settings(&settings)),
            Err(error) => {
                errors.report(&error);
                None
            }
        }
    });
    let loaded = settings.read().clone().flatten();
    loaded.unwrap_or_else(|| PlateSetup::defaults_for(unit))
}

/// The plate calculator for `weight`, with the user's bar and plates.
///
/// ```ignore
/// PlateCalculator { weight: target }
/// ```
#[allow(dead_code, reason = "opened by the session screen (#28)")]
#[component]
pub fn PlateCalculator(weight: Weight) -> Element {
    let setup = use_plate_setup();
    rsx! {
        PlateLoadout { view: plate_view(weight, &setup) }
    }
}

/// The plate calculator for `weight` in a bottom sheet over the page. Escape, the close button and
/// a tap outside the sheet call `on_close`.
///
/// ```ignore
/// let mut plates_open = use_signal(|| false);
/// // header: IconButton { label: "Plate calculator", onclick: move |_| plates_open.set(true), PlateIcon {} }
/// if plates_open() {
///     PlateCalculatorSheet { weight: target, on_close: move |()| plates_open.set(false) }
/// }
/// ```
#[allow(dead_code, reason = "opened by the session screen (#28)")]
#[component]
pub fn PlateCalculatorSheet(weight: Weight, on_close: EventHandler<()>) -> Element {
    rsx! {
        Sheet { title: "Plate calculator", on_close,
            PlateCalculator { weight }
        }
    }
}

/// The `/tools/plates` page: pick a weight with the stepper, see the plates.
#[component]
pub fn PlateTool() -> Element {
    let unit = use_unit();
    let setup = use_plate_setup();
    let mut weight = use_signal(move || tool_start(unit));
    let value = *weight.read();
    rsx! {
        div { class: "io-page-header",
            span { class: "io-label", "Tools" }
            h1 { class: "io-title", "Plates" }
        }
        WeightStepper {
            value,
            step: tool_step(unit),
            min: setup.bar,
            on_change: move |next| weight.set(next),
        }
        PlateLoadout { view: plate_view(value, &setup) }
    }
}

/// The loadout of a [`PlateView`]: the total, how far it is from the target, the per-side plates
/// as chips and a drawing of one sleeve.
#[component]
pub fn PlateLoadout(view: PlateView) -> Element {
    let unit = use_unit();
    let total = weight_number(view.total, unit);
    let status = if view.is_exact() { "Exact" } else { "Nearest" };
    let bar = weight_text(view.bar, unit);
    let miss = miss_text(&view, unit);
    rsx! {
        section { class: "io-card io-plates", aria_label: "Plates",
            div { class: "io-plates-total",
                span { class: "io-label", "{status}" }
                div { class: "io-plates-number",
                    output { aria_live: "polite", "{total}" }
                    span { class: "io-stepper-label", "{unit.symbol()}" }
                }
                if let Some(miss) = miss {
                    p { class: "io-muted io-hint", "{miss}" }
                }
            }
            div { class: "io-plate-stack", aria_hidden: "true",
                span { class: "io-plate-sleeve" }
                for (index, plate) in view.per_side.iter().enumerate() {
                    span {
                        key: "{index}",
                        class: "io-plate",
                        style: "height: {plate_height(*plate)}px",
                    }
                }
                span { class: "io-plate-collar" }
            }
            div { class: "io-chips",
                span { class: "io-muted io-hint", "Per side" }
                if view.per_side.is_empty() {
                    span { class: "io-hint", "Bar only" }
                }
                for (index, plate) in view.per_side.iter().enumerate() {
                    Chip { key: "{index}", "{weight_number(*plate, unit)}" }
                }
            }
            p { class: "io-muted io-hint", "Bar {bar}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use iron_oxide_domain::PlateStock;

    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn lb(value: f64) -> Weight {
        Weight::from_lb(value).unwrap()
    }

    fn kg_gym() -> PlateSetup {
        PlateSetup::defaults_for(Unit::Kg)
    }

    #[test]
    fn an_exact_weight_lists_the_plates_heaviest_first() {
        let view = plate_view(kg(100.0), &kg_gym());
        assert!(view.is_exact());
        assert_eq!(view.total, kg(100.0));
        assert_eq!(view.per_side, [kg(25.0), kg(15.0)]);
        assert_eq!(view.other, None);
        assert_eq!(miss_text(&view, Unit::Kg), None);

        let view = plate_view(kg(142.5), &kg_gym());
        assert!(view.is_exact());
        assert!(view.per_side.is_sorted_by(|a, b| a >= b));
        assert_eq!(view.per_side.len(), 4);
    }

    #[test]
    fn an_impossible_weight_gives_the_nearest_and_the_other_side() {
        // 101 kg: 100 kg is 1 under, 102.5 kg is 1.5 over.
        let view = plate_view(kg(101.0), &kg_gym());
        assert!(!view.is_exact());
        assert_eq!(view.total, kg(100.0));
        assert_eq!(view.outcome, PlateOutcome::Under(kg(1.0)));
        assert_eq!(view.other, Some(kg(102.5)));
        assert_eq!(
            miss_text(&view, Unit::Kg).as_deref(),
            Some("Target 101 kg · 1 kg under · or 102.5 kg")
        );

        // 102 kg: 102.5 kg is nearer.
        let view = plate_view(kg(102.0), &kg_gym());
        assert_eq!(view.total, kg(102.5));
        assert_eq!(view.outcome, PlateOutcome::Over(kg(0.5)));
        assert_eq!(view.other, Some(kg(100.0)));
        assert_eq!(
            miss_text(&view, Unit::Kg).as_deref(),
            Some("Target 102 kg · 0.5 kg over · or 100 kg")
        );
    }

    #[test]
    fn a_weight_below_the_bar_is_the_bar_alone() {
        let view = plate_view(kg(15.0), &kg_gym());
        assert_eq!(view.total, kg(20.0));
        assert!(view.per_side.is_empty());
        assert_eq!(view.outcome, PlateOutcome::Over(kg(5.0)));
        assert_eq!(view.other, None);
        assert_eq!(
            miss_text(&view, Unit::Kg).as_deref(),
            Some("Target 15 kg · 5 kg over")
        );
    }

    #[test]
    fn without_plates_only_the_bar_is_loaded() {
        let setup = PlateSetup {
            bar: kg(20.0),
            inventory: PlateInventory::empty(),
        };
        let view = plate_view(kg(60.0), &setup);
        assert_eq!(view.total, kg(20.0));
        assert!(view.per_side.is_empty());
        assert_eq!(view.outcome, PlateOutcome::Under(kg(40.0)));
    }

    #[test]
    fn the_inventory_limits_the_plates() {
        // One pair of 20s only: 100 kg is out of reach, 60 kg is the most.
        let setup = PlateSetup {
            bar: kg(20.0),
            inventory: PlateInventory::new([PlateStock {
                plate: kg(20.0),
                pairs: 1,
            }])
            .unwrap(),
        };
        let view = plate_view(kg(100.0), &setup);
        assert_eq!(view.total, kg(60.0));
        assert_eq!(view.per_side, [kg(20.0)]);
        assert_eq!(view.outcome, PlateOutcome::Under(kg(40.0)));
    }

    #[test]
    fn pounds_are_exact_on_a_45_lb_bar() {
        let setup = PlateSetup::defaults_for(Unit::Lb);
        assert_eq!(setup.bar, lb(45.0));
        let view = plate_view(lb(225.0), &setup);
        assert!(view.is_exact());
        assert_eq!(view.per_side, [lb(45.0), lb(45.0)]);
        assert_eq!(weight_number(view.per_side[0], Unit::Lb), "45");

        // 227.5 lb needs 1.25 lb plates: 225 and 230 lb are as near, the lighter one wins.
        let view = plate_view(lb(227.5), &setup);
        assert!(!view.is_exact());
        assert_eq!(view.total, lb(225.0));
        assert_eq!(view.other, Some(lb(230.0)));
        assert_eq!(
            miss_text(&view, Unit::Lb).as_deref(),
            Some("Target 227.5 lb · 2.5 lb under · or 230 lb")
        );
    }

    #[test]
    fn the_settings_give_the_bar_and_the_plates() {
        let mut settings = Settings::defaults();
        settings.bar_weight = kg(15.0);
        let setup = PlateSetup::from_settings(&settings);
        assert_eq!(setup.bar, kg(15.0));
        assert_eq!(setup.inventory, settings.plate_inventory);
        assert_eq!(PlateSetup::defaults_for(Unit::Kg).bar, kg(20.0));
    }

    #[test]
    fn heavier_plates_are_drawn_taller() {
        assert_eq!(plate_height(kg(25.0)), 96);
        assert_eq!(plate_height(kg(50.0)), 96);
        assert_eq!(plate_height(Weight::ZERO), 32);
        assert!(plate_height(kg(20.0)) > plate_height(kg(10.0)));
        assert!(plate_height(kg(1.25)) >= 32);
    }

    #[test]
    fn the_tool_steps_in_common_jumps() {
        assert_eq!(tool_step(Unit::Kg), kg(2.5));
        assert_eq!(tool_step(Unit::Lb), lb(5.0));
        assert_eq!(tool_start(Unit::Kg), kg(100.0));
        assert_eq!(tool_start(Unit::Lb), lb(225.0));
    }
}
