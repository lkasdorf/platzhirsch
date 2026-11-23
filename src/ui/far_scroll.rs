//! Scrolling a list that is too long for egui to place exactly.
//!
//! egui keeps positions as `f32`. A table of two million rows is 46 million
//! points high, where neighbouring `f32` values are 4 points apart: the rows
//! land a few points off, and the further down the worse. So egui is never
//! told about more than [`NATIVE_ROWS`] rows. For a longer list the table
//! keeps the true scroll position itself, as `f64`, and gives egui a shorter
//! stand-in list: the rows egui draws are mapped to the real rows at that
//! position. The scroll bar still shows where in the whole list the view is,
//! and the wheel still moves the rows by as many points as it says.

/// Up to this many rows egui scrolls a list by itself. At 23 points a row
/// that is below six million points, where `f32` still has half a point.
pub const NATIVE_ROWS: usize = 250_000;

/// This many rows at either end of a longer list are their own stand-ins: the
/// wheel there moves egui's offset and the true position alike, so neither
/// runs into its end before the other. Even, to keep the stripes in step.
const EDGE_ROWS: usize = 1_000;

/// How many rows egui is told about for a list of `total` rows. The rows left
/// out are an even number, so a row and its stand-in are both odd or both
/// even and the stripes do not flip while scrolling.
pub fn virtual_rows(total: usize) -> usize {
    if total <= NATIVE_ROWS { total } else { NATIVE_ROWS - (total - NATIVE_ROWS) % 2 }
}

/// The sizes the mapping needs, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Rows in the whole list.
    pub total: usize,
    /// From the top of one row to the top of the next.
    pub step: f64,
    /// The gap below a row, part of `step`; the last row has none.
    pub spacing: f64,
    /// Height of the part of the list that is on screen.
    pub view: f64,
}

impl Geometry {
    /// Rows of the list egui is not told about.
    fn left_out(&self) -> usize {
        self.total - virtual_rows(self.total)
    }

    /// The largest scroll position: the last row ends at the bottom of the view.
    fn top_max(&self) -> f64 {
        (self.total as f64 * self.step - self.spacing - self.view).max(0.0)
    }

    /// The row at the top of the view at the largest scroll position.
    fn first_max(&self) -> usize {
        (self.top_max() / self.step).floor() as usize
    }

    /// Where the middle part of the real list ends and that of the stand-in
    /// list: before them the rows are spread evenly, from them on a row and
    /// its stand-in are `left_out` apart.
    fn middle_ends(&self) -> (usize, usize) {
        let real = (self.first_max().saturating_sub(EDGE_ROWS) & !1).max(EDGE_ROWS + self.left_out());
        (real, real - self.left_out())
    }

    /// The row egui shows at the top when the real row `first` is there: odd
    /// or even like it, and rising with it but for that (an odd row can come
    /// one after the even row that follows it).
    pub fn virtual_first(&self, first: usize) -> usize {
        let (real_end, virtual_end) = self.middle_ends();
        if self.left_out() == 0 || first < EDGE_ROWS {
            first
        } else if first >= real_end {
            first - self.left_out()
        } else {
            // Pairs of rows, so an odd row stays odd.
            let pair = ((first - EDGE_ROWS) / 2) as u128;
            let (to, from) = (((virtual_end - EDGE_ROWS) / 2) as u128, ((real_end - EDGE_ROWS) / 2) as u128);
            EDGE_ROWS + 2 * (pair * to / from) as usize + (first & 1)
        }
    }

    /// The real row at the top when egui shows its row `shown` there: the
    /// first one that [`Self::virtual_first`] maps to `shown`.
    pub fn real_first(&self, shown: usize) -> usize {
        let (real_end, virtual_end) = self.middle_ends();
        if self.left_out() == 0 || shown < EDGE_ROWS {
            shown
        } else if shown >= virtual_end {
            shown + self.left_out()
        } else {
            let pair = ((shown - EDGE_ROWS) / 2) as u128;
            let (to, from) = (((real_end - EDGE_ROWS) / 2) as u128, ((virtual_end - EDGE_ROWS) / 2) as u128);
            EDGE_ROWS + 2 * (pair * to).div_ceil(from) as usize + (shown & 1)
        }
    }
}

