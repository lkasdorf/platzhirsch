//! The treemap below the table: the folder selected in the table as nested tiles.
//!
//! The layout is computed only when the cache key (shown node, metric, size in
//! whole pixels) changes. Per frame this draws the cached tiles and runs one
//! hit test under the pointer.
use eframe::egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::rows::TreeView;
use crate::format::format_bytes;
use crate::model::{Metric, NodeId, Tree};
use crate::treemap::{self, HEADER_MIN_H, HEADER_MIN_W, HEADER_PX, PAD_PX, Tile, hit_test, layout};

/// Tile colours per level for the light theme: own palette, 8 steps.
const LIGHT: [Color32; 8] = [
    Color32::from_rgb(214, 226, 236),
    Color32::from_rgb(158, 199, 222),
    Color32::from_rgb(163, 212, 170),
    Color32::from_rgb(238, 208, 135),
    Color32::from_rgb(233, 164, 140),
    Color32::from_rgb(196, 170, 222),
    Color32::from_rgb(140, 206, 200),
    Color32::from_rgb(214, 196, 160),
];
/// The same steps, darker, for the dark theme.
const DARK: [Color32; 8] = [
    Color32::from_rgb(52, 60, 70),
    Color32::from_rgb(44, 86, 112),
    Color32::from_rgb(52, 104, 66),
    Color32::from_rgb(128, 100, 38),
    Color32::from_rgb(126, 66, 50),
    Color32::from_rgb(88, 66, 122),
    Color32::from_rgb(36, 104, 100),
    Color32::from_rgb(100, 88, 62),
];
/// Thinner tiles get no outline: it would cover the whole tile.
const OUTLINE_MIN_PX: f32 = 3.0;
/// Outline of the tile of the entry selected in the table.
const SELECTED_LIGHT: Color32 = Color32::from_rgb(200, 30, 80);
const SELECTED_DARK: Color32 = Color32::from_rgb(255, 110, 150);

/// Laid-out tiles of one node at one size, kept across frames.
#[derive(Default)]
pub struct TreemapCache {
    /// Shown node, metric, width and height in whole pixels.
    key: Option<(NodeId, Metric, u32, u32)>,
    tiles: Vec<Tile>,
    computations: u32,
}

impl TreemapCache {
    /// How many times the layout was computed.
    pub fn computations(&self) -> u32 {
        self.computations
    }

    /// The tiles of `node` in a `width` x `height` area with its origin at 0,0.
    /// Computed only when node, metric or the size in whole pixels changes;
    /// a size below one pixel (or not a number) gives no tiles.
    pub fn tiles(&mut self, tree: &Tree, node: NodeId, metric: Metric, width: f32, height: f32) -> &[Tile] {
        let whole = |v: f32| {
            let r = v.round();
            (r.is_finite() && r >= 1.0).then_some(r as u32)
        };
        let (Some(w), Some(h)) = (whole(width), whole(height)) else { return &[] };
        let key = (node, metric, w, h);
        if self.key != Some(key) {
            let bounds = treemap::Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
            self.tiles = layout(tree, node, metric, bounds);
            self.key = Some(key);
            self.computations += 1;
        }
        &self.tiles
    }
}

/// Fill colour of a tile at `depth`: own palette of 8 steps, then it repeats.
pub fn depth_color(depth: u8, dark: bool) -> Color32 {
    let palette = if dark { &DARK } else { &LIGHT };
    palette[usize::from(depth) % palette.len()]
}

/// The folder the treemap shows: the selection, or a selected file's parent.
pub fn shown_node(tree: &Tree, selected: NodeId) -> NodeId {
    if tree.is_dir(selected) {
        selected
    } else {
        tree.parent(selected).unwrap_or_else(|| tree.root())
    }
}

