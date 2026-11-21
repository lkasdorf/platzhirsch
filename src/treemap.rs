//! Squarified treemap layout (Bruls, Huizing, van Wijk).
//!
//! Pure geometry: turns one folder of the in-memory tree into nested
//! rectangles and answers which rectangle lies under a point. Nothing in
//! here recurses over tree depth; pending folders live on an explicit stack.

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
}

/// Tiles narrower or lower than this are not subdivided further.
pub const MIN_TILE_PX: f32 = 4.0;
/// Space for the folder name, reserved when the tile is at least
/// `HEADER_MIN_W` x `HEADER_MIN_H` px.
pub const HEADER_PX: f32 = 14.0;
pub const HEADER_MIN_W: f32 = 40.0;
pub const HEADER_MIN_H: f32 = 28.0;
pub const PAD_PX: f32 = 1.0;
/// Tiles whose smaller side is below this are not emitted; their parent's
/// area stays unsubdivided there.
pub const MIN_SIDE_PX: f32 = 1.0;

pub fn layout(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect) -> Vec<Tile> {
    layout_with(tree, root, metric, bounds, HEADER_PX, PAD_PX)
}

/// Squarified layout. Order: parents before children; the first tile is
/// `root` itself (depth 0, all of `bounds`). Entries with value 0 get no tile,
/// nor do entries whose tile would be thinner than [`MIN_SIDE_PX`].
pub fn layout_with(
    tree: &Tree,
    root: NodeId,
    metric: Metric,
    bounds: Rect,
    header_px: f32,
    pad_px: f32,
) -> Vec<Tile> {
    let mut out = vec![Tile { node: root, rect: bounds, depth: 0 }];
    let finite = bounds.x.is_finite()
        && bounds.y.is_finite()
        && bounds.w.is_finite()
        && bounds.h.is_finite();
    if !finite || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return out;
    }
    let sane = |v: f32| if v.is_finite() { f64::from(v.max(0.0)) } else { 0.0 };
    let (header, pad) = (sane(header_px), sane(pad_px));

    let mut stack = vec![Tile { node: root, rect: bounds, depth: 0 }];
    let mut kids: Vec<(NodeId, u64)> = Vec::new();
    while let Some(p) = stack.pop() {
        if !tree.is_dir(p.node) || p.rect.w < MIN_TILE_PX || p.rect.h < MIN_TILE_PX {
            continue;
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
        if !(area.w > 0.0 && area.h > 0.0) {
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
            continue;
        }
        kids.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        squarify(tree, &kids, total, area, p.depth.saturating_add(1), &mut out, &mut stack);
    }
    out
}

#[derive(Clone, Copy)]
struct F64Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Places `kids` (sorted by value, descending, all > 0) into `area` row by
/// row. Every placed tile goes to `out`; folders are queued on `stack`.
fn squarify(
    tree: &Tree,
    kids: &[(NodeId, u64)],
    total: u128,
    area: F64Rect,
    depth: u8,
    out: &mut Vec<Tile>,
    stack: &mut Vec<Tile>,
) {
    let F64Rect { mut x, mut y, mut w, mut h } = area;
    let mut remaining = total;
    let mut i = 0;
    while i < kids.len() && w > 0.0 && h > 0.0 {
        // Every tile placed in the rest of the area is at most `min(w, h)`
        // across, so once that is below the cut-off no further tile is emitted.
        if w.min(h) < f64::from(MIN_SIDE_PX) {
            break;
        }
        let scale = w * h / remaining as f64; // pixels² per unit of value
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
                row_len - offset
            } else {
                row_len * (value as f64 / row_value as f64)
            };
            let r = if wide {
                F64Rect { x, y: y + offset, w: thick, h: len }
            } else {
                F64Rect { x: x + offset, y, w: len, h: thick }
            };
            offset += len;
            if r.w.min(r.h) < f64::from(MIN_SIDE_PX) {
                continue;
            }
            let tile = Tile {
                node,
                rect: Rect { x: r.x as f32, y: r.y as f32, w: r.w as f32, h: r.h as f32 },
                depth,
            };
            out.push(tile);
            if tree.is_dir(node) {
                stack.push(tile);
            }
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
}

/// The deepest tile under the point. Parents precede their children in
/// `tiles` and siblings are disjoint, so this is the last tile containing it.
pub fn hit_test(tiles: &[Tile], x: f32, y: f32) -> Option<NodeId> {
    tiles
        .iter()
        .rev()
        .find(|t| x >= t.rect.x && x < t.rect.x + t.rect.w && y >= t.rect.y && y < t.rect.y + t.rect.h)
        .map(|t| t.node)
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

    #[test]
    fn tiles_inside_parent_and_disjoint() {
        let t = sample();
        let tiles = layout(&t, 0, Metric::Size, rect(800.0, 300.0));
        assert!(tiles.len() > 4);
        let by_node: HashMap<NodeId, Rect> = tiles.iter().map(|x| (x.node, x.rect)).collect();
        for tile in &tiles[1..] {
            let parent = t.parent(tile.node).unwrap();
            assert!(inside(tile.rect, by_node[&parent]), "node {} outside parent", tile.node);
        }
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
    fn sub_pixel_tiles_are_not_emitted() {
        // One big file and 100,000 tiny ones: each tiny one would be far below 1 px.
        let mut sizes = vec![1_000_000_000];
        sizes.extend(std::iter::repeat_n(1, 100_000));
        let t = flat(&sizes);
        let tiles = layout_with(&t, 0, Metric::Size, rect(200.0, 100.0), 0.0, 0.0);
        assert!(tiles.iter().any(|x| x.node == 1), "the big file keeps its tile");
        assert!(tiles.len() < 100, "{} tiles", tiles.len());
        for tile in &tiles {
            assert!(tile.rect.w.min(tile.rect.h) >= MIN_SIDE_PX, "node {}: {:?}", tile.node, tile.rect);
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
        // With the default padding the chain stops once tiles get too small.
        let small = layout(&t, root, Metric::Size, rect(1000.0, 1000.0));
        assert!(small.len() < 1000);
    }
}
