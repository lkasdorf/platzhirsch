//! The folder tree as a table with size columns.
//!
//! Only the visible rows are drawn (`TableBody::rows`); everything computed
//! here per frame is per visible row. The row list itself lives in
//! [`TreeView`] and is rebuilt there only on change.
use eframe::egui::{self, Align, Color32, Key, Layout, Rect, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};

use super::rows::{NavKey, TreeView};
use crate::format::{format_bytes, format_count, format_date, format_percent};
use crate::model::{F_LINK, F_ONLINE_ONLY, F_UNREADABLE, Metric, NodeId, SortKey, Tree};
use crate::win;

pub struct TableResponse {
    pub selection_changed: bool,
}

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
        Col::Percent => match metric {
            Metric::Allocated => SortKey::Allocated,
            Metric::Size => SortKey::Size,
        },
        Col::Modified => SortKey::Modified,
    }
}

/// The sort order after a click on the header of `key`: the same column flips
/// the direction; a new column starts ascending for names, descending otherwise.
fn next_sort(current: (SortKey, bool), key: SortKey) -> (SortKey, bool) {
    if current.0 == key { (key, !current.1) } else { (key, key != SortKey::Name) }
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

/// The name cell: indentation, arrow, share bar, name and marker.
/// Returns true when the arrow was clicked.
fn name_cell(ui: &mut Ui, tree: &Tree, view: &TreeView, node: NodeId, depth: u16, row_height: f32) -> bool {
    let cell = ui.max_rect();
    let indent = (f32::from(depth) * INDENT).min((cell.width() - 2.0 * ARROW_WIDTH).max(0.0));
    ui.add_space(indent);
    let mut arrow_clicked = false;
    if tree.is_dir(node) && !tree.children(node).is_empty() {
        let (_, response) = ui.allocate_exact_size(vec2(ARROW_WIDTH, row_height), Sense::click());
        let openness = if view.is_expanded(node) { 1.0 } else { 0.0 };
        egui::collapsing_header::paint_default_icon(ui, openness, &response);
        arrow_clicked = response.clicked();
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
    ui.label(tree.name(node));
    if let Some(marker) = marker_text(tree.flags(node)) {
        ui.weak(marker);
    }
    arrow_clicked
}

/// The tree table. Draws only the visible rows; `scroll_to` asks for a row
/// index to be scrolled into view.
pub fn show(ui: &mut Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) -> TableResponse {
    let before = view.selected;
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

    let metric = view.metric();
    let (sort_key, desc) = view.sort();
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
                    let key = column_key(col, metric);
                    let sorted = (key == sort_key).then_some(desc);
                    if header_cell(&mut header, col, title, sorted) {
                        header_click = Some(key);
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
    if let Some(key) = header_click {
        let (key, desc) = next_sort((sort_key, desc), key);
        view.set_sort(tree, key, desc);
    }
    if let Some(node) = input.toggle {
        view.toggle(tree, node);
    }
    let changed = header_click.is_some() || input.select.is_some() || input.toggle.is_some();
    if changed {
        ctx.request_repaint();
    }
    TableResponse { selection_changed: view.selected != before }
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
        assert!(next_sort((SortKey::Allocated, true), SortKey::Size) == (SortKey::Size, true));
        assert!(next_sort((SortKey::Size, true), SortKey::Size) == (SortKey::Size, false));
        assert!(next_sort((SortKey::Size, true), SortKey::Name) == (SortKey::Name, false));
        assert!(column_key(Col::Percent, Metric::Size) == SortKey::Size);
        assert!(column_key(Col::Percent, Metric::Allocated) == SortKey::Allocated);
    }

    #[test]
    fn drive_root_path_gets_a_backslash() {
        let t = sample();
        assert_eq!(full_path(&t, 0), "C:\\");
        assert_eq!(full_path(&t, 5), "C:\\a\\b\\x");
    }
}
