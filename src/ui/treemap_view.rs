//! The treemap below the table: the folder selected in the table as nested tiles.
//!
//! The layout is computed only when the shown node or the metric changes, or
//! when the panel has kept a new size (in whole points) for [`RESIZE_SETTLE`].
//! While the size is still changing, the last layout is drawn stretched to
//! the panel. The fills and outlines of all tiles are one cached mesh, built
//! when the layout, the panel's position or size, or the theme changes. Per
//! frame this adds that mesh, the folder names, the hover highlight and the
//! selection outline, and runs one hit test under the pointer.
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    Align2, Color32, FontId, Mesh, Pos2, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2, vec2,
};

use super::rows::TreeView;
use super::table::full_path;
use crate::format::{format_bytes, format_count};
use crate::model::{Metric, NodeId, Tree};
use crate::treemap::{self, HEADER_PT, Tile, header_strip, hit_index, hit_test, layout};

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
/// Folders whose content is too small to draw keep this share of their
/// level colour's brightness.
const ELIDED_SHADE: f32 = 0.65;
/// Thinner tiles (in points) get no outline: it would cover the whole tile.
const OUTLINE_MIN_PT: f32 = 3.0;
/// Width of a tile's outline in points.
const OUTLINE_PT: f32 = 1.0;
/// Outline of the tile of the entry selected in the table.
const SELECTED_LIGHT: Color32 = Color32::from_rgb(200, 30, 80);
const SELECTED_DARK: Color32 = Color32::from_rgb(255, 110, 150);
/// How long the panel must keep a new size before the layout is computed for it.
pub const RESIZE_SETTLE: Duration = Duration::from_millis(150);

/// Shown node, metric, width and height in whole points.
type LayoutKey = (NodeId, Metric, u32, u32);

/// Where the laid-out tiles go on screen: `origin` is the panel's top left
/// corner, `scale` stretches the layout to the panel while a new size settles.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    origin: Pos2,
    scale: Vec2,
}

impl Mapping {
    fn to_screen(self, r: treemap::Rect) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(r.x * self.scale.x, r.y * self.scale.y),
            vec2(r.w * self.scale.x, r.h * self.scale.y),
        )
    }

    /// The layout coordinates of a screen point: the inverse of `to_screen`.
    fn to_layout(self, p: Pos2) -> (f32, f32) {
        ((p.x - self.origin.x) / self.scale.x, (p.y - self.origin.y) / self.scale.y)
    }
}

/// What [`TreemapCache::update`] has for this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Laid {
    /// Width and height in points the cached tiles were laid out for.
    pub w: u32,
    pub h: u32,
    /// While a new panel size waits to settle: how long until it is due.
    pub settle_in: Option<Duration>,
}

/// Laid-out tiles of one node at one size, and their mesh, kept across frames.
#[derive(Default)]
pub struct TreemapCache {
    key: Option<LayoutKey>,
    tiles: Vec<Tile>,
    /// Indices of the tiles that have a header strip for the folder name.
    headers: Vec<usize>,
    computations: u32,
    /// A panel size other than the laid-out one, and since when it is unchanged.
    resizing: Option<((u32, u32), Instant)>,
    /// The fills and outlines of all tiles, for the computation, the mapping
    /// and the theme it was built for.
    mesh: Option<((u32, Mapping, bool), Arc<Mesh>)>,
    mesh_builds: u32,
    /// The selected node, the computation, and the index of its tile.
    selected: Option<(NodeId, u32, Option<usize>)>,
}

impl TreemapCache {
    /// How many times the layout was computed.
    pub fn computations(&self) -> u32 {
        self.computations
    }