/// Where a long list is scrolled to, kept from one frame to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarScroll {
    /// From the top of the first row to the top of the view, in points.
    pub top: f64,
    /// The offset egui's scroll area had when `top` was brought up to date.
    pub offset: f32,
}

/// What to draw this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// To keep for the next frame. Its `offset` is the one egui's scroll area
    /// must have this frame.
    pub scroll: FarScroll,
    /// The real row at the top of the view.
    pub first: usize,
    /// The row egui draws there.
    pub virtual_first: usize,
}

impl Placement {
    /// The real row that egui's row `shown` stands for; `None` past either end.
    pub fn real_row(&self, shown: usize, total: usize) -> Option<usize> {
        let row = (self.first + shown).checked_sub(self.virtual_first)?;
        (row < total).then_some(row)
    }
}

/// Brings the scroll position up to date and says where the rows go.
///
/// `offset_now` is the offset of egui's scroll area as the last frame left it:
/// what `kept` asked for, changed by what the user did since. Dragging the
/// scroll bar (`bar_held`) puts the view at that share of the whole list. Any
/// other change, the wheel above all, moves the rows by that many points.
/// `scroll_to` is a row to bring into view with the smallest move; like egui,
/// it leaves the width of a gap between the row and the edge of the view.
pub fn place(
    kept: Option<FarScroll>,
    offset_now: f32,
    bar_held: bool,
    scroll_to: Option<usize>,
    geo: &Geometry,
) -> Placement {
    let now = f64::from(offset_now);
    let mut top = match kept {
        // Coming from a list short enough for egui: its offset was the position.
        None => now,
        Some(kept) if kept.offset == offset_now => kept.top,
        Some(_) if bar_held => {
            let shown = (now / geo.step).floor();
            geo.real_first(shown as usize) as f64 * geo.step + (now - shown * geo.step)
        }
        Some(kept) => kept.top + (now - f64::from(kept.offset)),
    };
    if let Some(row) = scroll_to {
        let row = row.min(geo.total.saturating_sub(1)) as f64;
        // The row with the gap below it, and a gap to the edge on either side.
        let (above, below) = (row * geo.step - geo.spacing, (row + 1.0) * geo.step + geo.spacing);
        if above < top {
            top = above;
        } else if below > top + geo.view {
            top = below - geo.view;
        }
    }
    let top = top.clamp(0.0, geo.top_max());
    let first = (top / geo.step).floor() as usize;
    let virtual_first = geo.virtual_first(first);
    let offset = (virtual_first as f64 * geo.step + (top - first as f64 * geo.step)) as f32;
    Placement { scroll: FarScroll { top, offset }, first, virtual_first }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(total: usize) -> Geometry {
        Geometry { total, step: 23.0, spacing: 3.0, view: 365.0 }
    }

    #[test]
    fn short_lists_are_left_to_egui() {
        for total in [0, 1, 1_000, NATIVE_ROWS] {
            assert_eq!(virtual_rows(total), total);
            let g = geo(total);
            for row in [0, 1, total / 2, total.saturating_sub(1)] {
                assert_eq!((g.virtual_first(row), g.real_first(row)), (row, row), "{total} rows, row {row}");
            }
        }
    }

    #[test]
    fn long_lists_get_a_stand_in_of_fixed_length() {
        assert_eq!(virtual_rows(NATIVE_ROWS + 1), NATIVE_ROWS - 1);
        assert_eq!(virtual_rows(NATIVE_ROWS + 2), NATIVE_ROWS);
        assert_eq!(virtual_rows(2_000_001), NATIVE_ROWS - 1);
        assert_eq!(virtual_rows(u32::MAX as usize), NATIVE_ROWS - 1);
        for total in [NATIVE_ROWS + 1, NATIVE_ROWS + 2, 300_000, 2_000_001, u32::MAX as usize] {
            assert_eq!((total - virtual_rows(total)) % 2, 0, "{total}: an even number of rows is left out");
        }
    }

    #[test]
    fn stand_in_rows_rise_with_the_rows_and_meet_them_at_both_ends() {
        for total in [NATIVE_ROWS + 1, NATIVE_ROWS + 2, 300_000, 2_000_001, 50_000_000, u32::MAX as usize] {
            let g = geo(total);
            let (first_max, shown) = (g.first_max(), virtual_rows(total));
            // The ends: the first rows are their own stand-ins, the last ones end where egui's list ends.
            assert_eq!(g.virtual_first(0), 0);
            assert_eq!(g.virtual_first(EDGE_ROWS - 1), EDGE_ROWS - 1);
            assert_eq!(first_max - g.virtual_first(first_max), total - shown, "{total}");
            assert_eq!(g.virtual_first(first_max - 500), g.virtual_first(first_max) - 500, "{total}");
            // In between: rising but for odd and even, never past the end, odd for odd.
            let step = (first_max / 20_000).max(1);
            let mut last = 0;
            let mut row = 0;
            while row <= first_max {
                for r in [row, row + 1] {
                    let v = g.virtual_first(r.min(first_max));
                    assert!(v + 1 >= last, "{total}: row {r} maps to {v} after {last}");
                    assert_eq!(v % 2, r.min(first_max) % 2, "{total}: row {r} maps to {v}");
                    assert!(v <= first_max - (total - shown), "{total}: row {r} maps to {v}");
                    last = last.max(v);
                }
                row += step;
            }
        }
    }

    #[test]
    fn a_stand_in_row_leads_back_to_itself() {
        for total in [NATIVE_ROWS + 1, NATIVE_ROWS + 2, 300_000, 2_000_001, 50_000_000, u32::MAX as usize] {
            let g = geo(total);
            let last = g.virtual_first(g.first_max());
            let step = (last / 20_000).max(1);
            for shown in (0..=last).step_by(step).chain([1, EDGE_ROWS - 1, EDGE_ROWS, EDGE_ROWS + 1, last - 1, last]) {
                let real = g.real_first(shown);
                assert_eq!(g.virtual_first(real), shown, "{total}: {shown} -> {real}");
                assert_eq!(real % 2, shown % 2);
            }
            // The middle of the bar is the middle of the list, to within a thousandth.
            let middle = g.real_first(last / 2) as f64 / g.first_max() as f64;
            assert!((middle - 0.5).abs() < 0.001, "{total}: {middle}");
        }
    }

    /// A list of two million rows scrolled as in the table.
    fn two_million() -> Geometry {
        geo(2_000_001)
    }

    #[test]
    fn coming_from_a_short_list_the_position_is_egui_s_offset() {
        let g = two_million();
        let p = place(None, 460.0, false, None, &g);
        assert_eq!((p.scroll.top, p.first, p.virtual_first), (460.0, 20, 20));
        assert_eq!(p.scroll.offset, 460.0);
        // Nothing changed: nothing moves, however often it is asked.
        assert_eq!(place(Some(p.scroll), p.scroll.offset, false, None, &g), p);
    }

    #[test]
    fn the_wheel_moves_the_rows_by_its_points_anywhere_in_the_list() {
        let g = two_million();
        // Row 1,000,000 at the top, 5 points of it scrolled out.
        let top = 1_000_000.0 * 23.0 + 5.0;
        let start = place(Some(FarScroll { top, offset: 0.0 }), 0.0, false, None, &g);
        assert_eq!(start.first, 1_000_000);
        assert_eq!(start.scroll.offset, start.virtual_first as f32 * 23.0 + 5.0);
        assert!(start.virtual_first > 100_000 && start.virtual_first < 150_000, "{}", start.virtual_first);
        // The wheel: egui moved its offset by 230 points, ten rows.
        let down = place(Some(start.scroll), start.scroll.offset + 230.0, false, None, &g);
        assert_eq!((down.first, down.scroll.top), (1_000_010, top + 230.0));
        // egui's offset goes back to where the bar belongs, with the same part of a row scrolled out.
        assert_eq!(down.scroll.offset - down.virtual_first as f32 * 23.0, 5.0);
        assert!(down.virtual_first.abs_diff(start.virtual_first) <= 2);
        let up = place(Some(down.scroll), down.scroll.offset - 240.0, false, None, &g);
        assert_eq!((up.first, up.scroll.top), (999_999, top - 10.0));
        // Past the ends it stops.
        let first = place(Some(FarScroll { top: 30.0, offset: 30.0 }), 0.0, false, None, &g);
        assert_eq!((first.first, first.scroll.top, first.scroll.offset), (0, 0.0, 0.0));
        let end = place(Some(FarScroll { top: g.top_max() - 10.0, offset: 1000.0 }), 1100.0, false, None, &g);
        assert_eq!(end.scroll.top, g.top_max());
        // 365 points hold the last 16 rows but for the gap below the last one.
        assert_eq!(end.first, 2_000_001 - 16);
        assert_eq!(end.real_row(end.virtual_first + 15, g.total), Some(2_000_000));
    }

    #[test]
    fn the_scroll_bar_goes_to_that_share_of_the_list() {
        let g = two_million();
        let kept = FarScroll { top: 0.0, offset: 0.0 };
        let shown = virtual_rows(g.total) as f32;
        // The bar dragged to the middle of egui's list, 7 points into a row.
        let middle = place(Some(kept), (shown / 2.0).floor() * 23.0 + 7.0, true, None, &g);
        let share = middle.first as f64 / g.total as f64;
        assert!((share - 0.5).abs() < 0.001, "{share}");
        // egui keeps the offset the drag gave it, so the handle stays under the pointer.
        assert_eq!(middle.scroll.offset, (shown / 2.0).floor() * 23.0 + 7.0);
        // To the very end: the last row is the last one drawn.
        let max = shown * 23.0 - 3.0 - 365.0;
        let end = place(Some(middle.scroll), max, true, None, &g);
        assert_eq!(end.scroll.top, g.top_max());
        assert_eq!(end.scroll.offset, max);
        assert_eq!(end.real_row(end.virtual_first + 15, g.total), Some(2_000_000));
        assert_eq!(end.real_row(end.virtual_first + 16, g.total), None);
        // And back to the start.
        let start = place(Some(end.scroll), 0.0, true, None, &g);
        assert_eq!((start.first, start.scroll.top), (0, 0.0));
        assert_eq!(start.real_row(0, g.total), Some(0));
    }

    #[test]
    fn a_row_is_brought_into_view_with_the_smallest_move() {
        let g = two_million();
        let kept = FarScroll { top: 1_000_000.0 * 23.0, offset: 0.0 };
        let at = |row| place(Some(kept), 0.0, false, Some(row), &g).scroll.top;
        // In view already, with 3 points to spare at either edge: rows 1,000,001 to 1,000,014.
        assert_eq!(at(1_000_001), kept.top);
        assert_eq!(at(1_000_014), kept.top);
        // Below: its lower edge, gap included, comes 3 points above the bottom of the view.
        assert_eq!(at(1_000_015), 1_000_016.0 * 23.0 + 3.0 - 365.0);
        // Above, or at the very edge: its top comes 3 points below the top.
        assert_eq!(at(1_000_000), kept.top - 3.0);
        assert_eq!(at(999_999), 999_999.0 * 23.0 - 3.0);
        assert_eq!(at(0), 0.0);
        // The last row, and a row that is not there.
        assert_eq!(at(2_000_000), g.top_max());
        assert_eq!(at(usize::MAX), g.top_max());
    }

    #[test]
    fn rows_keep_their_place_to_the_point_at_any_size() {
        // The last row of four billion: its top is where the view's height says.
        let g = geo(u32::MAX as usize);
        let end = place(Some(FarScroll { top: 0.0, offset: 0.0 }), 0.0, false, Some(g.total - 1), &g);
        let last = g.total - 1;
        let shown = last - end.first + end.virtual_first;
        assert_eq!(end.real_row(shown, g.total), Some(last));
        // Its bottom edge in the view: where egui draws its stand-in.
        let bottom = f64::from(shown as f32 * 23.0 + 20.0 - end.scroll.offset);
        assert_eq!(bottom, 365.0);
    }
}
