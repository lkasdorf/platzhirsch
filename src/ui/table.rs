//! The folder tree as a table with size columns.
//!
//! Only the visible rows are drawn (`TableBody::rows`); everything computed
//! here per frame is per visible row. The row list itself lives in
//! [`TreeView`] and never changes per frame.
//!
//! For screen readers the table is one tree: each drawn row is one item whose
//! name repeats what the row shows, and the selected row is the item the
//! keyboard focus is on. The cells' own texts are hidden from them.
use eframe::egui::{
    self, Align, Color32, Context, Event, FocusDirection, Id, IdSalt, Key, Layout, Popup, Rangef, Rect, Response, Role,
    ScrollArea, Sense, StrokeKind, Ui, UiBuilder, WidgetInfo, accesskit, scroll_area, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};

use super::far_scroll::{self, FarScroll, Geometry};
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
/// Id salt of the `TableBuilder`; the column resize handles derive their ids from it.
const TABLE_SALT: &str = "tree_table";
/// The Name column is never narrower than this.
const NAME_MIN_WIDTH: f32 = 160.0;
/// The width the columns after Name start with, and the least each can be given.
const SIZE_COLUMNS: [(f32, f32); COLUMNS.len() - 1] =
    [(90.0, 60.0), (90.0, 60.0), (80.0, 50.0), (70.0, 50.0), (85.0, 60.0), (90.0, 70.0)];
/// Id salt of the scroll area that moves the table sideways.
const SIDEWAYS_SALT: &str = "tree_table_sideways";
/// The accessible name of the table.
const TABLE_NAME: &str = "Folders";
/// A menu asked for with the keyboard waits this many frames for its row to be on screen:
/// a row scrolled into view is drawn in the next frame and has its rectangle in the one after.
const MENU_WAIT_PASSES: u64 = 2;

/// The table's own `Ui`: the tree in the accessibility tree, and the one
/// keyboard focus target for all rows.
fn table_id() -> Id {
    Id::unique("tree_table")
}

/// The accessibility item of `node`'s row; stable while the rows move.
fn row_item_id(node: NodeId) -> Id {
    table_id().with(("row", node))
}

/// The clickable, focusable area of the header of column `index`.
fn header_id(index: usize) -> Id {
    table_id().with(("header", index))
}

/// Takes a widget out of what screen readers see; its text is part of a
/// name given elsewhere.
fn hide_from_screen_readers(ctx: &Context, id: Id) {
    ctx.accesskit_node_builder(id, |node| node.set_hidden());
}

/// The texts a row shows in its size columns. The row's accessible name is
/// built from these same strings.
struct RowTexts {
    allocated: String,
    size: String,
    percent: String,
}

impl RowTexts {
    fn new(tree: &Tree, node: NodeId, metric: Metric) -> Self {
        let percent = match tree.parent(node) {
            None => format_percent(1, 1),
            Some(p) => format_percent(tree.metric(node, metric), tree.metric(p, metric)),
        };
        RowTexts { allocated: format_bytes(tree.allocated(node)), size: format_bytes(tree.size(node)), percent }
    }
}

/// The accessible name of a row:
/// `{name}, {allocated} allocated, {size} size, {percent} of parent`, then
/// `folder, expanded` or `folder, collapsed` for a folder (`folder` is
/// `Some(expanded)`), then the marker if there is one.
fn row_name(name: &str, texts: &RowTexts, folder: Option<bool>, marker: Option<&str>) -> String {
    let mut s = format!("{name}, {} allocated, {} size, {} of parent", texts.allocated, texts.size, texts.percent);
    match folder {
        Some(true) => s.push_str(", folder, expanded"),
        Some(false) => s.push_str(", folder, collapsed"),
        None => {}
    }
    if let Some(marker) = marker {
        s.push_str(", ");
        s.push_str(marker);
    }
    s
}

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
pub(super) fn full_path(tree: &Tree, node: NodeId) -> String {
    let mut path = tree.path(node);
    if path.ends_with(':') {
        path.push('\\');
    }
    path
}

/// A cell with `text` right-aligned (or nothing). The row's name already
/// says what the cell shows, so the cell is hidden from screen readers.
/// Returns the cell's rectangle.
fn text_cell(row: &mut TableRow<'_, '_>, ctx: &Context, text: Option<&str>) -> Rect {
    let mut rect = Rect::NOTHING;
    let (_, cell) = row.col(|ui| {
        rect = ui.max_rect();
        if let Some(text) = text {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text));
        }
    });
    hide_from_screen_readers(ctx, cell.id);
    rect
}

/// What the table keeps about its columns from one frame to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Widths {
    /// The width each column had.
    columns: [f32; COLUMNS.len()],
    /// The width the Name column would have with room for it. It is shown at
    /// `NAME_MIN_WIDTH` while this is less, and comes back when the window grows again.
    name: f32,
    /// The width the table had to fill.
    available: f32,
}

impl Widths {
    /// The widths for a table that has `available` to fill now, if they differ
    /// from what the table would keep by itself: the Name column takes what the
    /// window gained or lost, the other columns stay as they are.
    fn for_window(mut self, available: f32) -> Option<Widths> {
        // Set by hand since the last change of the window: that is the width it wants now.
        if (self.columns[0] - self.name.max(NAME_MIN_WIDTH)).abs() > 0.5 {
            self.name = self.columns[0];
        }
        if available == self.available {
            return None;
        }
        self.name += available - self.available;
        self.columns[0] = self.name.max(NAME_MIN_WIDTH);
        self.available = available;
        Some(self)
    }
}

/// A small triangle showing the sort direction.
fn sort_arrow(ui: &mut Ui, desc: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 6.0), Sense::hover());
    let rotation = if desc { 0.0 } else { std::f32::consts::PI };
    let color = ui.visuals().strong_text_color();
    ui.painter().add(egui::Shape::rotated_triangle(rect, rotation, color));
}

/// One header cell; returns true when it was clicked, or activated with
/// Space or Enter while it has the keyboard focus, and the width of its
/// column. For screen readers the cell is a button `Sort by {title}` that
/// tells the sort direction.
fn header_cell(
    header: &mut TableRow<'_, '_>,
    col: Col,
    index: usize,
    title: &str,
    sorted: Option<bool>,
) -> (bool, f32) {
    let mut clicked = false;
    let mut width = 0.0;
    header.col(|ui| {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        width = rect.width();
        // Below the title, so the title does not take its clicks.
        let response = ui.interact(rect, header_id(index), Sense::click());
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, format!("Sort by {title}")));
        if let Some(desc) = sorted {
            let direction =
                if desc { accesskit::SortDirection::Descending } else { accesskit::SortDirection::Ascending };
            ctx.accesskit_node_builder(response.id, |node| node.set_sort_direction(direction));
        }
        if response.has_focus() {
            ui.painter().rect_stroke(rect, 2.0, ui.visuals().selection.stroke, StrokeKind::Inside);
        }
        let label = if col == Col::Name {
            let label = ui.strong(title);
            if let Some(desc) = sorted {
                sort_arrow(ui, desc);
            }
            label
        } else {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(desc) = sorted {
                    sort_arrow(ui, desc);
                }
                ui.strong(title)
            })
            .inner
        };
        hide_from_screen_readers(&ctx, label.id);
        clicked = response.clicked();
    });
    (clicked, width)
}

