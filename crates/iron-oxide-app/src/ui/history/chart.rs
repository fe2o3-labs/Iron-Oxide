//! The geometry of the history charts (#33): scales, axis ticks and the SVG path of a line.
//!
//! Pure and free of Dioxus, so it is unit-tested without a browser; `progress.rs` turns a
//! [`ChartLayout`] into inline SVG. No chart library: a chart is one line over time, with a few
//! horizontal grid lines labelled in the user's unit.
//!
//! Weights stay exact [`Weight`]s up to here. Floats only appear for what is drawn: the value of a
//! weight in the user's unit (so that the ticks fall on round numbers of *that* unit), and the
//! pixel coordinates.

use iron_oxide_domain::{Unit, Weight};

/// The size of the chart's `viewBox`. 320 wide is the inner width of a card on a 390 px phone, so
/// text in the SVG shows at about its nominal size there.
pub const VIEW_WIDTH: f64 = 320.0;
pub const VIEW_HEIGHT: f64 = 176.0;

/// Room around the plot: tick labels on the left, dates below.
const PAD_LEFT: f64 = 44.0;
const PAD_RIGHT: f64 = 12.0;
const PAD_TOP: f64 = 12.0;
const PAD_BOTTOM: f64 = 28.0;

/// At most this many horizontal grid lines: readable at 390 px.
const MAX_TICKS: usize = 5;

/// One point of a chart: when (milliseconds since the epoch) and how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightPoint {
    pub at_ms: i64,
    pub weight: Weight,
}

/// A linear map from a value range (the domain) to a pixel range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    domain: (f64, f64),
    range: (f64, f64),
}

impl Scale {
    #[must_use]
    pub const fn new(domain: (f64, f64), range: (f64, f64)) -> Self {
        Self { domain, range }
    }

    /// The pixel of `value`. An empty domain (a single value) maps to the middle of the range.
    #[must_use]
    pub fn map(&self, value: f64) -> f64 {
        let (d0, d1) = self.domain;
        let (r0, r1) = self.range;
        let span = d1 - d0;
        if span.abs() < f64::EPSILON {
            return (r0 + r1) / 2.0;
        }
        r0 + (value - d0) / span * (r1 - r0)
    }
}

/// A "nice" step for about `target` intervals over `span`: 1, 2, 2.5 or 5 times a power of ten
/// (2.5 so that 2.5 kg and 25 lb steps, common in a gym, are possible).
#[must_use]
pub fn nice_step(span: f64, target: usize) -> f64 {
    let intervals = f64::from(u32::try_from(target.max(1)).unwrap_or(u32::MAX));
    let raw = span.abs() / intervals;
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10_f64.powf(raw.log10().floor());
    let normalised = raw / magnitude;
    let factor = [1.0, 2.0, 2.5, 5.0]
        .into_iter()
        .find(|&factor| normalised <= factor)
        .unwrap_or(10.0);
    factor * magnitude
}

/// The y axis: round tick values covering `min..=max` (values in the user's unit), at most
/// [`MAX_TICKS`] of them. Equal values (one point, or a flat line) get a range around them, so the
/// line sits mid-chart instead of on an edge.
#[must_use]
pub fn y_ticks(min: f64, max: f64) -> Vec<f64> {
    let (mut low, mut high) = if min <= max { (min, max) } else { (max, min) };
    if (high - low).abs() < f64::EPSILON {
        // ±5 % of the value (at least ±1 unit), so 100 kg reads 95 to 105.
        let pad = (low.abs() * 0.05).max(1.0);
        low -= pad;
        high += pad;
    }
    let mut step = nice_step(high - low, MAX_TICKS - 1);
    loop {
        let first = (low / step).floor() * step;
        let last = (high / step).ceil() * step;
        let count = ((last - first) / step).round();
        if count <= (MAX_TICKS - 1) as f64 {
            // `count` is a small whole number (at most MAX_TICKS - 1).
            let count = count as usize;
            return (0..=count)
                .map(|index| clean(first + step * index as f64))
                .collect();
        }
        // Rounding the ends out added an interval too many: take the next nice step.
        step = nice_step(step * MAX_TICKS as f64, MAX_TICKS - 2);
    }
}

/// Removes float noise such as `102.50000000001` (ticks are multiples of at least 0.001).
fn clean(value: f64) -> f64 {
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// A tick value as text: `100`, `102.5`, `0.25`.
#[must_use]
pub fn tick_label(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// A labelled tick: where it is drawn (pixels) and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub at: f64,
    pub label: String,
}

/// A point as drawn, in `viewBox` pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    pub x: f64,
    pub y: f64,
}