/// Draws the treemap of the selected folder into all of `ui`'s available
/// space. Returns the entry whose tile was clicked.
pub fn show(ui: &mut Ui, tree: &Tree, view: &TreeView, cache: &mut TreemapCache) -> Option<NodeId> {
    let size = ui.available_size();
    if !(size.x >= 1.0 && size.y >= 1.0) {
        return None;
    }
    let (area, mut response) = ui.allocate_exact_size(size, Sense::CLICK);
    let metric = view.metric();
    let tiles = cache.tiles(tree, shown_node(tree, view.selected), metric, area.width(), area.height());
    if tiles.is_empty() {
        return None;
    }

    let dark = ui.visuals().dark_mode;
    let outline = Stroke::new(1.0, Color32::from_black_alpha(if dark { 150 } else { 80 }));
    let text_color = if dark { Color32::from_gray(225) } else { Color32::from_gray(25) };
    let font = FontId::proportional(HEADER_PX - 3.0);
    let painter = ui.painter_at(area);
    let to_screen = |t: &Tile| Rect::from_min_size(area.min + vec2(t.rect.x, t.rect.y), vec2(t.rect.w, t.rect.h));

    // The one hit test of this frame.
    let hovered = response.hover_pos().and_then(|p| hit_test(tiles, p.x - area.min.x, p.y - area.min.y));
    let (mut hovered_rect, mut selected_rect) = (None, None);
    for tile in tiles {
        let rect = to_screen(tile);
        painter.rect_filled(rect, 0.0, depth_color(tile.depth, dark));
        if tile.rect.w.min(tile.rect.h) >= OUTLINE_MIN_PX {
            painter.rect_stroke(rect, 0.0, outline, StrokeKind::Inside);
        }
        if tree.is_dir(tile.node) && tile.rect.w >= HEADER_MIN_W && tile.rect.h >= HEADER_MIN_H {
            // The header strip the layout reserved; the name is clipped to it.
            let strip = Rect::from_min_size(
                rect.min + vec2(PAD_PX, PAD_PX),
                vec2(rect.width() - 2.0 * PAD_PX, HEADER_PX),
            );
            painter.with_clip_rect(strip).text(
                pos2(strip.min.x + 3.0, strip.center().y),
                Align2::LEFT_CENTER,
                tree.name(tile.node),
                font.clone(),
                text_color,
            );
        }
        if Some(tile.node) == hovered {
            hovered_rect = Some(rect);
        }
        if tile.node == view.selected {
            selected_rect = Some(rect);
        }
    }
    if let Some(rect) = hovered_rect {
        painter.rect_filled(rect, 0.0, Color32::from_white_alpha(if dark { 40 } else { 90 }));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, text_color), StrokeKind::Inside);
    }
    if let Some(rect) = selected_rect {
        let color = if dark { SELECTED_DARK } else { SELECTED_LIGHT };
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, color), StrokeKind::Inside);
    }

    if let Some(node) = hovered {
        response = response.on_hover_ui_at_pointer(|ui| {
            ui.label(tree.path(node));
            ui.label(format_bytes(tree.metric(node, metric)));
        });
    }
    if response.clicked() { hovered } else { None }
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
        b.finish(root).0
    }

    #[test]
    fn palette_has_eight_levels_then_repeats() {
        assert_ne!(depth_color(0, false), depth_color(1, false));
        assert_eq!(depth_color(0, false), depth_color(8, false));
        for d in 0..8u8 {
            for e in d + 1..8 {
                assert_ne!(depth_color(d, false), depth_color(e, false), "light {d} vs {e}");
                assert_ne!(depth_color(d, true), depth_color(e, true), "dark {d} vs {e}");
            }
        }
        assert_eq!(depth_color(3, true), depth_color(11, true));
        assert_ne!(depth_color(0, true), depth_color(0, false));
    }

    #[test]
    fn cache_computes_once_per_key() {
        let t = sample();
        let mut cache = TreemapCache::default();
        assert_eq!(cache.computations(), 0);
        let n = cache.tiles(&t, 0, Metric::Size, 400.0, 200.0).len();
        assert!(n > 1);
        cache.tiles(&t, 0, Metric::Size, 400.0, 200.0);
        // Sub-pixel jitter rounds to the same key.
        cache.tiles(&t, 0, Metric::Size, 400.2, 199.8);
        assert_eq!(cache.computations(), 1);
    }

    #[test]
    fn cache_recomputes_for_other_node_size_or_metric() {
        let t = sample();
        let mut cache = TreemapCache::default();
        cache.tiles(&t, 0, Metric::Size, 400.0, 200.0);
        cache.tiles(&t, 1, Metric::Size, 400.0, 200.0);
        assert_eq!(cache.computations(), 2);
        assert_eq!(cache.tiles(&t, 1, Metric::Size, 400.0, 200.0)[0].node, 1);
        assert_eq!(cache.computations(), 2);
        cache.tiles(&t, 1, Metric::Size, 500.0, 200.0);
        assert_eq!(cache.computations(), 3);
        cache.tiles(&t, 1, Metric::Allocated, 500.0, 200.0);
        assert_eq!(cache.computations(), 4);
    }

    #[test]
    fn cache_with_zero_size_has_no_tiles() {
        let t = sample();
        let mut cache = TreemapCache::default();
        assert!(cache.tiles(&t, 0, Metric::Size, 0.0, 200.0).is_empty());
        assert!(cache.tiles(&t, 0, Metric::Size, 0.4, 0.4).is_empty());
        assert!(cache.tiles(&t, 0, Metric::Size, f32::NAN, 200.0).is_empty());
        assert!(cache.tiles(&t, 0, Metric::Size, -5.0, 200.0).is_empty());
    }

    #[test]
    fn shown_node_is_selected_folder_or_parent_of_file() {
        let t = sample();
        assert_eq!(shown_node(&t, 0), 0);
        assert_eq!(shown_node(&t, 4), 4);
        assert_eq!(shown_node(&t, 2), 1);
        assert_eq!(shown_node(&t, 6), 0);
    }
}