/// What the user did with the rows this frame; applied after drawing.
#[derive(Default)]
struct RowInput {
    /// Row index to select.
    select: Option<usize>,
    toggle: Option<NodeId>,
    /// The selected row's item and rectangle, if it was drawn.
    selected_item: Option<(Id, Rect)>,
}

/// The table's keys pressed this frame, in order.
#[derive(Default)]
struct TableKeys {
    nav: Vec<NavKey>,
    /// Enter presses: each toggles the selected folder.
    toggle: usize,
    /// The Menu key or Shift+F10.
    menu: bool,
    /// Ctrl+C (egui-winit turns it into `Event::Copy`), Ctrl+Insert or the Copy key.
    copy: bool,
}

/// The keys meant for the table. They act only while the table has the
/// keyboard focus or nothing has it, and no menu or list is open: a focused
/// button, header, drive list or text field keeps its keys. `last_focus` is
/// the focus at the end of the last frame, so a key that made a text field
/// give up the focus earlier in this frame does not act a second time here.
fn table_keys(ctx: &Context, table: Id, last_focus: Option<Id>) -> TableKeys {
    let free = |focus: Option<Id>| focus.is_none_or(|f| f == table);
    if !free(ctx.memory(|m| m.focused())) || !free(last_focus) || Popup::is_any_open(ctx) {
        return TableKeys::default();
    }
    ctx.input(|i| {
        let mut keys = TableKeys::default();
        for event in &i.events {
            match event {
                Event::Key { key, pressed: true, modifiers, .. } => match key {
                    Key::ArrowUp => keys.nav.push(NavKey::Up),
                    Key::ArrowDown => keys.nav.push(NavKey::Down),
                    Key::ArrowLeft => keys.nav.push(NavKey::Left),
                    Key::ArrowRight => keys.nav.push(NavKey::Right),
                    Key::Enter if !modifiers.any() => keys.toggle += 1,
                    Key::Menu => keys.menu = true,
                    Key::F10 if modifiers.shift_only() => keys.menu = true,
                    _ => {}
                },
                Event::Copy => keys.copy = true,
                _ => {}
            }
        }
        keys
    })
}

/// The indentation of a row at `depth` in a Name column `width` wide:
/// `INDENT` per level, but at most `MAX_INDENT_SHARE` of the column.
fn indent_width(depth: u32, width: f32) -> f32 {
    (depth as f32 * INDENT).min(width.max(0.0) * MAX_INDENT_SHARE)
}

/// The name cell: indentation, arrow, share bar, name and marker.
/// Returns true when the arrow was clicked; the second click of a
/// double-click does not count, so a double-click toggles once.
/// Also returns the row's item for screen readers, which spans `row_x`; the
/// name and marker texts are hidden from them, the item's name holds both.
fn name_cell(
    ui: &mut Ui,
    tree: &Tree,
    view: &TreeView,
    node: NodeId,
    depth: u32,
    row_height: f32,
    row_x: Rangef,
) -> (bool, Response) {
    let cell = ui.max_rect();
    // Senses only hover, so it takes no clicks from the cell or the arrow.
    let item = ui.interact(Rect::from_x_y_ranges(row_x, cell.y_range()), row_item_id(node), Sense::hover());
    ui.add_space(indent_width(depth, cell.width()));
    let mut arrow_clicked = false;
    if tree.is_dir(node) && !tree.children(node).is_empty() {
        // Not a Tab stop: the keys reach it through the row (Left, Right, Enter).
        let (_, response) = ui.allocate_exact_size(vec2(ARROW_WIDTH, row_height), Sense::CLICK);
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
        let bar =
            Rect::from_min_size(egui::pos2(start, cell.center().y - row_height * 0.35), vec2(width, row_height * 0.7));
        ui.painter().rect_filled(bar, 2.0, BAR_COLOR);
    }
    // The full path replaces egui's own tooltip for a cut-off name; it is
    // built only while the name is hovered.
    let label = ui.add(egui::Label::new(tree.name(node)).show_tooltip_when_elided(false)).on_hover_ui(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(full_path(tree, node));
    });
    hide_from_screen_readers(ui.ctx(), label.id);
    if let Some(marker) = marker_text(tree.flags(node)) {
        let marker = ui.weak(marker);
        hide_from_screen_readers(ui.ctx(), marker.id);
    }
    (arrow_clicked, item)
}

/// The tree table. Draws only the visible rows; `scroll_to` asks for a row
/// index to be scrolled into view.
///
/// Keyboard: the table is one Tab stop. While it or nothing has the focus,
/// the arrow keys move the selection (see [`TreeView::key_nav`]), Enter
/// expands or collapses the selected folder, the Menu key or Shift+F10 opens
/// the selected row's context menu, and Ctrl+C copies its full path. The
/// column headers are Tab stops of their own and sort with Space or Enter.
pub fn show(ui: &mut Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) {
    let builder = UiBuilder::new().scope_id(table_id()).sense(Sense::FOCUSABLE).accessibility_role(Role::Tree);
    // Sideways when the columns are wider than the window; the table scrolls its rows itself.
    let sideways = ScrollArea::horizontal().id_salt(SIDEWAYS_SALT).auto_shrink(false);
    let shown = sideways.show(ui, |ui| ui.scope_builder(builder, |ui| table(ui, tree, view, scroll_to))).inner;
    let response = shown.response;
    response.widget_info(|| WidgetInfo::labeled(Role::Tree, true, TABLE_NAME));
    // After the table's `Ui` has ended: it gives up the focus when the click
    // that selected a row did not land on the `Ui` itself.
    if shown.inner {
        response.request_focus();
    }
    let focus = ui.ctx().memory(|m| m.focused());
    ui.ctx().data_mut(|d| d.insert_temp(last_focus_id(), focus));
}

/// Where the table keeps the keyboard focus it saw at the end of a frame.
fn last_focus_id() -> Id {
    table_id().with("focus_at_frame_end")
}