    /// The cached tiles, in layout coordinates with the origin at 0,0.
    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    /// Brings the tiles up to date for `node` in a `width` x `height` area.
    /// A new node or metric is laid out at once. A new size (in whole points)
    /// is laid out only once it has been the same for [`RESIZE_SETTLE`] up to
    /// `now`; until then the old tiles stay. A size below one point (or not a
    /// number) gives `None`: nothing to draw.
    pub fn update(&mut self, tree: &Tree, node: NodeId, metric: Metric, width: f32, height: f32, now: Instant) -> Option<Laid> {
        let whole = |v: f32| {
            let r = v.round();
            (r.is_finite() && r >= 1.0).then_some(r as u32)
        };
        let (Some(w), Some(h)) = (whole(width), whole(height)) else { return None };
        let want = (node, metric, w, h);
        let mut settle_in = None;
        match self.key {
            Some(key) if key == want => self.resizing = None,
            Some((n, m, _, _)) if n == node && m == metric => {
                let since = match self.resizing {
                    Some((size, since)) if size == (w, h) => since,
                    _ => {
                        self.resizing = Some(((w, h), now));
                        now
                    }
                };
                let waited = now.saturating_duration_since(since);
                if waited >= RESIZE_SETTLE {
                    self.compute(tree, want);
                } else {
                    settle_in = Some(RESIZE_SETTLE - waited);
                }
            }
            _ => self.compute(tree, want),
        }
        let (_, _, w, h) = self.key?;
        Some(Laid { w, h, settle_in })
    }

    fn compute(&mut self, tree: &Tree, key: LayoutKey) {
        let (node, metric, w, h) = key;
        let bounds = treemap::Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
        self.tiles = layout(tree, node, metric, bounds);
        self.headers = (0..self.tiles.len()).filter(|&i| header_strip(tree, &self.tiles[i]).is_some()).collect();
        self.key = Some(key);
        self.resizing = None;
        self.computations += 1;
    }

    /// The mesh of the cached tiles placed by `map`; rebuilt only when the
    /// layout, the mapping or the theme changed since the last call.
    fn mesh(&mut self, map: Mapping, dark: bool) -> Arc<Mesh> {
        let key = (self.computations, map, dark);
        if let Some((k, mesh)) = &self.mesh
            && *k == key
        {
            return Arc::clone(mesh);
        }
        let mesh = Arc::new(build_mesh(&self.tiles, map, dark));
        self.mesh = Some((key, Arc::clone(&mesh)));
        self.mesh_builds += 1;
        mesh
    }

    /// The index of `node`'s tile; searched once per selection and layout.
    fn tile_of(&mut self, node: NodeId) -> Option<usize> {
        match self.selected {
            Some((n, c, at)) if n == node && c == self.computations => at,
            _ => {
                let at = self.tiles.iter().position(|t| t.node == node);
                self.selected = Some((node, self.computations, at));
                at
            }
        }
    }
}

/// Fill colour of a tile at `depth`: own palette of 8 steps, then it repeats.
pub fn depth_color(depth: u8, dark: bool) -> Color32 {
    let palette = if dark { &DARK } else { &LIGHT };
    palette[usize::from(depth) % palette.len()]
}

/// Fill colour of a tile: its level colour, darker if its content is elided.
pub fn tile_color(tile: &Tile, dark: bool) -> Color32 {
    let c = depth_color(tile.depth, dark);
    if tile.elided { scaled(c, ELIDED_SHADE) } else { c }
}

/// `c` with every channel multiplied by `f`.
fn scaled(c: Color32, f: f32) -> Color32 {
    let ch = |v: u8| (f32::from(v) * f).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(ch(c.r()), ch(c.g()), ch(c.b()))
}

/// The outline colour over `fill`: black at the theme's opacity, blended in
/// beforehand so the mesh needs no transparency.
fn outline_color(fill: Color32, dark: bool) -> Color32 {
    let alpha = if dark { 150.0 } else { 80.0 };
    scaled(fill, 1.0 - alpha / 255.0)
}

