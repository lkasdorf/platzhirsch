//! The folder tree as a table with size columns.
//!
//! Only the visible rows are drawn (`TableBody::rows`); everything computed
//! here per frame is per visible row. The row list itself lives in
//! [`TreeView`] and never changes per frame.
use eframe::egui::{self, Align, Color32, Id, Key, Layout, Popup, Rect, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use super::rows::{NavKey, TreeView, metric_key};
use crate::format::{format_bytes, format_count, format_date, format_percent};
use crate::model::{F_LINK, F_ONLINE_ONLY, F_UNREADABLE, Metric, NodeId, SortKey, Tree};
use crate::win;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Allocated,
    Size,
    Files,
    Folders,
    Percent,
    Modified,
}

const COLUMNS: [(Col, &str); 7] = [
    (Col::Name, "Name"),
    (Col::Allocated, "Allocated"),
    (Col::Size, "Size"),
    (Col::Files, "Files"),
    (Col::Folders, "Folders"),
    (Col::Percent, "% of Parent"),
    (Col::Modified, "Modified"),
];

/// Indentation per tree level.
const INDENT: f32 = 16.0;
/// The indentation takes at most this share of the Name column.
const MAX_INDENT_SHARE: f32 = 0.4;
/// Width of the expand/collapse arrow.
const ARROW_WIDTH: f32 = 14.0;
/// Colour of the share bar behind the name.
const BAR_COLOR: Color32 = Color32::from_rgba_unmultiplied_const(240, 170, 50, 110);

/// The note shown behind an entry's name. `unreadable` wins over
/// `online-only`, which wins over `link`.
pub fn marker_text(flags: u8) -> Option<&'static str> {
    if flags & F_UNREADABLE != 0 {
        Some("unreadable")
    } else if flags & F_ONLINE_ONLY != 0 {
        Some("online-only")
    } else if flags & F_LINK != 0 {
        Some("link")
    } else {
        None
    }
}

/// The share of the parent folder in `metric`, in `0.0..=1.0`.
/// The root is `1.0`; a parent with value 0 gives `0.0`.
pub fn bar_fraction(tree: &Tree, node: NodeId, metric: Metric) -> f32 {
    let Some(parent) = tree.parent(node) else { return 1.0 };
    let whole = tree.metric(parent, metric);
    if whole == 0 {
        return 0.0;
    }
    (tree.metric(node, metric) as f64 / whole as f64).clamp(0.0, 1.0) as f32
}

/// The sort key behind a column; `% of Parent` sorts by the chosen metric.
fn column_key(col: Col, metric: Metric) -> SortKey {
    match col {
        Col::Name => SortKey::Name,
        Col::Allocated => SortKey::Allocated,
        Col::Size => SortKey::Size,
        Col::Files => SortKey::Files,
        Col::Folders => SortKey::Dirs,
        Col::Percent => metric_key(metric),
        Col::Modified => SortKey::Modified,
    }
}

/// The one column that shows the sort triangle: `% of Parent` if it was the
/// last clicked header and the rows are sorted by the chosen metric, otherwise
/// the column of the sort key.
fn sorted_column(key: SortKey, metric: Metric, percent_sorted: bool) -> Col {
    if percent_sorted && key == column_key(Col::Percent, metric) {
        return Col::Percent;
    }
    match key {
        SortKey::Name => Col::Name,
        SortKey::Allocated => Col::Allocated,
        SortKey::Size => Col::Size,
        SortKey::Files => Col::Files,
        SortKey::Dirs => Col::Folders,
        SortKey::Modified => Col::Modified,
    }
}

/// The sort order after a click on `clicked` while `shown` has the triangle:
/// the same column flips the direction; another column starts ascending for
/// names, descending otherwise.
fn next_sort(current: (SortKey, bool), shown: Col, clicked: Col, metric: Metric) -> (SortKey, bool) {
    let key = column_key(clicked, metric);
    if clicked == shown { (key, !current.1) } else { (key, key != SortKey::Name) }
}

/// The path handed to Explorer and the clipboard; a drive root `C:` becomes `C:\`.
fn full_path(tree: &Tree, node: NodeId) -> String {
    let mut path = tree.path(node);
    if path.ends_with(':') {
        path.push('\\');
    }
    path
}

/// Text in a cell, right-aligned.
fn right_label(ui: &mut Ui, text: String) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text));
}