/// The table inside its own `Ui`. Returns true when the table should take
/// the keyboard focus: a row was clicked, or a menu opened with the keyboard
/// has closed.
fn table(ui: &mut Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) -> bool {
    let ctx = ui.ctx().clone();
    let table = ui.unique_id();
    let mut scroll_to = scroll_to;
    let mut take_focus = false;

    // A menu opened with the keyboard gives the focus back to the table once it
    // has closed, unless the focus has moved on. Kept: the ids of its entries,
    // which may still hold the focus in the first frame after it closed.
    let return_id = table.with("menu_returns_focus");
    if !Popup::is_any_open(&ctx)
        && let Some(entries) = ctx.data_mut(|d| d.remove_temp::<Vec<Id>>(return_id))
    {
        take_focus = ctx.memory(|m| m.focused()).is_none_or(|f| entries.contains(&f));
    }

    let last_focus = ctx.data(|d| d.get_temp::<Option<Id>>(last_focus_id())).flatten();
    let keys = table_keys(&ctx, table, last_focus);
    if !keys.nav.is_empty() && ctx.memory(|m| m.has_focus(table)) {
        // The arrows move the selection, not egui's focus to a neighbouring widget.
        ctx.memory_mut(|m| m.move_focus(FocusDirection::None));
    }
    for &nav in &keys.nav {
        scroll_to = Some(view.key_nav(tree, nav));
    }
    for _ in 0..keys.toggle {
        view.toggle(tree, view.selected);
    }
    if keys.copy {
        ctx.copy_text(full_path(tree, view.selected));
    }
    // The menu opens when the selected row is on screen: this frame, or within
    // two frames after scrolling to it. A row not on screen by then drops the
    // request, so the menu cannot pop open much later.
    let menu_id = table.with("menu_for_row");
    let pass = ctx.cumulative_pass_nr();
    if keys.menu {
        ctx.data_mut(|d| d.insert_temp(menu_id, (view.selected, pass)));
        scroll_to = Some(view.selected_row());
    }
    let menu_for = ctx
        .data(|d| d.get_temp::<(NodeId, u64)>(menu_id))
        .filter(|&(n, asked)| n == view.selected && pass <= asked.saturating_add(MENU_WAIT_PASSES))
        .map(|(n, _)| n);
    if menu_for.is_none() {
        ctx.data_mut(|d| d.remove_temp::<(NodeId, u64)>(menu_id));
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
    let focused = ctx.memory(|m| m.has_focus(table));

    // The table keeps its column widths by itself, whatever the window does. When the
    // window's width has changed, it is started anew with the Name column following it.
    let widths_id = table.with("column_widths");
    let available = ui.available_width();
    let kept = ctx.data(|d| d.get_temp::<Widths>(widths_id));
    let anew = kept.and_then(|w| w.for_window(available));
    let mut seen = [0.0; COLUMNS.len()];

    // A list too long for egui to place exactly is scrolled by the table
    // itself (see `far_scroll`); egui then gets the offset to show.
    let state_id = ui.scope_id().with(IdSalt::new(TABLE_SALT));
    let scroll_id = ui.scope_id().with(IdSalt::new(state_id.with("__scroll_area")));
    let far_id = table.with("far_scroll");
    let kept_far = ctx.data(|d| d.get_temp::<(FarScroll, f32)>(far_id));
    let total_rows = view.rows().len();
    let row_gap = ui.spacing().item_spacing.y;
    let mut view_height = kept_far.map_or_else(|| ui.available_height() - row_height - row_gap, |(_, height)| height);
    let mut offset = None;
    let far = (far_scroll::virtual_rows(total_rows) != total_rows).then(|| {
        let offset_now = scroll_area::State::load(&ctx, scroll_id).map_or(0.0, |s| s.clamped_offset().y);
        // The scroll bar's own widget, as egui names it.
        let bar_held = ctx.read_response(scroll_id.with(1usize)).is_some_and(|bar| bar.is_pointer_button_down_on());
        let geometry = Geometry {
            total: total_rows,
            step: f64::from(row_height + row_gap),
            spacing: f64::from(row_gap),
            view: f64::from(view_height.max(0.0)),
        };
        let placement =
            far_scroll::place(kept_far.map(|(far, _)| far), offset_now, bar_held, scroll_to.take(), &geometry);
        if placement.scroll.offset != offset_now {
            offset = Some(placement.scroll.offset);
        }
        placement
    });
    if far.is_none()
        && let Some((left, _)) = kept_far
    {
        // Short enough for egui again: it goes on where the view was.
        offset = Some(left.top as f32);
    }

    {
        // Labels must not swallow the clicks that select rows.
        ui.style_mut().interaction.selectable_labels = false;
        let mut builder = TableBuilder::new(ui)
            .id_salt(TABLE_SALT)
            .striped(true)
            .resizable(true)
            // Clicks only: the cells are no Tab stops, the table is one.
            .sense(Sense::CLICK)
            .cell_layout(Layout::left_to_right(Align::Center));
        // At first the Name column fills what the others leave.
        let name = anew.map_or_else(Column::remainder, |w| Column::initial(w.columns[0]));
        builder = builder.column(name.at_least(NAME_MIN_WIDTH).clip(true));
        for (i, (initial, least)) in SIZE_COLUMNS.into_iter().enumerate() {
            let width = anew.map_or(initial, |w| w.columns[i + 1]);
            builder = builder.column(Column::initial(width).at_least(least));
        }
        if anew.is_some() {
            builder.reset();
        }
        if let Some(row) = scroll_to {
            builder = builder.scroll_to_row(row, None);
        }
        if let Some(offset) = offset {
            builder = builder.vertical_scroll_offset(offset);
        }
        if keys.menu {
            // Jump to the row, so it is drawn by the next frame and its menu can open.
            builder = builder.animate_scrolling(false);
        }
        builder
            .header(row_height, |mut header| {
                for (index, (col, title)) in COLUMNS.into_iter().enumerate() {
                    let sorted = (col == shown).then_some(desc);
                    let (clicked, width) = header_cell(&mut header, col, index, title, sorted);
                    seen[index] = width;
                    if clicked {
                        header_click = Some(col);
                    }
                }
            })
            .body(|mut body| {
                let painter = body.ui_mut().painter().clone();
                let focus_stroke = body.ui_mut().visuals().selection.stroke;
                let row_x = body.max_rect().x_range();
                let rows = view.rows();
                view_height = body.ui_mut().clip_rect().height();
                body.rows(row_height, far_scroll::virtual_rows(rows.len()), |mut row| {
                    // In a long list egui's rows stand for the rows at the scroll position.
                    let index = match &far {
                        Some(placement) => placement.real_row(row.index(), rows.len()),
                        None => Some(row.index()),
                    };
                    let Some(&r) = index.and_then(|index| rows.get(index)) else { return };
                    let index = index.unwrap_or_default();
                    let node = r.node;
                    let is_dir = tree.is_dir(node);
                    let selected = node == view.selected;
                    row.set_selected(selected);
                    let texts = RowTexts::new(tree, node, metric);
                    let mut item = None;
                    // From the first cell's left edge to the last cell's right edge, where the
                    // highlight ends. The row's response is no measure: a cell that fills its
                    // width reports a rectangle reaching past its column.
                    let mut cells = Rect::NOTHING;
                    row.col(|ui| {
                        cells = ui.max_rect();
                        let (arrow_clicked, response) = name_cell(ui, tree, view, node, r.depth, row_height, row_x);
                        if arrow_clicked {
                            input.toggle = Some(node);
                        }
                        item = Some(response);
                    });
                    let files = is_dir.then(|| format_count(u64::from(tree.file_count(node))));
                    let folders = is_dir.then(|| format_count(u64::from(tree.dir_count(node))));
                    text_cell(&mut row, &ctx, Some(&texts.allocated));
                    text_cell(&mut row, &ctx, Some(&texts.size));
                    text_cell(&mut row, &ctx, files.as_deref());
                    text_cell(&mut row, &ctx, folders.as_deref());
                    text_cell(&mut row, &ctx, Some(&texts.percent));
                    cells |= text_cell(&mut row, &ctx, Some(&format_date(tree.mtime(node))));

                    let response = row.response();
                    if let Some(item) = item {
                        // The name is built only while a screen reader (or a test) reads the tree.
                        let folder = is_dir.then(|| view.is_expanded(node));
                        item.widget_info(|| {
                            let name = row_name(tree.name(node), &texts, folder, marker_text(tree.flags(node)));
                            WidgetInfo::labeled(Role::TreeItem, true, name)
                        });
                        let expandable = is_dir && !tree.children(node).is_empty();
                        ctx.accesskit_node_builder(item.id, |n| {
                            n.set_selected(selected);
                            // Zero-based in AccessKit: UI Automation reports the root row as level 1.
                            n.set_level(r.depth as usize);
                            if expandable {
                                n.set_expanded(view.is_expanded(node));
                            }
                        });
                        if selected {
                            input.selected_item = Some((item.id, cells));
                        }
                    }
                    if response.clicked() || response.secondary_clicked() {
                        input.select = Some(index);
                    }
                    if response.double_clicked() && is_dir {
                        input.toggle = Some(node);
                    }
                    let popup_id = Popup::default_response_id(&response);
                    if rows_changed {
                        Popup::close_id(&ctx, popup_id);
                    }
                    // Only on screen: a row drawn just outside the view has no place for the menu.
                    let open_by_key = menu_for == Some(node) && response.interact_rect.is_positive();
                    if open_by_key {
                        Popup::open_id(&ctx, popup_id);
                        ctx.data_mut(|d| {
                            d.remove_temp::<(NodeId, u64)>(menu_id);
                            d.insert_temp(return_id, Vec::<Id>::new());
                        });
                    }
                    // Opened with the keyboard there is no pointer position: the menu goes below the row.
                    let mut menu = Popup::context_menu(&response).anchor_widget(table);
                    if !response.secondary_clicked() && Popup::position_of_id(&ctx, popup_id).is_none() {
                        menu = menu.anchor(&response);
                    }
                    menu.show(|ui| {
                        let explorer = ui.button("Show in Explorer");
                        if open_by_key {
                            explorer.request_focus();
                        }
                        if explorer.clicked() {
                            win::reveal_in_explorer(&full_path(tree, node));
                            ui.close();
                        }
                        let copy = ui.button("Copy path");
                        if copy.clicked() {
                            ui.ctx().copy_text(full_path(tree, node));
                            ui.close();
                        }
                        let entries = vec![explorer.id, copy.id];
                        ui.ctx().data_mut(|d| {
                            if d.get_temp::<Vec<Id>>(return_id).is_some() {
                                d.insert_temp(return_id, entries);
                            }
                        });
                    });
                });
                if focused && let Some((_, rect)) = input.selected_item {
                    painter.rect_stroke(rect, 0.0, focus_stroke, StrokeKind::Inside);
                }
            });
        // The column resize handles would be Tab stops and screen reader
        // regions without a name or a visible focus; the mouse keeps them.
        for i in 0..COLUMNS.len() {
            let handle = state_id.with("resize_column").with(i);
            ctx.memory_mut(|m| m.ignore_focus(handle));
            hide_from_screen_readers(&ctx, handle);
        }
    }
    let name = anew.or(kept).map_or(seen[0], |w| w.name);
    ctx.data_mut(|d| d.insert_temp(widths_id, Widths { columns: seen, name, available }));
    ctx.data_mut(|d| {
        if let Some(placement) = far {
            d.insert_temp(far_id, (placement.scroll, view_height));
        } else {
            d.remove::<(FarScroll, f32)>(far_id);
        }
    });

    // While the table has the focus, screen readers are on the selected row.
    if let Some((item, _)) = input.selected_item {
        ctx.accesskit_node_builder(table, |n| n.set_active_descendant(item.accesskit_id()));
    }

    // The row index belongs to the rows drawn this frame, so select before re-sorting.
    if let Some(index) = input.select {
        view.select_row(index);
        take_focus = true;
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
    take_focus
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
    fn row_name_lists_the_shown_values() {
        let texts = RowTexts { allocated: "8.0 KB".into(), size: "1,015 Bytes".into(), percent: "66.7 %".into() };
        assert_eq!(row_name("a", &texts, None, None), "a, 8.0 KB allocated, 1,015 Bytes size, 66.7 % of parent");
        assert_eq!(
            row_name("a", &texts, Some(true), None),
            "a, 8.0 KB allocated, 1,015 Bytes size, 66.7 % of parent, folder, expanded"
        );
        assert_eq!(
            row_name("a", &texts, Some(false), Some("link")),
            "a, 8.0 KB allocated, 1,015 Bytes size, 66.7 % of parent, folder, collapsed, link"
        );
    }

    #[test]
    fn row_texts_are_the_formatted_cell_values() {
        let t = sample();
        let texts = RowTexts::new(&t, 1, Metric::Allocated);
        assert_eq!((texts.allocated.as_str(), texts.size.as_str()), ("8.0 KB", "1,015 Bytes"));
        assert_eq!(texts.percent, "66.7 %");
        assert_eq!(RowTexts::new(&t, 0, Metric::Size).percent, "100.0 %");
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

    #[test]
    fn name_width_takes_what_the_window_gains_or_loses() {
        let columns = [331.0, 90.0, 90.0, 80.0, 70.0, 85.0, 90.0];
        let kept = Widths { columns, name: 331.0, available: 884.0 };
        assert_eq!(kept.for_window(884.0), None);
        let wider = kept.for_window(1184.0).unwrap();
        assert_eq!((wider.columns[0], wider.name, wider.available), (631.0, 631.0, 1184.0));
        assert_eq!(wider.columns[1..], columns[1..]);
        // Below the minimum the wanted width is kept, so the column comes back as it was.
        let narrow = kept.for_window(584.0).unwrap();
        assert_eq!((narrow.columns[0], narrow.name), (NAME_MIN_WIDTH, 31.0));
        assert_eq!(narrow.for_window(884.0).unwrap().columns[0], 331.0);
        // Dragged to 200 while it was held at the minimum: 200 is what it wants now.
        let dragged = Widths { columns: [200.0, 90.0, 90.0, 80.0, 70.0, 85.0, 90.0], ..narrow };
        assert_eq!(dragged.for_window(684.0).unwrap().columns[0], 300.0);
    }

    // Headless tests of `show`: egui_kittest drives a plain `egui::Context`
    // with synthetic input. No window, no renderer, simulated time only.
    mod ui {
        use super::*;
        use eframe::egui::{Event, Modifiers, PointerButton, Pos2};
        use egui_kittest::Harness;
        use egui_kittest::kittest::{NodeT, Queryable};

        struct State {
            tree: Tree,
            view: TreeView,
            /// Every text the table put on the clipboard.
            copied: Vec<String>,
            /// Set: a text field above the table, for the focus rules.
            text: Option<String>,
            /// Set: the table is not drawn this frame.
            hidden: bool,
        }

        fn harness() -> Harness<'static, State> {
            harness_with(None)
        }

        fn harness_with(text: Option<String>) -> Harness<'static, State> {
            let tree = sample();
            let view = TreeView::new(&tree, Metric::Allocated);
            Harness::builder()
                .with_size(vec2(900.0, 400.0))
                // Short frames, so two clicks two frames apart are a double-click.
                .with_step_dt(0.05)
                .build_ui_state(
                    |ui, s: &mut State| {
                        if let Some(text) = &mut s.text {
                            ui.add(egui::TextEdit::singleline(text).id(text_field_id()).hint_text("Filter"));
                        }
                        if !s.hidden {
                            show(ui, &s.tree, &mut s.view, None);
                        }
                        ui.ctx().output(|o| {
                            for command in &o.commands {
                                if let egui::OutputCommand::CopyText(t) = command {
                                    s.copied.push(t.clone());
                                }
                            }
                        });
                    },
                    State { tree, view, copied: Vec::new(), text, hidden: false },
                )
        }

        fn text_field_id() -> Id {
            Id::unique("test_text_field")
        }

        fn focused(h: &Harness<'_, State>) -> Option<Id> {
            h.ctx.memory(|m| m.focused())
        }

        fn press(h: &mut Harness<'_, State>, key: Key) {
            h.key_press(key);
            h.run();
        }

        /// Hidden from screen readers: the node or one of its ancestors is hidden.
        fn hidden(node: &egui_kittest::kittest::AccessKitNode<'_>) -> bool {
            std::iter::successors(Some(*node), |n| n.parent()).any(|n| n.is_hidden())
        }

        const ROOT: &str = "C:, 12.0 KB allocated, 1,016 Bytes size, 100.0 % of parent, folder, expanded";
        const A: &str = "a, 8.0 KB allocated, 1,015 Bytes size, 66.7 % of parent, folder, collapsed";
        const TOP: &str = "top.txt, 4.0 KB allocated, 1 Bytes size, 33.3 % of parent";

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
                    h.event(Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    });
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

        #[test]
        fn each_row_is_one_tree_item_named_by_what_it_shows() {
            let h = harness();
            let root = h.get_by_role_and_label(Role::TreeItem, ROOT);
            assert_eq!(root.accesskit_node().is_selected(), Some(true));
            assert_eq!(root.accesskit_node().data().is_expanded(), Some(true));
            // AccessKit counts levels from 0; UI Automation adds 1, so the root row is level 1.
            assert_eq!(root.accesskit_node().level(), Some(0));
            let a = h.get_by_role_and_label(Role::TreeItem, A);
            assert_eq!(a.accesskit_node().is_selected(), Some(false));
            assert_eq!(a.accesskit_node().data().is_expanded(), Some(false));
            assert_eq!(a.accesskit_node().level(), Some(1));
            // The tree itself is named.
            assert!(h.query_by_role_and_label(Role::Tree, "Folders").is_some());
            // A file has no expanded state.
            let top = h.get_by_role_and_label(Role::TreeItem, TOP);
            assert_eq!(top.accesskit_node().data().is_expanded(), None);
            // The cells' own texts are still drawn, but hidden from screen readers.
            for text in ["a", "8.0 KB", "1,015 Bytes", "66.7 %", "Size"] {
                let node = h.get_by_label(text);
                assert!(hidden(&node.accesskit_node()), "{text} is not hidden");
            }
            assert!(!hidden(&a.accesskit_node()));
            // Four rows, four items: C:, a, empty, top.txt.
            assert_eq!(h.query_all_by_role(Role::TreeItem).count(), 4);
        }

        #[test]
        fn marker_ends_the_row_name() {
            let mut b = TreeBuilder::with_capacity(2, 16);
            let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
            b.push("locked", root, 0, 0, 0, F_DIR | F_UNREADABLE);
            let tree = b.finish(root).0;
            let mut h = harness();
            let s = h.state_mut();
            s.view = TreeView::new(&tree, Metric::Allocated);
            s.tree = tree;
            h.run();
            let name = "locked, 0 Bytes allocated, 0 Bytes size, 0.0 % of parent, folder, collapsed, unreadable";
            assert!(h.query_by_role_and_label(Role::TreeItem, name).is_some());
        }

        #[test]
        fn headers_are_buttons_named_sort_by_column() {
            let mut h = harness();
            for (_, title) in COLUMNS {
                assert!(h.query_by_role_and_label(Role::Button, &format!("Sort by {title}")).is_some(), "{title}");
            }
            let allocated = h.get_by_role_and_label(Role::Button, "Sort by Allocated");
            assert_eq!(allocated.accesskit_node().data().sort_direction(), Some(accesskit::SortDirection::Descending));
            h.get_by_role_and_label(Role::Button, "Sort by Size").click();
            h.run();
            assert!(h.state().view.sort() == (SortKey::Size, true));
        }

        #[test]
        fn tab_reaches_the_headers_and_enter_or_space_sorts() {
            let mut h = harness();
            press(&mut h, Key::Tab);
            assert_eq!(focused(&h), Some(table_id()));
            // Name, Allocated, Size.
            for _ in 0..3 {
                press(&mut h, Key::Tab);
            }
            assert_eq!(focused(&h), Some(header_id(2)));
            assert!(h.get_by_role_and_label(Role::Button, "Sort by Size").accesskit_node().is_focused());
            press(&mut h, Key::Enter);
            assert!(h.state().view.sort() == (SortKey::Size, true));
            press(&mut h, Key::Space);
            assert!(h.state().view.sort() == (SortKey::Size, false));
            // The keys went to the header only: the root is still expanded and selected.
            assert!(h.state().view.is_expanded(0));
            assert_eq!(h.state().view.selected, 0);
            // The arrows move egui's focus between the headers, not the selection.
            press(&mut h, Key::ArrowRight);
            assert_eq!(focused(&h), Some(header_id(3)));
            assert_eq!(h.state().view.selected, 0);
        }

        #[test]
        fn tab_after_the_last_header_skips_the_column_resize_handles() {
            let mut h = harness();
            // The handles exist, under the ids the table derives for them.
            let state_id = table_id().with(IdSalt::new(TABLE_SALT));
            let handle = |i: usize| state_id.with("resize_column").with(i);
            assert!(h.ctx.read_response(handle(1)).is_some());
            // Hidden from screen readers, too: an unnamed region per column border otherwise.
            for i in 0..COLUMNS.len() {
                let node = h.get_by(|n| n.locate().0 == handle(i).accesskit_id());
                assert!(node.accesskit_node().is_hidden(), "handle {i}");
            }
            // The table, then the seven headers.
            for _ in 0..8 {
                press(&mut h, Key::Tab);
            }
            assert_eq!(focused(&h), Some(header_id(6)));
            press(&mut h, Key::Tab);
            let focus = focused(&h);
            assert!((0..COLUMNS.len()).all(|i| focus != Some(handle(i))), "{focus:?}");
            press(&mut h, Key::Tab);
            assert_eq!(focused(&h), Some(table_id()));
        }

        #[test]
        fn with_the_focus_on_the_table_the_arrows_move_the_selection() {
            let mut h = harness();
            press(&mut h, Key::Tab);
            assert_eq!(focused(&h), Some(table_id()));
            // At the top row: the focus does not jump to the headers above.
            press(&mut h, Key::ArrowUp);
            assert_eq!(focused(&h), Some(table_id()));
            press(&mut h, Key::ArrowDown);
            assert_eq!(h.state().view.selected, 1);
            assert_eq!(focused(&h), Some(table_id()));
            // Screen readers are on the selected row.
            assert!(h.get_by_role_and_label(Role::TreeItem, A).accesskit_node().is_focused());
            press(&mut h, Key::ArrowRight);
            assert!(h.state().view.is_expanded(1));
            assert_eq!(focused(&h), Some(table_id()));
        }

        #[test]
        fn clicking_a_row_gives_the_table_the_focus() {
            let mut h = harness();
            let pos = label_center(&h, "a");
            click(&mut h, pos, PointerButton::Primary);
            assert_eq!(focused(&h), Some(table_id()));
            press(&mut h, Key::ArrowUp);
            assert_eq!(h.state().view.selected, 0);
            assert_eq!(focused(&h), Some(table_id()));
        }

        #[test]
        fn enter_toggles_the_selected_folder() {
            let mut h = harness();
            press(&mut h, Key::ArrowDown);
            assert_eq!(h.state().view.selected, 1);
            press(&mut h, Key::Enter);
            assert!(h.state().view.is_expanded(1));
            assert!(h.query_by_label("big.bin").is_some());
            let expanded = A.replace("collapsed", "expanded");
            assert!(h.query_by_role_and_label(Role::TreeItem, &expanded).is_some());
            press(&mut h, Key::Enter);
            assert!(!h.state().view.is_expanded(1));
            // A file: nothing to toggle.
            press(&mut h, Key::ArrowDown);
            press(&mut h, Key::ArrowDown);
            assert_eq!(h.state().view.selected, 6);
            let rows = h.state().view.rows().len();
            press(&mut h, Key::Enter);
            assert_eq!(h.state().view.rows().len(), rows);
        }

        #[test]
        fn ctrl_c_copies_the_selected_path() {
            let mut h = harness();
            press(&mut h, Key::ArrowDown);
            // What egui-winit sends for Ctrl+C.
            h.event(Event::Copy);
            h.run();
            assert_eq!(h.state().copied, ["C:\\a"]);
            press(&mut h, Key::ArrowUp);
            h.event(Event::Copy);
            h.run();
            assert_eq!(h.state().copied, ["C:\\a", "C:\\"]);
        }

        fn menu_entry(h: &Harness<'_, State>, label: &str) -> bool {
            h.get_by_label(label).accesskit_node().is_focused()
        }

        #[test]
        fn menu_key_opens_the_menu_for_the_selected_row_and_the_keys_work_it() {
            let mut h = harness();
            press(&mut h, Key::Tab);
            press(&mut h, Key::ArrowDown);
            assert!(h.query_by_label("Copy path").is_none());
            press(&mut h, Key::Menu);
            assert!(h.query_by_label("Copy path").is_some());
            // The first entry has the focus; the arrows move it, Enter runs the entry.
            assert!(menu_entry(&h, "Show in Explorer"));
            press(&mut h, Key::ArrowDown);
            assert!(menu_entry(&h, "Copy path"));
            assert_eq!(h.state().view.selected, 1, "the arrows went to the menu, not the table");
            press(&mut h, Key::Enter);
            assert_eq!(h.state().copied, ["C:\\a"]);
            assert!(h.query_by_label("Copy path").is_none());
            // Enter ran the entry only; the folder did not toggle.
            assert!(!h.state().view.is_expanded(1));
            // The focus is back on the table.
            assert_eq!(focused(&h), Some(table_id()));
        }

        #[test]
        fn shift_f10_opens_the_menu_and_escape_closes_it() {
            let mut h = harness();
            press(&mut h, Key::ArrowDown);
            h.key_press_modifiers(Modifiers::SHIFT, Key::F10);
            h.run();
            assert!(h.query_by_label("Copy path").is_some());
            press(&mut h, Key::Escape);
            assert!(h.query_by_label("Copy path").is_none());
            assert_eq!(focused(&h), Some(table_id()));
            // F10 without Shift does nothing.
            press(&mut h, Key::F10);
            assert!(h.query_by_label("Copy path").is_none());
        }

        /// A root with 200 files (the table's body is at least 200 points high); the last file is selected but scrolled away.
        fn long_list() -> Harness<'static, State> {
            let mut b = TreeBuilder::with_capacity(201, 2048);
            let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
            for i in 0..200u64 {
                b.push(&format!("f{i:03}"), root, 1000 - i, 0, 0, 0);
            }
            let tree = b.finish(root).0;
            let mut h = harness();
            h.set_size(vec2(900.0, 300.0));
            let s = h.state_mut();
            s.view = TreeView::new(&tree, Metric::Size);
            s.view.select_row(200);
            s.tree = tree;
            h.run();
            assert!(h.query_by_label("f199").is_none(), "the selected row is not drawn");
            h
        }

        #[test]
        fn menu_key_on_a_row_scrolled_away_scrolls_to_it_and_opens() {
            let mut h = long_list();
            press(&mut h, Key::Menu);
            assert!(h.query_by_label("f199").is_some());
            assert!(h.query_by_label("Copy path").is_some());
        }

        #[test]
        fn menu_request_is_dropped_when_its_row_is_not_drawn_within_two_frames() {
            let mut h = long_list();
            h.key_down(Key::Menu);
            h.step();
            // The table is not drawn for a while, so the row cannot be either.
            h.state_mut().hidden = true;
            h.run_steps(3);
            h.state_mut().hidden = false;
            h.key_up(Key::Menu);
            h.run();
            assert!(h.query_by_label("f199").is_some(), "scrolled to the row");
            assert!(h.query_by_label("Copy path").is_none(), "but the late menu stays closed");
        }

        /// Every rectangle painted in the last frame.
        fn painted_rects(h: &Harness<'_, State>) -> Vec<egui::epaint::RectShape> {
            fn collect(shape: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
                match shape {
                    egui::Shape::Rect(rect) => out.push(rect.clone()),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                    _ => {}
                }
            }
            let mut out = Vec::new();
            h.output().shapes.iter().for_each(|s| collect(&s.shape, &mut out));
            out
        }

        /// The horizontal extent of the selected row's highlight and of its focus outline.
        fn highlight_and_outline(h: &Harness<'_, State>) -> (Rangef, Rangef) {
            let selection = h.ctx.global_style().visuals.selection;
            let rects = painted_rects(h);
            let span = |what: &str, matches: &dyn Fn(&egui::epaint::RectShape) -> bool| {
                let found: Vec<_> = rects.iter().filter(|r| matches(r)).collect();
                assert!(!found.is_empty(), "no {what} painted");
                let left = found.iter().map(|r| r.rect.left()).fold(f32::INFINITY, f32::min);
                let right = found.iter().map(|r| r.rect.right()).fold(f32::NEG_INFINITY, f32::max);
                Rangef::new(left, right)
            };
            let highlight = span("highlight", &|r| r.fill == selection.bg_fill);
            let outline = span("outline", &|r| r.fill == Color32::TRANSPARENT && r.stroke == selection.stroke);
            (highlight, outline)
        }

        /// Drags the right edge of the Name column by `dx`.
        fn drag_name_column(h: &mut Harness<'_, State>, dx: f32) {
            let header = h.get_by_label("Sort by Name").rect();
            let gap = h.ctx.global_style().spacing.item_spacing.x;
            let from = egui::pos2(header.right() + gap * 0.5, header.center().y);
            let to = from + vec2(dx, 0.0);
            h.hover_at(from);
            h.run();
            h.drag_at(from);
            h.run();
            h.hover_at(to);
            h.run();
            h.drop_at(to);
            h.run();
        }

        /// The horizontal extent of the header of the last column.
        fn last_header(h: &Harness<'_, State>) -> Rangef {
            h.get_by_label("Sort by Modified").rect().x_range()
        }

        #[test]
        fn focus_outline_ends_with_the_last_column() {
            let mut h = harness();
            press(&mut h, Key::Tab);
            assert_eq!(focused(&h), Some(table_id()));
            for narrowed in [false, true] {
                if narrowed {
                    // Now the columns end well before the window does.
                    drag_name_column(&mut h, -200.0);
                    assert!(last_header(&h).max < 800.0);
                }
                let (highlight, outline) = highlight_and_outline(&h);
                assert_eq!(outline.max, last_header(&h).max, "narrowed: {narrowed}");
                assert!(highlight.min <= outline.min && outline.max <= highlight.max, "narrowed: {narrowed}");
            }
        }

        #[test]
        fn window_narrower_than_the_columns_scrolls_sideways() {
            let mut h = harness();
            h.set_size(vec2(500.0, 400.0));
            h.run();
            assert!(last_header(&h).max > 500.0, "the last column does not fit");
            // Over a row, but not over a name: that would show its tooltip.
            let over_row = egui::pos2(h.get_by_label("Sort by Allocated").rect().center().x, label_center(&h, "a").y);
            h.hover_at(over_row);
            h.event(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(-2000.0, 0.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Modifiers::NONE,
            });
            // The wheel is smoothed over several frames.
            h.run_steps(20);
            let last = last_header(&h);
            assert!(last.max <= 500.0 && last.min >= 0.0, "the last column is on screen: {last:?}");
            // The rows moved with the headers, and the keys still work on them.
            assert!(label_center(&h, "a").x < 0.0);
            press(&mut h, Key::Tab);
            press(&mut h, Key::ArrowDown);
            assert_eq!(h.state().view.selected, 1);
            assert!(last_header(&h).max <= 500.0, "moving the selection does not scroll back");
        }

        /// The width of the Name column.
        fn name_width(h: &Harness<'_, State>) -> f32 {
            h.get_by_label("Sort by Name").rect().width()
        }

        fn set_width(h: &mut Harness<'_, State>, width: f32) {
            h.set_size(vec2(width, 400.0));
            h.run();
        }

        fn assert_close(actual: f32, expected: f32, what: &str) {
            assert!((actual - expected).abs() < 0.5, "{what}: {actual}, expected {expected}");
        }

        #[test]
        fn name_column_follows_the_window_width() {
            let mut h = harness();
            let (name, end) = (name_width(&h), last_header(&h).max);
            assert_close(end, 892.0, "the columns fill the window");
            // Wider: the Name column takes the new space, nothing is left empty.
            set_width(&mut h, 1200.0);
            assert_close(name_width(&h), name + 300.0, "Name at 1200");
            assert_close(last_header(&h).max, end + 300.0, "last column at 1200");
            set_width(&mut h, 900.0);
            assert_close(name_width(&h), name, "Name back at 900");
            // Too narrow for all columns: Name stops at its minimum, the rest scrolls.
            set_width(&mut h, 600.0);
            assert_close(name_width(&h), NAME_MIN_WIDTH, "Name at 600");
            assert!(last_header(&h).max > 600.0);
            set_width(&mut h, 900.0);
            assert_close(name_width(&h), name, "Name back at 900 again");
        }

        #[test]
        fn name_width_set_by_hand_follows_the_window_too() {
            let mut h = harness();
            let name = name_width(&h);
            drag_name_column(&mut h, -100.0);
            assert_close(name_width(&h), name - 100.0, "Name after the drag");
            set_width(&mut h, 1000.0);
            assert_close(name_width(&h), name, "Name at 1000");
            // The other columns keep the widths they have.
            assert_close(h.get_by_label("Sort by Allocated").rect().width(), 90.0, "Allocated");
            // And it can still be set by hand afterwards.
            drag_name_column(&mut h, -50.0);
            assert_close(name_width(&h), name - 50.0, "Name after the second drag");
        }

        #[test]
        fn rows_scroll_bar_stays_in_view_when_the_columns_are_wider_than_the_window() {
            let mut h = long_list();
            h.set_size(vec2(500.0, 300.0));
            h.run();
            assert!(last_header(&h).max > 500.0, "the last column does not fit");
            assert!(h.query_by_label("f000").is_some());
            // The bar's lane, at the right edge of what is on screen.
            let lane = egui::pos2(500.0 - 12.0, 250.0);
            h.hover_at(lane);
            h.run_steps(10);
            h.drag_at(lane);
            h.run_steps(3);
            h.drop_at(lane);
            h.run_steps(5);
            assert!(h.query_by_label("f000").is_none(), "the rows have moved");
            // The selection has not: the press went to the bar, not to a row.
            assert_eq!(h.state().view.selected_row(), 200);
        }

        #[test]
        fn a_focused_text_field_keeps_its_keys() {
            let mut h = harness_with(Some(String::new()));
            h.ctx.memory_mut(|m| m.request_focus(text_field_id()));
            h.run();
            for key in [Key::ArrowDown, Key::Menu] {
                press(&mut h, key);
            }
            h.event(Event::Copy);
            h.run();
            assert_eq!(h.state().view.selected, 0);
            assert!(h.query_by_label("Copy path").is_none());
            assert!(h.state().copied.iter().all(|t| t != "C:\\"));
            // Enter makes the field give up the focus, but does not reach the table too.
            press(&mut h, Key::Enter);
            assert!(h.state().view.is_expanded(0));
            assert_eq!(focused(&h), None);
            // Now the table has the keys.
            press(&mut h, Key::ArrowDown);
            assert_eq!(h.state().view.selected, 1);
        }
    }
}

