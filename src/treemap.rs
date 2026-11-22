//! Squarified treemap layout (Bruls, Huizing, van Wijk).
//!
//! Pure geometry: turns one folder of the in-memory tree into nested
//! rectangles and answers which rectangle lies under a point. Nothing in
//! here recurses over tree depth; pending folders live on an explicit stack.
//! All lengths are in egui points (logical pixels), not physical pixels.

use crate::model::{Metric, NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub node: NodeId,
    pub rect: Rect,
    pub depth: u8,
    /// A folder with children of non-zero value of which none got a tile:
    /// all of them were below [`MIN_SIDE_PT`], or the folder's own tile was
    /// too small to subdivide.
    pub elided: bool,
}

/// Tiles narrower or lower than this many points are not subdivided further.
pub const MIN_TILE_PT: f32 = 4.0;
/// Space for the folder name in points, reserved when the tile is at least
/// `HEADER_MIN_W` x `HEADER_MIN_H` points.
pub const HEADER_PT: f32 = 14.0;
pub const HEADER_MIN_W: f32 = 40.0;
pub const HEADER_MIN_H: f32 = 28.0;
/// Gap in points between a folder's edge and its children.
pub const PAD_PT: f32 = 1.0;
/// Tiles whose smaller side is below this many points are not emitted; their
/// parent's area stays unsubdivided there.
pub const MIN_SIDE_PT: f32 = 1.0;

pub fn layout(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect) -> Vec<Tile> {
    layout_with(tree, root, metric, bounds, HEADER_PT, PAD_PT)
}

/// Squarified layout. Order: parents before children; the first tile is
/// `root` itself (depth 0, all of `bounds`). Entries with value 0 get no tile,
/// nor do entries whose tile would be thinner than [`MIN_SIDE_PT`]. A folder
/// tile none of whose children of non-zero value got a tile is marked
/// [`Tile::elided`].
pub fn layout_with(
    tree: &Tree,
    root: NodeId,
    metric: Metric,
    bounds: Rect,
    header_pt: f32,
    pad_pt: f32,
) -> Vec<Tile> {
    let mut out = vec![Tile { node: root, rect: bounds, depth: 0, elided: false }];
    let finite = bounds.x.is_finite()
        && bounds.y.is_finite()
        && bounds.w.is_finite()
        && bounds.h.is_finite();
    if !finite || bounds.w <= 0.0 || bounds.h <= 0.0 {
        out[0].elided = has_valued_child(tree, root, metric);
        return out;
    }
    let sane = |v: f32| if v.is_finite() { f64::from(v.max(0.0)) } else { 0.0 };
    let (header, pad) = (sane(header_pt), sane(pad_pt));

    // Indices into `out` of the folder tiles still to subdivide.
    let mut stack = vec![0usize];
    let mut kids: Vec<(NodeId, u64)> = Vec::new();
    while let Some(at) = stack.pop() {
        let p = out[at];
        if !tree.is_dir(p.node) {
            continue;
        }
        kids.clear();
        let mut total: u128 = 0;
        for &c in tree.children(p.node) {
            let v = tree.metric(c, metric);
            if v > 0 {
                kids.push((c, v));
                total += u128::from(v);
            }
        }
        if kids.is_empty() {
            continue; // empty, or only entries of value 0: nothing is hidden
        }
        let mut area = F64Rect {
            x: f64::from(p.rect.x) + pad,
            y: f64::from(p.rect.y) + pad,
            w: f64::from(p.rect.w) - 2.0 * pad,
            h: f64::from(p.rect.h) - 2.0 * pad,
        };
        if p.rect.w >= HEADER_MIN_W && p.rect.h >= HEADER_MIN_H {
            area.y += header;
            area.h -= header;
        }
        let placed = if p.rect.w < MIN_TILE_PT || p.rect.h < MIN_TILE_PT || !(area.w > 0.0 && area.h > 0.0) {
            0
        } else {
            kids.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            squarify(tree, &kids, total, area, p.depth.saturating_add(1), &mut out, &mut stack)
        };
        out[at].elided = placed == 0;
    }
    out
}