/// A small triangle showing the sort direction.
fn sort_arrow(ui: &mut Ui, desc: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 6.0), Sense::hover());
    let rotation = if desc { 0.0 } else { std::f32::consts::PI };
    let color = ui.visuals().strong_text_color();
    ui.painter().add(egui::Shape::rotated_triangle(rect, rotation, color));
}

/// One header cell; returns true when it was clicked.
fn header_cell(header: &mut TableRow<'_, '_>, col: Col, title: &str, sorted: Option<bool>) -> bool {
    let (_, response) = header.col(|ui| {
        if col == Col::Name {
            ui.strong(title);
            if let Some(desc) = sorted {
                sort_arrow(ui, desc);
            }
        } else {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(desc) = sorted {
                    sort_arrow(ui, desc);
                }
                ui.strong(title);
            });
        }
    });
    response.clicked()
}

/// What the user did with the rows this frame; applied after drawing.
#[derive(Default)]
struct RowInput {
    /// Row index to select.
    select: Option<usize>,
    toggle: Option<NodeId>,
}

/// The indentation of a row at `depth` in a Name column `width` wide:
/// `INDENT` per level, but at most `MAX_INDENT_SHARE` of the column.
fn indent_width(depth: u32, width: f32) -> f32 {
    (depth as f32 * INDENT).min(width.max(0.0) * MAX_INDENT_SHARE)
}

/// The name cell: indentation, arrow, share bar, name and marker.
/// Returns true when the arrow was clicked; the second click of a
/// double-click does not count, so a double-click toggles once.
fn name_cell(ui: &mut Ui, tree: &Tree, view: &TreeView, node: NodeId, depth: u32, row_height: f32) -> bool {
    let cell = ui.max_rect();
    ui.add_space(indent_width(depth, cell.width()));
    let mut arrow_clicked = false;
    if tree.is_dir(node) && !tree.children(node).is_empty() {
        let (_, response) = ui.allocate_exact_size(vec2(ARROW_WIDTH, row_height), Sense::click());
        let openness = if view.is_expanded(node) { 1.0 } else { 0.0 };
        egui::collapsing_header::paint_default_icon(ui, openness, &response);
        arrow_clicked = response.clicked() && !response.double_clicked();
    } else {
        ui.add_space(ARROW_WIDTH);
    }
    let start = ui.cursor().min.x;
    let fraction = bar_fraction(tree, node, view.metric());
    if fraction > 0.0 && cell.right() > start {
        let width = (cell.right() - start) * fraction;
        let bar = Rect::from_min_size(
            egui::pos2(start, cell.center().y - row_height * 0.35),
            vec2(width, row_height * 0.7),
        );
        ui.painter().rect_filled(bar, 2.0, BAR_COLOR);
    }
    // The full path replaces egui's own tooltip for a cut-off name; it is
    // built only while the name is hovered.
    ui.add(egui::Label::new(tree.name(node)).show_tooltip_when_elided(false))
        .on_hover_ui(|ui| {
            ui.set_max_width(ui.spacing().tooltip_width);
            ui.label(full_path(tree, node));
        });
    if let Some(marker) = marker_text(tree.flags(node)) {
        ui.weak(marker);
    }
    arrow_clicked
}

