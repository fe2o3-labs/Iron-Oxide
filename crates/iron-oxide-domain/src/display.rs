//! Formatting helpers shared by the `Display` impls.

use std::fmt::{self, Alignment, Write};

/// Writes `text` honouring the formatter's width, fill and alignment (left by default, like
/// strings). Unlike [`fmt::Formatter::pad`], it never truncates to the precision, because our
/// `Display` impls use the precision for decimals instead.
pub(crate) fn pad(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    let length = text.chars().count();
    let padding = f.width().map_or(0, |width| width.saturating_sub(length));
    if padding == 0 {
        return f.write_str(text);
    }
    let (before, after) = match f.align() {
        Some(Alignment::Right) => (padding, 0),
        Some(Alignment::Center) => (padding / 2, padding - padding / 2),
        Some(Alignment::Left) | None => (0, padding),
    };
    let fill = f.fill();
    for _ in 0..before {
        f.write_char(fill)?;
    }
    f.write_str(text)?;
    for _ in 0..after {
        f.write_char(fill)?;
    }
    Ok(())
}