fn has_valued_child(tree: &Tree, node: NodeId, metric: Metric) -> bool {
    tree.is_dir(node) && tree.children(node).iter().any(|&c| tree.metric(c, metric) > 0)
}

/// The strip at the top of a folder tile that [`layout`] reserves for the
/// folder's name, or `None` if the tile is too small to have one.
pub fn header_strip(tree: &Tree, tile: &Tile) -> Option<Rect> {
    let r = tile.rect;
    (tree.is_dir(tile.node) && r.w >= HEADER_MIN_W && r.h >= HEADER_MIN_H).then_some(Rect {
        x: r.x + PAD_PT,
        y: r.y + PAD_PT,
        w: r.w - 2.0 * PAD_PT,
        h: HEADER_PT,
    })
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy)]
struct F64Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Places `kids` (sorted by value, descending, all > 0) into `area` row by
/// row. Every placed tile goes to `out`; folders are queued on `stack` by
/// their index in `out`. Returns how many tiles were placed.
fn squarify(
    tree: &Tree,
    kids: &[(NodeId, u64)],
    total: u128,
    area: F64Rect,
    depth: u8,
    out: &mut Vec<Tile>,
    stack: &mut Vec<usize>,
) -> usize {
    let placed_before = out.len();
    let F64Rect { mut x, mut y, mut w, mut h } = area;
    let mut remaining = total;
    let mut i = 0;
    while i < kids.len() && w > 0.0 && h > 0.0 {
        // Every tile placed in the rest of the area is at most `min(w, h)`
        // across, so once that is below the cut-off no further tile is emitted.
        if w.min(h) < f64::from(MIN_SIDE_PT) {
            break;
        }
        let scale = w * h / remaining as f64; // points² per unit of value
        let side = w.min(h);
        let largest = kids[i].1 as f64 * scale;

        // Grow the row while the worst aspect ratio does not get worse.
        let mut j = i;
        let mut row_area = 0.0;
        let mut row_value: u128 = 0;
        let mut best = f64::INFINITY;
        while j < kids.len() {
            let a = kids[j].1 as f64 * scale;
            let sum = row_area + a;
            let worst = (side * side * largest / (sum * sum)).max(sum * sum / (side * side * a));
            if j > i && worst > best {
                break;
            }
            best = worst;
            row_area = sum;
            row_value += u128::from(kids[j].1);
            j += 1;
        }
        if !row_area.is_finite() || row_area <= 0.0 {
            break;
        }

        let last_row = j == kids.len();
        let wide = w >= h;
        // Thickness of the row across the short side; the last row takes what is left.
        let thick = if last_row {
            if wide { w } else { h }
        } else {
            (row_area / side).min(if wide { w } else { h })
        };
        let row_len = side;
        let mut offset = 0.0;
        for (k, &(node, value)) in kids[i..j].iter().enumerate() {
            let last_item = i + k + 1 == j;
            let len = if last_item {
                // Rounding can leave `offset` a hair above `row_len`.
                (row_len - offset).max(0.0)
            } else {
                row_len * (value as f64 / row_value as f64)
            };
            let r = if wide {
                F64Rect { x, y: y + offset, w: thick, h: len }
            } else {
                F64Rect { x: x + offset, y, w: len, h: thick }
            };
            offset += len;
            let rect = Rect { x: r.x as f32, y: r.y as f32, w: r.w as f32, h: r.h as f32 };
            if rect.w.is_nan() || rect.h.is_nan() || rect.w.min(rect.h) < MIN_SIDE_PT {
                continue;
            }
            if tree.is_dir(node) {
                stack.push(out.len());
            }
            out.push(Tile { node, rect, depth, elided: false });
        }

        if wide {
            x += thick;
            w -= thick;
        } else {
            y += thick;
            h -= thick;
        }
        remaining -= row_value;
        i = j;
    }
    out.len() - placed_before
}