/// Headless tests of a list longer than egui scrolls by itself.
#[cfg(test)]
mod long_list {
    use super::*;
    use crate::model::{F_DIR, NO_PARENT, TreeBuilder};
    use eframe::egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, TouchPhase};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    struct State {
        tree: Tree,
        view: TreeView,
        scroll_to: Option<usize>,
    }

    /// The root `C:` with `files` files `f0`, `f1`, ... in this order: row `i + 1` is `f{i}`.
    fn harness(files: u32) -> Harness<'static, State> {
        let mut b = TreeBuilder::with_capacity(files as usize + 1, 16);
        let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
        for i in 0..files {
            let size = u64::from(files - i) * 4096;
            b.push(&format!("f{i}"), root, size, size, 0, 0);
        }
        let tree = b.finish(root).0;
        let view = TreeView::new(&tree, Metric::Allocated);
        let mut h = Harness::builder().with_size(vec2(900.0, 400.0)).with_step_dt(0.05).build_ui_state(
            |ui, s: &mut State| show(ui, &s.tree, &mut s.view, s.scroll_to.take()),
            State { tree, view, scroll_to: None },
        );
        h.run_steps(3);
        h
    }

    /// The rows on screen from top to bottom: name, top and bottom edge.
    fn drawn(h: &Harness<'_, State>) -> Vec<(String, f32, f32)> {
        let mut rows: Vec<(String, f32, f32)> = h
            .query_all_by_role(Role::TreeItem)
            .map(|node| {
                let name = node.accesskit_node().label().unwrap_or_default();
                (name.split(',').next().unwrap_or_default().to_string(), node.rect().min.y, node.rect().max.y)
            })
            .collect();
        rows.sort_by(|a, b| a.1.total_cmp(&b.1));
        rows
    }

    fn names(rows: &[(String, f32, f32)]) -> Vec<&str> {
        rows.iter().map(|r| r.0.as_str()).collect()
    }

    /// The edges of the rows without their names.
    fn edges(rows: &[(String, f32, f32)]) -> Vec<(f32, f32)> {
        rows.iter().map(|r| (r.1, r.2)).collect()
    }

    fn scroll_to(h: &mut Harness<'_, State>, row: usize) {
        h.state_mut().view.select_row(row);
        h.state_mut().scroll_to = Some(row);
        h.run_steps(40);
    }

    fn wheel(h: &mut Harness<'_, State>, at: Pos2, points: f32) {
        h.hover_at(at);
        h.event(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, points),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        });
        // egui spreads a wheel step over some frames.
        h.run_steps(60);
    }

    /// The number in a file's name: `f123` is 123.
    fn file_number(name: &str) -> usize {
        name.trim_start_matches('f').parse().unwrap_or_else(|_| panic!("not a file row: {name}"))
    }

    #[test]
    fn the_end_of_two_million_rows_is_placed_like_the_end_of_a_thousand() {
        let mut short = harness(1_000);
        scroll_to(&mut short, 1_000);
        let want = drawn(&short);
        assert_eq!(names(&want).last(), Some(&"f999"));

        // 46 million points high: there egui's positions are 4 points apart.
        let mut long = harness(2_000_000);
        scroll_to(&mut long, 2_000_000);
        let got = drawn(&long);
        assert_eq!(edges(&got), edges(&want), "{got:?}");
        let last: Vec<String> = (2_000_000 - got.len()..2_000_000).map(|i| format!("f{i}")).collect();
        assert_eq!(names(&got), last);
        // The start is where a short list starts, too.
        scroll_to(&mut short, 0);
        scroll_to(&mut long, 0);
        assert_eq!(drawn(&long), drawn(&short));
    }

    #[test]
    fn the_wheel_moves_a_long_list_row_by_row() {
        let mut h = harness(300_000);
        let start = drawn(&h);
        assert_eq!(names(&start)[..3], ["C:", "f0", "f1"]);
        let at = Pos2::new(300.0, (start[5].1 + start[5].2) / 2.0);
        // Ten rows of 23 points down.
        wheel(&mut h, at, -230.0);
        let moved = drawn(&h);
        assert_eq!(names(&moved)[..3], ["f9", "f10", "f11"]);
        assert_eq!(edges(&moved), edges(&start));
        // Part of a row more: the same rows, that much higher.
        wheel(&mut h, at, -12.0);
        let part = drawn(&h);
        assert_eq!(part[1].0, "f10");
        assert_eq!(part[1].1, start[1].1 - 12.0);
        wheel(&mut h, at, 12.0);
        assert_eq!(drawn(&h), moved);

        // The same far down the list, where egui's row is not the real one.
        scroll_to(&mut h, 200_000);
        let before = drawn(&h);
        let first = file_number(&before[0].0);
        wheel(&mut h, at, -230.0);
        let after = drawn(&h);
        assert_eq!(file_number(&after[0].0), first + 10);
        assert_eq!(edges(&after), edges(&before));
        wheel(&mut h, at, 460.0);
        assert_eq!(file_number(&drawn(&h)[0].0), first - 10);
        // Up from the top and down from the end it stops.
        scroll_to(&mut h, 0);
        wheel(&mut h, at, 500.0);
        assert_eq!(drawn(&h), start);
        scroll_to(&mut h, 300_000);
        let end = drawn(&h);
        wheel(&mut h, at, -500.0);
        assert_eq!(drawn(&h), end);
        assert_eq!(names(&end).last(), Some(&"f299999"));
    }

    /// Top and bottom edge of the part of the table that shows rows; leaves the list at its start.
    fn view_edges(h: &mut Harness<'_, State>) -> (f32, f32) {
        let last = h.state().view.rows().len() - 1;
        scroll_to(h, last);
        let bottom = drawn(h).last().unwrap().2;
        scroll_to(h, 0);
        (drawn(h)[0].1, bottom)
    }

    #[test]
    fn the_scroll_bar_of_a_long_list_spans_the_whole_list() {
        let mut h = harness(300_000);
        let (top, bottom) = view_edges(&mut h);
        // The bar is at the right edge of the rows; a press on its track puts the handle there.
        let bar = |y: f32| Pos2::new(888.0, y);
        let middle = bar((top + bottom) / 2.0);
        h.hover_at(middle);
        h.run_steps(5);
        h.event(Event::PointerButton {
            pos: middle,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(5);
        let at_middle = file_number(&drawn(&h)[0].0);
        assert!((147_000..153_000).contains(&at_middle), "row {at_middle} at the middle of the bar");
        // Held still, the list stays where it is.
        h.run_steps(5);
        assert_eq!(file_number(&drawn(&h)[0].0), at_middle);
        // Dragged to the bottom: the last row, where the end of a list is.
        for y in [bottom - 40.0, bottom, bottom + 60.0] {
            h.event(Event::PointerMoved(bar(y)));
            h.run_steps(3);
        }
        h.event(Event::PointerButton {
            pos: bar(bottom + 60.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(5);
        let end = drawn(&h);
        assert_eq!(names(&end).last(), Some(&"f299999"));
        assert_eq!(end.last().unwrap().2, bottom);
    }

    /// Clicks the fourth row on screen, goes 40 rows down and 60 up with the
    /// arrow keys, and returns the edges of the selected row after either.
    fn walk(h: &mut Harness<'_, State>) -> [(f32, f32); 2] {
        let rows = drawn(h);
        let click = Pos2::new(300.0, (rows[3].1 + rows[3].2) / 2.0);
        h.hover_at(click);
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: click,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.run();
        let clicked = h.state().view.selected_row();
        assert_eq!(format!("f{}", clicked - 1), rows[3].0);
        let mut edges = [(0.0, 0.0); 2];
        for (i, (key, times, row)) in
            [(Key::ArrowDown, 40, clicked + 40), (Key::ArrowUp, 60, clicked - 20)].into_iter().enumerate()
        {
            for _ in 0..times {
                h.key_press(key);
                h.run();
            }
            h.run_steps(40);
            assert_eq!(h.state().view.selected_row(), row);
            let name = format!("f{}", row - 1);
            let rows = drawn(h);
            let selected =
                rows.iter().find(|r| r.0 == name).unwrap_or_else(|| panic!("{name} not on screen: {rows:?}"));
            edges[i] = (selected.1, selected.2);
        }
        edges
    }

    #[test]
    fn the_arrow_keys_keep_the_selected_row_of_a_long_list_in_view() {
        // What a list does that egui scrolls by itself.
        let mut short = harness(5_000);
        let (top, bottom) = view_edges(&mut short);
        scroll_to(&mut short, 2_000);
        let want = walk(&mut short);
        // Down: the row, the gap below it and one more gap end at the lower edge.
        // Up: the row is the first one, a gap below the upper edge.
        assert_eq!(want, [(bottom - 26.0, bottom - 6.0), (top + 3.0, top + 23.0)]);

        let mut long = harness(300_000);
        assert_eq!(view_edges(&mut long), (top, bottom));
        scroll_to(&mut long, 200_000);
        assert_eq!(walk(&mut long), want);
    }

    #[test]
    fn a_list_that_gets_short_again_goes_on_where_it_was() {
        // Just past what egui scrolls by itself.
        let files = far_scroll::NATIVE_ROWS as u32 + 10;
        let mut h = harness(files);
        scroll_to(&mut h, 1_000);
        let at = Pos2::new(300.0, 200.0);
        wheel(&mut h, at, -230.0);
        let long = drawn(&h);
        // Sorted the other way round the list is as long; collapsed it is one row.
        let root = h.state().tree.root();
        let s = h.state_mut();
        s.view.toggle(&s.tree, root);
        h.run_steps(5);
        assert_eq!(names(&drawn(&h)), ["C:"]);
        let s = h.state_mut();
        s.view.toggle(&s.tree, root);
        h.run_steps(5);
        // Open again it starts at the top, as a short list does after it was one row.
        assert_eq!(names(&drawn(&h))[..2], ["C:", "f0"]);
        scroll_to(&mut h, 1_000);
        wheel(&mut h, at, -230.0);
        assert_eq!(drawn(&h), long);
    }
}