/// The fills and outlines of all tiles as one mesh in screen coordinates.
/// An outlined tile is two rectangles, the outline colour and the fill inset
/// by the outline width; a thinner tile is one rectangle. Four vertices and
/// two triangles per rectangle, so the vertex count stays far below the
/// `u32` index range.
fn build_mesh(tiles: &[Tile], map: Mapping, dark: bool) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(tiles.len() * 8);
    mesh.reserve_triangles(tiles.len() * 4);
    for tile in tiles {
        let fill = tile_color(tile, dark);
        let rect = map.to_screen(tile.rect);
        if tile.rect.w.min(tile.rect.h) >= OUTLINE_MIN_PT {
            mesh.add_colored_rect(rect, outline_color(fill, dark));
            mesh.add_colored_rect(rect.shrink(OUTLINE_PT), fill);
        } else {
            mesh.add_colored_rect(rect, fill);
        }
    }
    mesh
}

/// The folder the treemap shows: the selection, or a selected file's parent.
pub fn shown_node(tree: &Tree, selected: NodeId) -> NodeId {
    if tree.is_dir(selected) {
        selected
    } else {
        tree.parent(selected).unwrap_or_else(|| tree.root())
    }
}

/// Over the header strip of the first tile (the folder shown) at layout
/// point `x`, `y`: that folder's parent, or the folder itself at the tree's
/// root. `None` elsewhere, and when that tile has no header strip.
pub fn up_target(tree: &Tree, tiles: &[Tile], x: f32, y: f32) -> Option<NodeId> {
    let shown = tiles.first()?;
    let strip = header_strip(tree, shown)?;
    strip.contains(x, y).then(|| tree.parent(shown.node).unwrap_or(shown.node))
}

/// What a click at layout point `x`, `y` selects: the way up over the shown
/// folder's header strip, elsewhere the deepest tile there.
pub fn click_target(tree: &Tree, tiles: &[Tile], x: f32, y: f32) -> Option<NodeId> {
    up_target(tree, tiles, x, y).or_else(|| hit_test(tiles, x, y))
}