/// The deepest tile under the point. Parents precede their children in
/// `tiles` and siblings are disjoint up to rounding, so this is the last tile
/// containing it.
pub fn hit_test(tiles: &[Tile], x: f32, y: f32) -> Option<NodeId> {
    hit_index(tiles, x, y).map(|i| tiles[i].node)
}

/// Like [`hit_test`], but the index of the tile in `tiles`.
pub fn hit_index(tiles: &[Tile], x: f32, y: f32) -> Option<usize> {
    tiles.iter().rposition(|t| t.rect.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TreeBuilder, F_DIR, NO_PARENT};
    use std::collections::HashMap;

    fn sample() -> Tree {
        let mut b = TreeBuilder::with_capacity(8, 64);
        let root = b.push("C:", NO_PARENT, 0, 0, 10, F_DIR); // 0
        let a = b.push("a", root, 0, 0, 20, F_DIR); // 1
        b.push("big.bin", a, 1000, 4096, 50, 0); // 2
        b.push("Small.txt", a, 10, 0, 30, 0); // 3
        let bd = b.push("b", a, 0, 0, 5, F_DIR); // 4
        b.push("x", bd, 5, 4096, 40, 0); // 5
        b.push("top.txt", root, 1, 4096, 1, 0); // 6
        b.push("lost", 99, 7, 7, 7, 0); // 7: parent missing
        b.finish(root).0
    }

    fn flat(sizes: &[u64]) -> Tree {
        let mut b = TreeBuilder::with_capacity(sizes.len() + 1, 64);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR);
        for &s in sizes {
            b.push("f", root, s, s, 0, 0);
        }
        b.finish(root).0
    }

    fn rect(w: f32, h: f32) -> Rect {
        Rect { x: 0.0, y: 0.0, w, h }
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        let e = 1e-2;
        inner.x >= outer.x - e
            && inner.y >= outer.y - e
            && inner.x + inner.w <= outer.x + outer.w + e
            && inner.y + inner.h <= outer.y + outer.h + e
    }

    fn overlap(a: Rect, b: Rect) -> f32 {
        let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    #[test]
    fn classic_example() {
        let sizes = [6, 6, 4, 3, 2, 2, 1];
        let t = flat(&sizes);
        let b = rect(600.0, 400.0);
        let tiles = layout_with(&t, 0, Metric::Size, b, 0.0, 0.0);
        assert_eq!(tiles.len(), 8);
        assert_eq!((tiles[0].node, tiles[0].depth, tiles[0].rect), (0, 0, b));
        let total = 600.0 * 400.0;
        for tile in &tiles[1..] {
            assert_eq!(tile.depth, 1);
            let want = t.size(tile.node) as f32 / 24.0;
            let got = tile.rect.w * tile.rect.h / total;
            assert!((got - want).abs() <= want * 0.01, "node {}: {got} vs {want}", tile.node);
            let ratio = tile.rect.w.max(tile.rect.h) / tile.rect.w.min(tile.rect.h);
            assert!(ratio <= 3.0, "aspect ratio {ratio}");
        }
    }

    /// The part of a folder tile its children are laid out in: minus the
    /// padding and, where one is reserved, minus the header.
    fn content(parent: Rect) -> Rect {
        let mut r = Rect {
            x: parent.x + PAD_PT,
            y: parent.y + PAD_PT,
            w: parent.w - 2.0 * PAD_PT,
            h: parent.h - 2.0 * PAD_PT,
        };
        if parent.w >= HEADER_MIN_W && parent.h >= HEADER_MIN_H {
            r.y += HEADER_PT;
            r.h -= HEADER_PT;
        }
        r
    }

    #[test]
    fn tiles_inside_parent_and_disjoint() {
        let t = sample();
        let tiles = layout(&t, 0, Metric::Size, rect(800.0, 300.0));
        assert!(tiles.len() > 4);
        let by_node: HashMap<NodeId, Rect> = tiles.iter().map(|x| (x.node, x.rect)).collect();
        for tile in &tiles[1..] {
            let parent = by_node[&t.parent(tile.node).unwrap()];
            assert!(inside(tile.rect, content(parent)), "node {} outside its parent's content", tile.node);
        }
        // At least one parent here reserves a header, so the check above covers it.
        assert!(tiles.iter().any(|x| header_strip(&t, x).is_some()));
        for (i, a) in tiles.iter().enumerate().skip(1) {
            for b in &tiles[i + 1..] {
                if t.parent(a.node) == t.parent(b.node) {
                    assert!(overlap(a.rect, b.rect) < 0.5, "{} overlaps {}", a.node, b.node);
                }
            }
        }
    }

    #[test]
    fn zero_total_is_empty() {
        let empty = flat(&[]);
        let zeros = flat(&[0, 0, 0]);
        for t in [&empty, &zeros] {
            let tiles = layout(t, 0, Metric::Size, rect(500.0, 500.0));
            assert_eq!(tiles.len(), 1);
            assert_eq!(tiles[0].node, 0);
        }
    }

    #[test]
    fn tiny_bounds() {
        let t = sample();
        for b in [rect(2.0, 2.0), rect(0.0, 100.0), rect(100.0, 0.0), rect(0.0, 0.0)] {
            assert!(layout(&t, 0, Metric::Size, b).len() <= 1);
        }
    }

    #[test]
    fn hit_deepest() {
        let t = sample();
        let tiles = layout(&t, 0, Metric::Size, rect(800.0, 300.0));
        let x = tiles.iter().find(|tile| tile.node == 5).expect("x has a tile").rect;
        assert_eq!(hit_test(&tiles, x.x + x.w / 2.0, x.y + x.h / 2.0), Some(5));
        // The root tile is hit where no child covers the point (outside the padding of a).
        assert_eq!(hit_test(&tiles, 0.0, 0.0), Some(0));
        assert_eq!(hit_test(&tiles, -5.0, -5.0), None);
        assert_eq!(hit_test(&tiles, 900.0, 100.0), None);
    }

    #[test]
    fn metric_switch() {
        let t = sample();
        let b = rect(800.0, 300.0);
        let by_size = layout(&t, 0, Metric::Size, b);
        let by_alloc = layout(&t, 0, Metric::Allocated, b);
        assert!(by_size.iter().any(|x| x.node == 3));
        assert!(by_alloc.iter().all(|x| x.node != 3)); // Small.txt: allocated 0
        let big = |tiles: &[Tile]| tiles.iter().find(|x| x.node == 2).unwrap().rect;
        assert_ne!(big(&by_size), big(&by_alloc));
    }

    #[test]
    fn sub_point_tiles_are_not_emitted() {
        // One big file and 100,000 tiny ones: each tiny one would be far below 1 point.
        let mut sizes = vec![1_000_000_000];
        sizes.extend(std::iter::repeat_n(1, 100_000));
        let t = flat(&sizes);
        let tiles = layout_with(&t, 0, Metric::Size, rect(200.0, 100.0), 0.0, 0.0);
        assert!(tiles.iter().any(|x| x.node == 1), "the big file keeps its tile");
        assert!(tiles.len() < 100, "{} tiles", tiles.len());
        for tile in &tiles {
            assert!(tile.rect.w.min(tile.rect.h) >= MIN_SIDE_PT, "node {}: {:?}", tile.node, tile.rect);
        }
    }

    #[test]
    fn deep_chain_no_stack_overflow() {
        const DEPTH: u32 = 100_000;
        let mut b = TreeBuilder::with_capacity(DEPTH as usize + 1, DEPTH as usize + 8);
        let root = b.push("d", NO_PARENT, 0, 0, 0, F_DIR);
        for i in 1..DEPTH {
            b.push("d", i - 1, 0, 0, 0, F_DIR);
        }
        b.push("f", DEPTH - 1, 1, 1, 0, 0);
        let (t, _) = b.finish(root);
        // No padding: every level keeps the full size, so the whole chain is laid out.
        let tiles = layout_with(&t, root, Metric::Size, rect(1000.0, 1000.0), 0.0, 0.0);
        assert_eq!(tiles.len(), DEPTH as usize + 1);
        assert_eq!(tiles.last().unwrap().depth, 255);
        assert!(tiles.iter().all(|x| !x.elided));
        // With the default padding the chain stops once tiles get too small;
        // the last folder laid out still has content, so it is marked.
        let small = layout(&t, root, Metric::Size, rect(1000.0, 1000.0));
        assert!(small.len() < 1000);
        let last = small.last().unwrap();
        assert!(t.is_dir(last.node) && last.elided);
        assert!(small[..small.len() - 1].iter().all(|x| !x.elided));
    }

    #[test]
    fn folder_of_tiny_files_is_elided() {
        // 100,000 one-byte files in a 300 x 200 panel: each would get about 0.55 points².
        let t = flat(&vec![1; 100_000]);
        let tiles = layout(&t, 0, Metric::Size, rect(300.0, 200.0));
        assert_eq!(tiles.len(), 1, "no child tiles");
        assert_eq!(tiles[0].node, 0);
        assert!(tiles[0].elided);
        for tile in &tiles {
            let r = tile.rect;
            assert!(!(r.x.is_nan() || r.y.is_nan() || r.w.is_nan() || r.h.is_nan()), "{r:?}");
        }
        assert_eq!(hit_test(&tiles, 150.0, 100.0), Some(0));
    }

    #[test]
    fn elided_only_where_content_is_hidden() {
        // Empty folders and folders whose children all have value 0 hide nothing.
        for t in [&flat(&[]), &flat(&[0, 0, 0])] {
            assert!(!layout(t, 0, Metric::Size, rect(500.0, 500.0))[0].elided);
            assert!(!layout(t, 0, Metric::Size, rect(2.0, 2.0))[0].elided);
        }
        // Everything fits: nothing is marked. Files are never marked.
        let t = sample();
        let tiles = layout(&t, 0, Metric::Size, rect(800.0, 300.0));
        assert!(tiles.iter().all(|x| !x.elided), "{tiles:?}");
        // Too small to subdivide, and no area at all.
        assert!(layout(&t, 0, Metric::Size, rect(3.0, 3.0))[0].elided);
        assert!(layout(&t, 0, Metric::Size, rect(0.0, 100.0))[0].elided);
        // A file shown as root (not a folder) is not marked.
        assert!(!layout(&t, 2, Metric::Size, rect(3.0, 3.0))[0].elided);
    }

    #[test]
    fn header_strip_only_on_large_folder_tiles() {
        let t = sample();
        let folder = Tile { node: 1, rect: Rect { x: 10.0, y: 20.0, w: 100.0, h: 50.0 }, depth: 1, elided: false };
        assert_eq!(
            header_strip(&t, &folder),
            Some(Rect { x: 10.0 + PAD_PT, y: 20.0 + PAD_PT, w: 100.0 - 2.0 * PAD_PT, h: HEADER_PT })
        );
        let small = Tile { rect: Rect { x: 0.0, y: 0.0, w: HEADER_MIN_W - 1.0, h: 50.0 }, ..folder };
        assert_eq!(header_strip(&t, &small), None);
        let file = Tile { node: 2, ..folder };
        assert_eq!(header_strip(&t, &file), None);
    }

    #[test]
    fn no_tile_below_the_minimum_side_or_not_finite() {
        let mut sizes: Vec<u64> = (1..=3000).map(|i| (i * 7919) % 10_007 + 1).collect();
        sizes.extend([1_000_000, 3, 3, 3]);
        let t = flat(&sizes);
        for (w, h) in [(800.0, 300.0), (37.0, 1013.0), (1.5, 999.0), (333.3, 77.7)] {
            for tile in layout(&t, 0, Metric::Size, rect(w, h)).iter().skip(1) {
                let r = tile.rect;
                assert!(r.x.is_finite() && r.y.is_finite(), "{r:?}");
                assert!(r.w >= MIN_SIDE_PT && r.h >= MIN_SIDE_PT, "{r:?}");
            }
        }
    }
}