/// Everything the SVG needs to draw one line chart.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartLayout {
    /// The `d` attribute of the line (`M x y L x y …`).
    pub path: String,
    pub dots: Vec<Dot>,
    /// Horizontal grid lines, bottom to top, labelled in the user's unit.
    pub y_ticks: Vec<Tick>,
    /// The dates under the x axis: the first and, if different, the last session.
    pub x_ticks: Vec<Tick>,
    /// The plot area: left, top, right, bottom.
    pub plot: (f64, f64, f64, f64),
}

/// Lays out `points` (oldest first) in `unit`. `date_label` writes a time (ms) as a short date.
/// `None` for no points: the screen shows its empty state instead of an empty chart.
#[must_use]
pub fn layout(
    points: &[WeightPoint],
    unit: Unit,
    date_label: impl Fn(i64) -> String,
) -> Option<ChartLayout> {
    let first = points.first()?;
    let last = points.last()?;
    let values: Vec<f64> = points
        .iter()
        .map(|point| point.weight.value_in(unit))
        .collect();
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let ticks = y_ticks(min, max);
    let (low, high) = (
        ticks.first().copied().unwrap_or(min),
        ticks.last().copied().unwrap_or(max),
    );

    let (left, top) = (PAD_LEFT, PAD_TOP);
    let (right, bottom) = (VIEW_WIDTH - PAD_RIGHT, VIEW_HEIGHT - PAD_BOTTOM);
    let y = Scale::new((low, high), (bottom, top));
    // Times as f64 milliseconds: exact for any date before the year 287 396.
    let x = Scale::new((first.at_ms as f64, last.at_ms as f64), (left, right));

    let dots: Vec<Dot> = points
        .iter()
        .zip(&values)
        .map(|(point, &value)| Dot {
            x: round1(x.map(point.at_ms as f64)),
            y: round1(y.map(value)),
        })
        .collect();
    let path = dots
        .iter()
        .enumerate()
        .map(|(index, dot)| {
            let command = if index == 0 { 'M' } else { 'L' };
            format!("{command}{} {}", dot.x, dot.y)
        })
        .collect::<Vec<_>>()
        .join(" ");

    let y_ticks = ticks
        .iter()
        .map(|&value| Tick {
            at: round1(y.map(value)),
            label: tick_label(value),
        })
        .collect();
    let mut x_ticks = vec![Tick {
        at: round1(x.map(first.at_ms as f64)),
        label: date_label(first.at_ms),
    }];
    let last_label = date_label(last.at_ms);
    if last.at_ms != first.at_ms && last_label != x_ticks[0].label {
        x_ticks.push(Tick {
            at: round1(x.map(last.at_ms as f64)),
            label: last_label,
        });
    }

    Some(ChartLayout {
        path,
        dots,
        y_ticks,
        x_ticks,
        plot: (left, top, right, bottom),
    })
}