/// Draws the treemap of the selected folder into all of `ui`'s available
/// space. Returns the entry the click selects: the clicked tile's, or the
/// shown folder's parent for a click on its header strip.
pub fn show(ui: &mut Ui, tree: &Tree, view: &TreeView, cache: &mut TreemapCache, now: Instant) -> Option<NodeId> {
    let size = ui.available_size();
    if !(size.x >= 1.0 && size.y >= 1.0) {
        return None;
    }
    let (area, mut response) = ui.allocate_exact_size(size, Sense::CLICK);
    let metric = view.metric();
    let shown = shown_node(tree, view.selected);
    // One image for screen readers; it has no keyboard access of its own and
    // follows the table's selection. The name is built only while they read.
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, format!("Treemap of {}", full_path(tree, shown))));
    let laid = cache.update(tree, shown, metric, area.width(), area.height(), now)?;
    if let Some(wait) = laid.settle_in {
        // Nothing else may cause a frame once the user stops dragging.
        ui.ctx().request_repaint_after(wait);
    }
    let map = Mapping {
        origin: area.min,
        scale: vec2(area.width() / laid.w as f32, area.height() / laid.h as f32),
    };

    let dark = ui.visuals().dark_mode;
    let painter = ui.painter_at(area);
    painter.add(Shape::mesh(cache.mesh(map, dark)));
    let selected_at = cache.tile_of(view.selected);
    let tiles = cache.tiles();

    let text_color = if dark { Color32::from_gray(225) } else { Color32::from_gray(25) };
    let font = FontId::proportional(HEADER_PT - 3.0);
    for &at in &cache.headers {
        let tile = &tiles[at];
        let Some(strip) = header_strip(tree, tile) else { continue };
        let strip = map.to_screen(strip);
        painter.with_clip_rect(strip).text(
            pos2(strip.min.x + 3.0, strip.center().y),
            Align2::LEFT_CENTER,
            tree.name(tile.node),
            font.clone(),
            text_color,
        );
    }

    // The one hit test of this frame, in layout coordinates.
    let pointer = response.hover_pos().map(|p| map.to_layout(p));
    let hovered = pointer.and_then(|(x, y)| hit_index(tiles, x, y));
    if let (Some(at), Some((x, y))) = (hovered, pointer) {
        let tile = &tiles[at];
        let strip = header_strip(tree, tile);
        let in_strip = strip.is_some_and(|s| s.contains(x, y));
        // A folder's own visible area is its strip; its body shows its children.
        let lit = match strip {
            Some(s) if in_strip || !tile.elided => s,
            _ => tile.rect,
        };
        painter.rect_filled(map.to_screen(lit), 0.0, Color32::from_white_alpha(if dark { 40 } else { 90 }));
        painter.rect_stroke(map.to_screen(tile.rect), 0.0, Stroke::new(1.0, text_color), StrokeKind::Inside);
    }
    if let Some(at) = selected_at {
        let color = if dark { SELECTED_DARK } else { SELECTED_LIGHT };
        painter.rect_stroke(map.to_screen(tiles[at].rect), 0.0, Stroke::new(2.0, color), StrokeKind::Inside);
    }

    if let (Some(at), Some((x, y))) = (hovered, pointer) {
        let tile = tiles[at];
        let go_up = up_target(tree, tiles, x, y).filter(|&n| n != tile.node).is_some();
        response = response.on_hover_ui_at_pointer(|ui| {
            ui.label(tree.path(tile.node));
            ui.label(format_bytes(tree.metric(tile.node, metric)));
            if tile.elided {
                let n = u64::from(tree.file_count(tile.node)) + u64::from(tree.dir_count(tile.node));
                ui.label(format!("{} entries too small to draw", format_count(n)));
            }
            if go_up {
                ui.label("Click to go up");
            }
        });
    }
    match pointer {
        Some((x, y)) if response.clicked() => click_target(tree, cache.tiles(), x, y),
        _ => None,
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
        b.finish(root).0
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
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
    fn elided_tiles_are_visibly_darker_in_both_themes() {
        let tile = |depth, elided| Tile { node: 0, rect: treemap::Rect { x: 0.0, y: 0.0, w: 9.0, h: 9.0 }, depth, elided };
        for dark in [false, true] {
            for d in 0..8u8 {
                let (plain, shaded) = (tile_color(&tile(d, false), dark), tile_color(&tile(d, true), dark));
                assert_eq!(plain, depth_color(d, dark));
                let sum = |c: Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
                assert!(sum(plain) >= sum(shaded) + 40, "depth {d}, dark {dark}: {plain:?} vs {shaded:?}");
            }
        }
    }

    #[test]
    fn cache_computes_once_per_key() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        assert_eq!(cache.computations(), 0);
        let laid = cache.update(&t, 0, Metric::Size, 400.0, 200.0, now).unwrap();
        assert_eq!(laid, Laid { w: 400, h: 200, settle_in: None });
        assert!(cache.tiles().len() > 1);
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, now);
        // Sub-point jitter rounds to the same key.
        cache.update(&t, 0, Metric::Size, 400.2, 199.8, now);
        assert_eq!(cache.computations(), 1);
    }

    #[test]
    fn cache_recomputes_for_other_node_size_or_metric() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, now);
        cache.update(&t, 1, Metric::Size, 400.0, 200.0, now);
        assert_eq!(cache.computations(), 2);
        cache.update(&t, 1, Metric::Size, 400.0, 200.0, now);
        assert_eq!(cache.tiles()[0].node, 1);
        assert_eq!(cache.computations(), 2);
        // A new size is laid out once it has settled.
        cache.update(&t, 1, Metric::Size, 500.0, 200.0, now);
        cache.update(&t, 1, Metric::Size, 500.0, 200.0, now + RESIZE_SETTLE);
        assert_eq!(cache.computations(), 3);
        cache.update(&t, 1, Metric::Allocated, 500.0, 200.0, now + RESIZE_SETTLE);
        assert_eq!(cache.computations(), 4);
    }

    #[test]
    fn resize_burst_is_laid_out_once_after_it_settles() {
        let t = sample();
        let start = Instant::now();
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, start);
        assert_eq!(cache.computations(), 1);
        // Ten different sizes, one frame of 16 ms apart, as while dragging.
        let mut now = start;
        for i in 1..=10u16 {
            now = start + ms(16 * u64::from(i));
            let laid = cache.update(&t, 0, Metric::Size, 400.0 + f32::from(i) * 3.0, 200.0, now).unwrap();
            // The old layout stays, to be drawn stretched; a repaint is due when the size settles.
            assert_eq!((laid.w, laid.h, laid.settle_in), (400, 200, Some(RESIZE_SETTLE)));
        }
        assert_eq!(cache.computations(), 1);
        // The last size again, but not yet long enough.
        let laid = cache.update(&t, 0, Metric::Size, 430.0, 200.0, now + ms(100)).unwrap();
        assert_eq!(laid.settle_in, Some(ms(50)));
        assert_eq!(cache.computations(), 1);
        // The last size after the delay: one computation for the whole burst.
        let laid = cache.update(&t, 0, Metric::Size, 430.0, 200.0, now + RESIZE_SETTLE).unwrap();
        assert_eq!(laid, Laid { w: 430, h: 200, settle_in: None });
        assert_eq!(cache.computations(), 2);
    }

    #[test]
    fn going_back_to_the_laid_out_size_needs_no_layout() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, now);
        cache.update(&t, 0, Metric::Size, 410.0, 200.0, now + ms(10));
        let laid = cache.update(&t, 0, Metric::Size, 400.0, 200.0, now + ms(20)).unwrap();
        assert_eq!(laid.settle_in, None);
        // The earlier pending size starts over when it comes back.
        let laid = cache.update(&t, 0, Metric::Size, 410.0, 200.0, now + ms(400)).unwrap();
        assert_eq!(laid.settle_in, Some(RESIZE_SETTLE));
        assert_eq!(cache.computations(), 1);
    }

    #[test]
    fn node_or_metric_change_during_a_resize_is_laid_out_at_once() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, now);
        cache.update(&t, 0, Metric::Size, 450.0, 200.0, now + ms(10));
        let laid = cache.update(&t, 1, Metric::Size, 450.0, 200.0, now + ms(20)).unwrap();
        assert_eq!(laid, Laid { w: 450, h: 200, settle_in: None });
        assert_eq!(cache.computations(), 2);
        cache.update(&t, 1, Metric::Allocated, 450.0, 200.0, now + ms(30));
        assert_eq!(cache.computations(), 3);
    }

    #[test]
    fn cache_with_zero_size_has_no_tiles() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        assert!(cache.update(&t, 0, Metric::Size, 0.0, 200.0, now).is_none());
        assert!(cache.update(&t, 0, Metric::Size, 0.4, 0.4, now).is_none());
        assert!(cache.update(&t, 0, Metric::Size, f32::NAN, 200.0, now).is_none());
        assert!(cache.update(&t, 0, Metric::Size, -5.0, 200.0, now).is_none());
        assert!(cache.tiles().is_empty());
        assert_eq!(cache.computations(), 0);
    }

    #[test]
    fn shown_node_is_selected_folder_or_parent_of_file() {
        let t = sample();
        assert_eq!(shown_node(&t, 0), 0);
        assert_eq!(shown_node(&t, 4), 4);
        assert_eq!(shown_node(&t, 2), 1);
        assert_eq!(shown_node(&t, 6), 0);
    }

    #[test]
    fn mesh_has_every_tile_and_is_built_once_per_layout_place_and_theme() {
        let t = sample();
        let now = Instant::now();
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 400.0, 200.0, now);
        let map = Mapping { origin: pos2(10.0, 20.0), scale: vec2(1.0, 1.0) };
        let mesh = cache.mesh(map, false);
        let tiles = cache.tiles();
        let outlined = tiles.iter().filter(|x| x.rect.w.min(x.rect.h) >= OUTLINE_MIN_PT).count();
        assert_eq!(mesh.vertices.len(), 4 * (tiles.len() + outlined));
        assert_eq!(mesh.indices.len(), 6 * (tiles.len() + outlined));
        assert!(mesh.is_valid());
        // The first rectangle is the root tile's outline, placed at the origin.
        assert_eq!(mesh.vertices[0].pos, pos2(10.0, 20.0));
        assert_eq!(mesh.vertices[0].color, outline_color(depth_color(0, false), false));
        assert_eq!(mesh.vertices[4].color, depth_color(0, false));
        assert_eq!(mesh.vertices[4].pos, pos2(11.0, 21.0));
        // The same place and theme reuse the mesh; another place, size or theme rebuilds it.
        cache.mesh(map, false);
        assert_eq!(cache.mesh_builds, 1);
        let stretched = Mapping { scale: vec2(1.5, 1.0), ..map };
        let wide = cache.mesh(stretched, false);
        assert_eq!(wide.vertices[1].pos, pos2(10.0 + 600.0, 20.0));
        cache.mesh(stretched, true);
        assert_eq!(cache.mesh_builds, 3);
    }

    #[test]
    fn elided_folder_is_darker_in_the_mesh() {
        let mut b = TreeBuilder::with_capacity(100_001, 64);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR);
        for _ in 0..100_000 {
            b.push("f", root, 1, 1, 0, 0);
        }
        let t = b.finish(root).0;
        let mut cache = TreemapCache::default();
        cache.update(&t, 0, Metric::Size, 300.0, 200.0, Instant::now());
        assert!(cache.tiles()[0].elided);
        let mesh = cache.mesh(Mapping { origin: Pos2::ZERO, scale: Vec2::splat(1.0) }, true);
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.vertices[4].color, scaled(depth_color(0, true), ELIDED_SHADE));
    }

    #[test]
    fn mapping_round_trips() {
        let map = Mapping { origin: pos2(5.0, 7.0), scale: vec2(1.25, 0.5) };
        let r = map.to_screen(treemap::Rect { x: 8.0, y: 10.0, w: 4.0, h: 6.0 });
        assert_eq!(r, Rect::from_min_size(pos2(15.0, 12.0), vec2(5.0, 3.0)));
        assert_eq!(map.to_layout(pos2(15.0, 12.0)), (8.0, 10.0));
    }

    #[test]
    fn root_header_click_goes_up() {
        let t = sample();
        let bounds = treemap::Rect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 };
        // Showing folder `a` (1): its header strip leads to its parent, the root.
        let tiles = layout(&t, 1, Metric::Size, bounds);
        assert_eq!(up_target(&t, &tiles, 50.0, 5.0), Some(0));
        assert_eq!(click_target(&t, &tiles, 50.0, 5.0), Some(0));
        // Below the strip the click selects the tile there, as before.
        assert_eq!(up_target(&t, &tiles, 50.0, 100.0), None);
        assert_eq!(click_target(&t, &tiles, 50.0, 100.0), hit_test(&tiles, 50.0, 100.0));
        // At the tree's root the strip selects the root itself.
        let tiles = layout(&t, 0, Metric::Size, bounds);
        assert_eq!(up_target(&t, &tiles, 50.0, 5.0), Some(0));
        // A shown folder too small for a strip has no way up.
        let tiles = layout(&t, 1, Metric::Size, treemap::Rect { x: 0.0, y: 0.0, w: 30.0, h: 20.0 });
        assert_eq!(up_target(&t, &tiles, 5.0, 5.0), None);
        assert_eq!(up_target(&t, &[], 5.0, 5.0), None);
    }

    // Headless tests of `show`: no window, no renderer, and the clock is passed in.
    mod ui {
        use super::*;
        use eframe::egui::{Event, PointerButton, ViewportId};
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        struct State {
            tree: Tree,
            view: TreeView,
            cache: TreemapCache,
            now: Instant,
            clicked: Option<NodeId>,
        }

        /// Folder `a` (1) with 2,000 files of different sizes under the root, shown.
        fn harness() -> Harness<'static, State> {
            let mut b = TreeBuilder::with_capacity(2_003, 64);
            let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR);
            let a = b.push("a", root, 0, 0, 0, F_DIR);
            for i in 0..2_000u64 {
                b.push("f", a, 1 + (i * 7919) % 5_000, 0, 0, 0);
            }
            b.push("top.txt", root, 1, 0, 0, 0);
            let tree = b.finish(root).0;
            let mut view = TreeView::new(&tree, Metric::Size);
            view.reveal(&tree, a);
            let state = State { tree, view, cache: TreemapCache::default(), now: Instant::now(), clicked: None };
            Harness::builder()
                .with_size(vec2(800.0, 300.0))
                .build_ui_state(
                    |ui, s: &mut State| {
                        if let Some(n) = show(ui, &s.tree, &s.view, &mut s.cache, s.now) {
                            s.clicked = Some(n);
                        }
                    },
                    state,
                )
        }

        fn origin(h: &Harness<'_, State>) -> Pos2 {
            h.state().cache.mesh.as_ref().map(|((_, map, _), _)| map.origin).unwrap()
        }

        #[test]
        fn one_mesh_for_all_tiles_and_no_rebuild_per_frame() {
            let mut h = harness();
            h.run();
            let tiles = h.state().cache.tiles().len();
            assert!(tiles > 500, "{tiles} tiles");
            let shapes = &h.output().shapes;
            let meshes = shapes.iter().filter(|s| matches!(s.shape, Shape::Mesh(_))).count();
            assert_eq!(meshes, 1);
            assert!(shapes.len() < 20, "{} shapes for {tiles} tiles", shapes.len());
            h.run();
            h.run();
            assert_eq!(h.state().cache.computations(), 1);
            assert_eq!(h.state().cache.mesh_builds, 1);
        }

        #[test]
        fn treemap_is_one_image_named_by_the_folder_it_shows() {
            let mut h = harness();
            h.run();
            assert!(h.query_by_role_and_label(Role::Image, "Treemap of C:\\a").is_some());
            // It follows the table's selection: a file shows its folder, the root gets its backslash.
            let s = h.state_mut();
            s.view.reveal(&s.tree, 2_002);
            h.run();
            assert!(h.query_by_role_and_label(Role::Image, "Treemap of C:\\").is_some());
            assert!(h.query_by_role_and_label(Role::Image, "Treemap of C:\\a").is_none());
        }

        #[test]
        fn click_on_the_shown_folders_header_goes_up() {
            let mut h = harness();
            h.run();
            let at = origin(&h) + vec2(100.0, 6.0);
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() });
            }
            h.run();
            assert_eq!(h.state().clicked, Some(0));
        }

        #[test]
        fn resize_waits_and_asks_for_a_repaint_when_it_settles() {
            let mut h = harness();
            h.run();
            h.set_size(vec2(700.0, 280.0));
            h.run();
            assert_eq!(h.state().cache.computations(), 1);
            let delay = h.output().viewport_output[&ViewportId::ROOT].repaint_delay;
            assert!(delay <= RESIZE_SETTLE, "{delay:?}");
            // The stretched mesh still covers the new size, and the pointer hits what is drawn.
            let ((_, map, _), mesh) = h.state().cache.mesh.clone().unwrap();
            assert!(map.scale.x < 1.0);
            let mid = mesh.calc_bounds().center();
            let (x, y) = map.to_layout(mid);
            let node = hit_test(h.state().cache.tiles(), x, y).unwrap();
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: mid, button: PointerButton::Primary, pressed, modifiers: Default::default() });
            }
            h.run();
            assert_eq!(h.state().clicked, Some(node));
            // Once the delay has passed the next frame lays out the new size.
            h.state_mut().now += RESIZE_SETTLE;
            h.run();
            assert_eq!(h.state().cache.computations(), 2);
        }
    }
}