/// The tree table. Draws only the visible rows; `scroll_to` asks for a row
/// index to be scrolled into view.
pub fn show(ui: &mut Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) {
    let ctx = ui.ctx().clone();
    let mut scroll_to = scroll_to;

    // Arrow keys, unless a widget has keyboard focus or a menu is open.
    if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() {
        for (key, nav) in [
            (Key::ArrowUp, NavKey::Up),
            (Key::ArrowDown, NavKey::Down),
            (Key::ArrowLeft, NavKey::Left),
            (Key::ArrowRight, NavKey::Right),
        ] {
            for _ in 0..ctx.input(|i| i.num_presses(key)) {
                scroll_to = Some(view.key_nav(tree, nav));
            }
        }
    }

    // A context menu belongs to the row index it was opened on; when the
    // rows have changed since the last frame, that index shows another entry.
    let version_id = Id::unique("tree_table_rows_version");
    let version = view.rows_version();
    let rows_changed = ctx.data_mut(|d| {
        let last = d.get_temp::<u64>(version_id);
        d.insert_temp(version_id, version);
        last.is_some_and(|last| last != version)
    });

    let metric = view.metric();
    let (sort_key, desc) = view.sort();
    let shown = sorted_column(sort_key, metric, view.percent_sorted());
    let row_height = ui.spacing().interact_size.y;
    let mut header_click = None;
    let mut input = RowInput::default();

    ui.scope(|ui| {
        // Labels must not swallow the clicks that select rows.
        ui.style_mut().interaction.selectable_labels = false;
        let mut builder = TableBuilder::new(ui)
            .id_salt("tree_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::initial(90.0).at_least(60.0))
            .column(Column::initial(90.0).at_least(60.0))
            .column(Column::initial(80.0).at_least(50.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(85.0).at_least(60.0))
            .column(Column::initial(90.0).at_least(70.0));
        if let Some(row) = scroll_to {
            builder = builder.scroll_to_row(row, None);
        }
        builder
            .header(row_height, |mut header| {
                for (col, title) in COLUMNS {
                    let sorted = (col == shown).then_some(desc);
                    if header_cell(&mut header, col, title, sorted) {
                        header_click = Some(col);
                    }
                }
            })
            .body(|body| {
                let rows = view.rows();
                body.rows(row_height, rows.len(), |mut row| {
                    let r = rows[row.index()];
                    let node = r.node;
                    let is_dir = tree.is_dir(node);
                    row.set_selected(node == view.selected);
                    row.col(|ui| {
                        if name_cell(ui, tree, view, node, r.depth, row_height) {
                            input.toggle = Some(node);
                        }
                    });
                    row.col(|ui| right_label(ui, format_bytes(tree.allocated(node))));
                    row.col(|ui| right_label(ui, format_bytes(tree.size(node))));
                    row.col(|ui| {
                        if is_dir {
                            right_label(ui, format_count(u64::from(tree.file_count(node))));
                        }
                    });
                    row.col(|ui| {
                        if is_dir {
                            right_label(ui, format_count(u64::from(tree.dir_count(node))));
                        }
                    });
                    row.col(|ui| {
                        let percent = match tree.parent(node) {
                            None => format_percent(1, 1),
                            Some(p) => format_percent(tree.metric(node, metric), tree.metric(p, metric)),
                        };
                        right_label(ui, percent);
                    });
                    row.col(|ui| right_label(ui, format_date(tree.mtime(node))));

                    let response = row.response();
                    if response.clicked() || response.secondary_clicked() {
                        input.select = Some(row.index());
                    }
                    if response.double_clicked() && is_dir {
                        input.toggle = Some(node);
                    }
                    if rows_changed {
                        Popup::close_id(&ctx, Popup::default_response_id(&response));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Show in Explorer").clicked() {
                            win::reveal_in_explorer(&full_path(tree, node));
                            ui.close();
                        }
                        if ui.button("Copy path").clicked() {
                            ui.ctx().copy_text(full_path(tree, node));
                            ui.close();
                        }
                    });
                });
            });
    });

    // The row index belongs to the rows drawn this frame, so select before re-sorting.
    if let Some(index) = input.select {
        view.select_row(index);
    }
    if let Some(col) = header_click {
        let (key, desc) = next_sort((sort_key, desc), shown, col, metric);
        view.set_sort(tree, key, desc);
        if col == Col::Percent {
            view.set_percent_sorted(true);
        }
    }
    if let Some(node) = input.toggle {
        view.toggle(tree, node);
    }
    let changed = header_click.is_some() || input.select.is_some() || input.toggle.is_some();
    if changed {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{F_DIR, NO_PARENT, TreeBuilder};

    fn sample() -> Tree {
        let mut b = TreeBuilder::with_capacity(8, 64);
        let root = b.push("C:", NO_PARENT, 0, 0, 10, F_DIR); // 0
        let a = b.push("a", root, 0, 0, 20, F_DIR); // 1
        b.push("big.bin", a, 1000, 4096, 50, 0); // 2
        b.push("Small.txt", a, 10, 0, 30, 0); // 3
        let bd = b.push("b", a, 0, 0, 5, F_DIR); // 4
        b.push("x", bd, 5, 4096, 40, 0); // 5
        b.push("top.txt", root, 1, 4096, 1, 0); // 6
        let empty = b.push("empty", root, 0, 0, 0, F_DIR); // 7
        b.push("zero.txt", empty, 0, 0, 0, 0); // 8
        b.finish(root).0
    }

    #[test]
    fn marker_names_the_flag() {
        assert_eq!(marker_text(F_DIR), None);
        assert_eq!(marker_text(0), None);
        assert_eq!(marker_text(F_DIR | F_UNREADABLE), Some("unreadable"));
        assert_eq!(marker_text(F_DIR | F_LINK), Some("link"));
        assert_eq!(marker_text(F_DIR | F_ONLINE_ONLY), Some("online-only"));
    }

    #[test]
    fn marker_prefers_unreadable_then_online_only() {
        assert_eq!(marker_text(F_DIR | F_UNREADABLE | F_ONLINE_ONLY | F_LINK), Some("unreadable"));
        assert_eq!(marker_text(F_ONLINE_ONLY | F_LINK), Some("online-only"));
    }

    #[test]
    fn bar_is_share_of_parent() {
        let t = sample();
        assert_eq!(bar_fraction(&t, 0, Metric::Allocated), 1.0);
        assert!((bar_fraction(&t, 1, Metric::Allocated) - 8192.0 / 12288.0).abs() < 1e-6);
        assert!((bar_fraction(&t, 2, Metric::Size) - 1000.0 / 1015.0).abs() < 1e-6);
        assert_eq!(bar_fraction(&t, 3, Metric::Allocated), 0.0);
    }

    #[test]
    fn bar_of_child_in_empty_folder_is_zero() {
        let t = sample();
        assert_eq!(bar_fraction(&t, 8, Metric::Allocated), 0.0);
        assert_eq!(bar_fraction(&t, 8, Metric::Size), 0.0);
    }

    #[test]
    fn header_click_sorts_and_second_click_flips() {
        let a = Metric::Allocated;
        assert!(next_sort((SortKey::Allocated, true), Col::Allocated, Col::Size, a) == (SortKey::Size, true));
        assert!(next_sort((SortKey::Size, true), Col::Size, Col::Size, a) == (SortKey::Size, false));
        assert!(next_sort((SortKey::Size, true), Col::Size, Col::Name, a) == (SortKey::Name, false));
        assert!(column_key(Col::Percent, Metric::Size) == SortKey::Size);
        assert!(column_key(Col::Percent, Metric::Allocated) == SortKey::Allocated);
    }

    #[test]
    fn percent_header_first_click_sorts_descending_then_flips() {
        let a = Metric::Allocated;
        // Default: sorted by Allocated, triangle on Allocated. A click on % of Parent
        // is a new column, so it starts descending instead of flipping.
        assert!(next_sort((SortKey::Allocated, true), Col::Allocated, Col::Percent, a) == (SortKey::Allocated, true));
        assert!(next_sort((SortKey::Allocated, true), Col::Percent, Col::Percent, a) == (SortKey::Allocated, false));
        // And back from % of Parent to Allocated starts descending again.
        assert!(next_sort((SortKey::Allocated, false), Col::Percent, Col::Allocated, a) == (SortKey::Allocated, true));
    }

    #[test]
    fn triangle_defaults_to_the_metric_column() {
        assert!(sorted_column(SortKey::Allocated, Metric::Allocated, false) == Col::Allocated);
        assert!(sorted_column(SortKey::Size, Metric::Size, false) == Col::Size);
    }

    #[test]
    fn triangle_stays_on_percent_after_metric_switch() {
        assert!(sorted_column(SortKey::Allocated, Metric::Allocated, true) == Col::Percent);
        // set_metric moved the key to Size; the triangle stays on % of Parent.
        assert!(sorted_column(SortKey::Size, Metric::Size, true) == Col::Percent);
    }

    #[test]
    fn triangle_on_other_clicked_column() {
        let m = Metric::Allocated;
        assert!(sorted_column(SortKey::Name, m, false) == Col::Name);
        assert!(sorted_column(SortKey::Files, m, false) == Col::Files);
        assert!(sorted_column(SortKey::Dirs, m, false) == Col::Folders);
        assert!(sorted_column(SortKey::Modified, m, false) == Col::Modified);
        // The metric column that is not the chosen one.
        assert!(sorted_column(SortKey::Size, m, false) == Col::Size);
    }

    #[test]
    fn drive_root_path_gets_a_backslash() {
        let t = sample();
        assert_eq!(full_path(&t, 0), "C:\\");
        assert_eq!(full_path(&t, 5), "C:\\a\\b\\x");
    }

    #[test]
    fn indent_is_capped_at_forty_percent_of_the_column() {
        assert_eq!(indent_width(0, 300.0), 0.0);
        assert_eq!(indent_width(3, 300.0), 48.0);
        assert_eq!(indent_width(70_000, 300.0), 120.0);
        assert_eq!(indent_width(u32::MAX, 160.0), 64.0);
        assert_eq!(indent_width(5, -1.0), 0.0);
    }

    // Headless tests of `show`: egui_kittest drives a plain `egui::Context`
    // with synthetic input. No window, no renderer, simulated time only.
    mod ui {
        use super::*;
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;
        use eframe::egui::{Event, Modifiers, PointerButton, Pos2};

        struct State {
            tree: Tree,
            view: TreeView,
        }

        fn harness() -> Harness<'static, State> {
            let tree = sample();
            let view = TreeView::new(&tree, Metric::Allocated);
            Harness::builder()
                .with_size(vec2(900.0, 400.0))
                // Short frames, so two clicks two frames apart are a double-click.
                .with_step_dt(0.05)
                .build_ui_state(|ui, s: &mut State| show(ui, &s.tree, &mut s.view, None), State { tree, view })
        }

        /// Centre of the label showing `text`.
        fn label_center(h: &Harness<'_, State>, text: &str) -> Pos2 {
            h.get_by_label(text).rect().center()
        }

        /// Centre of the expand arrow in front of the name `text`.
        fn arrow_center(h: &Harness<'_, State>, text: &str) -> Pos2 {
            let label = h.get_by_label(text).rect();
            let gap = h.ctx.global_style().spacing.item_spacing.x;
            egui::pos2(label.left() - gap - ARROW_WIDTH / 2.0, label.center().y)
        }

        fn click(h: &mut Harness<'_, State>, pos: Pos2, button: PointerButton) {
            h.hover_at(pos);
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
            }
            h.run();
        }

        #[test]
        fn click_selects_the_row_and_arrow_down_moves_from_it() {
            let mut h = harness();
            assert_eq!(h.state().view.selected, 0);
            let pos = label_center(&h, "a");
            click(&mut h, pos, PointerButton::Primary);
            assert_eq!(h.state().view.selected, 1);
            h.key_press(Key::ArrowDown);
            h.run();
            // Rows: C:, a, empty, top.txt
            assert_eq!(h.state().view.selected, 7);
        }

        #[test]
        fn arrow_click_expands_without_selecting() {
            let mut h = harness();
            let pos = arrow_center(&h, "a");
            click(&mut h, pos, PointerButton::Primary);
            assert!(h.state().view.is_expanded(1));
            assert_eq!(h.state().view.selected, 0);
            assert!(h.query_by_label("big.bin").is_some());
        }

        #[test]
        fn header_click_sorts_and_second_click_reverses() {
            let mut h = harness();
            let pos = label_center(&h, "Size");
            click(&mut h, pos, PointerButton::Primary);
            assert!(h.state().view.sort() == (SortKey::Size, true));
            // Far apart in time, so the second click is not a double-click.
            h.run_steps(20);
            click(&mut h, pos, PointerButton::Primary);
            assert!(h.state().view.sort() == (SortKey::Size, false));
        }

        #[test]
        fn double_click_on_the_arrow_toggles_once() {
            let mut h = harness();
            let pos = arrow_center(&h, "a");
            h.hover_at(pos);
            for _ in 0..2 {
                for pressed in [true, false] {
                    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
                }
            }
            h.run();
            assert!(h.state().view.is_expanded(1));
            assert_eq!(h.state().view.selected, 0);
        }

        #[test]
        fn hovering_a_name_shows_its_full_path() {
            let mut h = harness();
            h.state_mut().view.reveal(&sample(), 5);
            h.run();
            assert!(h.query_by_label("C:\\a\\b\\x").is_none());
            let pos = label_center(&h, "x");
            h.hover_at(pos);
            // Past the tooltip delay, in simulated time.
            h.run_steps(20);
            assert!(h.query_by_label("C:\\a\\b\\x").is_some());
        }

        #[test]
        fn context_menu_closes_when_the_rows_change() {
            let mut h = harness();
            let pos = label_center(&h, "a");
            click(&mut h, pos, PointerButton::Secondary);
            assert!(h.query_by_label("Copy path").is_some());
            let s = h.state_mut();
            s.view.toggle(&s.tree, 7); // expands "empty", below the menu's row
            h.run();
            assert!(h.query_by_label("Copy path").is_none());
            assert!(h.query_by_label("zero.txt").is_some());
        }
    }
}