/// One decimal is plenty for SVG coordinates and keeps the markup short.
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn point(day: i64, weight: f64) -> WeightPoint {
        WeightPoint {
            at_ms: day * 86_400_000,
            weight: kg(weight),
        }
    }

    fn day_label(ms: i64) -> String {
        format!("d{}", ms / 86_400_000)
    }

    #[test]
    fn scales_map_linearly_and_can_flip() {
        let scale = Scale::new((0.0, 100.0), (0.0, 200.0));
        assert!((scale.map(50.0) - 100.0).abs() < 1e-9);
        let flipped = Scale::new((0.0, 100.0), (150.0, 10.0));
        assert!((flipped.map(0.0) - 150.0).abs() < 1e-9);
        assert!((flipped.map(100.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_domain_maps_to_the_middle() {
        let scale = Scale::new((5.0, 5.0), (10.0, 30.0));
        assert!((scale.map(5.0) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn nice_steps_are_1_2_2_5_or_5_times_a_power_of_ten() {
        assert!((nice_step(40.0, 4) - 10.0).abs() < 1e-9);
        assert!((nice_step(9.0, 4) - 2.5).abs() < 1e-9);
        assert!((nice_step(7.0, 4) - 2.0).abs() < 1e-9);
        assert!((nice_step(170.0, 4) - 50.0).abs() < 1e-9);
        assert!((nice_step(0.3, 4) - 0.1).abs() < 1e-9);
        assert!((nice_step(0.0, 4) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ticks_are_round_and_cover_the_values() {
        assert_eq!(y_ticks(100.0, 140.0), [100.0, 110.0, 120.0, 130.0, 140.0]);
        assert_eq!(y_ticks(102.5, 117.5), [100.0, 105.0, 110.0, 115.0, 120.0]);
        let ticks = y_ticks(61.0, 187.0);
        assert!(ticks.len() <= MAX_TICKS, "{ticks:?}");
        assert!(
            ticks[0] <= 61.0 && *ticks.last().unwrap() >= 187.0,
            "{ticks:?}"
        );
    }

    #[test]
    fn ticks_never_exceed_the_maximum() {
        for (min, max) in [
            (0.0, 1.0),
            (19.0, 21.0),
            (97.5, 102.5),
            (1.0, 999.0),
            (44.09, 330.69),
        ] {
            let ticks = y_ticks(min, max);
            assert!(
                (2..=MAX_TICKS).contains(&ticks.len()),
                "{min}..{max}: {ticks:?}"
            );
            assert!(
                ticks[0] <= min && *ticks.last().unwrap() >= max,
                "{ticks:?}"
            );
        }
    }

    #[test]
    fn a_flat_series_gets_a_range_around_it() {
        // 100 ± 5 → 95..105, in 2.5 steps.
        assert_eq!(y_ticks(100.0, 100.0), [95.0, 97.5, 100.0, 102.5, 105.0]);
        // Near zero, at least ±1.
        let ticks = y_ticks(0.0, 0.0);
        assert!(
            ticks[0] <= -1.0 && *ticks.last().unwrap() >= 1.0,
            "{ticks:?}"
        );
    }

    #[test]
    fn tick_labels_have_no_trailing_zeros() {
        assert_eq!(tick_label(100.0), "100");
        assert_eq!(tick_label(102.5), "102.5");
        assert_eq!(tick_label(0.25), "0.25");
        assert_eq!(tick_label(-0.0), "0");
    }

    #[test]
    fn no_points_no_chart() {
        assert_eq!(layout(&[], Unit::Kg, day_label), None);
    }

    #[test]
    fn a_single_point_sits_in_the_middle() {
        let chart = layout(&[point(3, 100.0)], Unit::Kg, day_label).unwrap();
        let (left, top, right, bottom) = chart.plot;
        assert_eq!(chart.dots.len(), 1);
        let dot = chart.dots[0];
        assert!((dot.x - round1(f64::midpoint(left, right))).abs() < 1e-9);
        assert!((dot.y - round1(f64::midpoint(top, bottom))).abs() < 1e-9);
        assert_eq!(chart.path, format!("M{} {}", dot.x, dot.y));
        assert_eq!(chart.x_ticks.len(), 1);
        assert_eq!(chart.x_ticks[0].label, "d3");
        assert!(
            chart
                .dots
                .iter()
                .all(|dot| dot.x.is_finite() && dot.y.is_finite())
        );
    }

    #[test]
    fn the_line_runs_left_to_right_and_up_with_the_weight() {
        let points = [point(0, 100.0), point(7, 110.0), point(14, 120.0)];
        let chart = layout(&points, Unit::Kg, day_label).unwrap();
        let (left, top, right, bottom) = chart.plot;
        assert!((chart.dots[0].x - left).abs() < 1e-9);
        assert!((chart.dots[2].x - right).abs() < 1e-9);
        // Evenly spaced sessions are evenly spaced on the x axis.
        assert!((chart.dots[1].x - round1(f64::midpoint(left, right))).abs() < 0.11);
        // Heavier is higher (smaller y), within the plot.
        assert!(chart.dots[0].y > chart.dots[1].y && chart.dots[1].y > chart.dots[2].y);
        assert!(chart.dots.iter().all(|dot| dot.y >= top && dot.y <= bottom));
        assert!(chart.path.starts_with('M') && chart.path.matches(" L").count() == 2);
        // The y ticks run bottom to top and the lowest one is on the bottom edge.
        assert_eq!(chart.y_ticks.first().unwrap().label, "100");
        assert_eq!(chart.y_ticks.last().unwrap().label, "120");
        assert!((chart.y_ticks[0].at - bottom).abs() < 1e-9);
        let labels: Vec<_> = chart
            .x_ticks
            .iter()
            .map(|tick| tick.label.as_str())
            .collect();
        assert_eq!(labels, ["d0", "d14"]);
    }

    #[test]
    fn ticks_are_round_numbers_of_the_users_unit() {
        // 100 to 140 kg is 220.46 to 308.65 lb: the ticks are whole pounds, not converted kilos.
        let points = [point(0, 100.0), point(7, 140.0)];
        let chart = layout(&points, Unit::Lb, day_label).unwrap();
        let labels: Vec<_> = chart
            .y_ticks
            .iter()
            .map(|tick| tick.label.as_str())
            .collect();
        assert_eq!(labels, ["200", "250", "300", "350"]);
        let in_kg = layout(&points, Unit::Kg, day_label).unwrap();
        let labels: Vec<_> = in_kg
            .y_ticks
            .iter()
            .map(|tick| tick.label.as_str())
            .collect();
        assert_eq!(labels, ["100", "110", "120", "130", "140"]);
    }

    #[test]
    fn sessions_on_the_same_day_share_one_date_label() {
        let points = [
            WeightPoint {
                at_ms: 1_000,
                weight: kg(100.0),
            },
            WeightPoint {
                at_ms: 2_000,
                weight: kg(105.0),
            },
        ];
        let chart = layout(&points, Unit::Kg, day_label).unwrap();
        assert_eq!(chart.x_ticks.len(), 1);
    }
}
