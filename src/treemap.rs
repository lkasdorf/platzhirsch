//! Squarified treemap layout (Bruls, Huizing, van Wijk).
//!
//! Pure geometry: turns one folder of the in-memory tree into nested
//! rectangles and answers which rectangle lies under a point. Nothing in
//! here recurses over tree depth; pending folders wait in a queue, the one
//! with the largest tile first.
//! All lengths are in egui points (logical pixels), not physical pixels.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

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
    /// A folder of whose children of non-zero value only some got a tile.
    /// Where the others would be, its [`body`] shows no child.
    pub partial: bool,
    /// Children are missing (`elided` or `partial`) because the limit on the
    /// number of tiles was reached, not only because they are too small.
    pub capped: bool,
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
/// A layout has at most this many tiles: far more than a real drive needs on
/// a large screen, and a bound on the time and memory of the rare tree with
/// millions of entries of similar size.
pub const MAX_TILES: usize = 300_000;

pub fn layout(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect) -> Vec<Tile> {
    layout_with(tree, root, metric, bounds, HEADER_PT, PAD_PT, MAX_TILES)
}

/// Squarified layout. Order: parents before children; the first tile is
/// `root` itself (depth 0, all of `bounds`). Entries with value 0 get no tile,
/// nor do entries whose tile would be thinner than [`MIN_SIDE_PT`]. A folder
/// tile none of whose children of non-zero value got a tile is marked
/// [`Tile::elided`], one of which only some did [`Tile::partial`].
///
/// At most `max_tiles` tiles are laid out (the tile of `root` always is).
/// The folder with the largest tile is subdivided first, and in a folder the
/// largest entries get their tiles first, so the limit leaves out the
/// smallest. Folders it cuts short are marked [`Tile::capped`] as well.
pub fn layout_with(
    tree: &Tree,
    root: NodeId,
    metric: Metric,
    bounds: Rect,
    header_pt: f32,
    pad_pt: f32,
    max_tiles: usize,
) -> Vec<Tile> {
    let mut out = vec![Tile { node: root, rect: bounds, depth: 0, elided: false, partial: false, capped: false }];
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

    // Indices into `out` of the folder tiles still to subdivide, the largest
    // tile first.
    let mut queue = BinaryHeap::new();
    queue.push(queued(bounds, 0));
    let mut kids: Vec<(NodeId, u64)> = Vec::new();
    while let Some((_, Reverse(at))) = queue.pop() {
        let p = out[at];
        if !tree.is_dir(p.node) {
            continue;
        }
        if out.len() >= max_tiles {
            // No tile left for any child: the folder stays whole.
            let hides = has_valued_child(tree, p.node, metric);
            out[at].elided = hides;
            out[at].capped = hides;
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
        let area = body_of(p.rect, header, pad);
        let (placed, capped) = if p.rect.w < MIN_TILE_PT || p.rect.h < MIN_TILE_PT || !(area.w > 0.0 && area.h > 0.0) {
            (0, false)
        } else {
            kids.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            squarify(tree, &kids, total, area, p.depth.saturating_add(1), max_tiles, &mut out, &mut queue)
        };
        out[at].elided = placed == 0;
        out[at].partial = placed > 0 && placed < kids.len();
        out[at].capped = capped;
    }
    out
}

fn has_valued_child(tree: &Tree, node: NodeId, metric: Metric) -> bool {
    tree.is_dir(node) && tree.children(node).iter().any(|&c| tree.metric(c, metric) > 0)
}

/// The queue entry of the folder tile `rect` at index `at` of the output:
/// the larger area comes first, among equal areas the earlier tile. The bits
/// of a positive float order like its value.
fn queued(rect: Rect, at: usize) -> (u32, Reverse<usize>) {
    let area = rect.w * rect.h;
    (if area.is_finite() && area > 0.0 { area.to_bits() } else { 0 }, Reverse(at))
}

/// The part of the folder tile `rect` its children are laid out in: the tile
/// minus `pad` on every side and, where a header strip is reserved, minus
/// `header` at the top.
fn body_of(rect: Rect, header: f64, pad: f64) -> F64Rect {
    let mut area = F64Rect {
        x: f64::from(rect.x) + pad,
        y: f64::from(rect.y) + pad,
        w: f64::from(rect.w) - 2.0 * pad,
        h: f64::from(rect.h) - 2.0 * pad,
    };
    if rect.w >= HEADER_MIN_W && rect.h >= HEADER_MIN_H {
        area.y += header;
        area.h -= header;
    }
    area
}

/// The part of a folder tile that [`layout`] places its children in. Where a
/// [`Tile::partial`] folder shows no child there, entries are left out.
pub fn body(tile: &Tile) -> Rect {
    let r = body_of(tile.rect, f64::from(HEADER_PT), f64::from(PAD_PT));
    Rect { x: r.x as f32, y: r.y as f32, w: r.w as f32, h: r.h as f32 }
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
/// row. Every placed tile goes to `out`; folders are put in `queue` by their
/// index in `out`. Stops when `out` holds `max_tiles` tiles. Returns how many
/// tiles were placed, and whether that limit kept a tile from being placed.
#[allow(clippy::too_many_arguments)]
fn squarify(
    tree: &Tree,
    kids: &[(NodeId, u64)],
    total: u128,
    area: F64Rect,
    depth: u8,
    max_tiles: usize,
    out: &mut Vec<Tile>,
    queue: &mut BinaryHeap<(u32, Reverse<usize>)>,
) -> (usize, bool) {
    let placed_before = out.len();
    let F64Rect { mut x, mut y, mut w, mut h } = area;
    let mut remaining = total;
    let mut i = 0;
    let mut capped = false;
    'rows: while i < kids.len() && w > 0.0 && h > 0.0 {
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
            if out.len() >= max_tiles {
                capped = true;
                break 'rows;
            }
            if tree.is_dir(node) {
                queue.push(queued(rect, out.len()));
            }
            out.push(Tile { node, rect, depth, elided: false, partial: false, capped: false });
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
    (out.len() - placed_before, capped)
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
        let tiles = layout_with(&t, 0, Metric::Size, b, 0.0, 0.0, MAX_TILES);
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
        let tiles = layout_with(&t, 0, Metric::Size, rect(200.0, 100.0), 0.0, 0.0, MAX_TILES);
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
        let tiles = layout_with(&t, root, Metric::Size, rect(1000.0, 1000.0), 0.0, 0.0, MAX_TILES);
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
        // Every folder shows at least some of its content: none is marked. Files never are.
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
    fn folder_with_some_entries_too_small_is_partial() {
        // Half the space in one file, half in 200,000 files far below 1 point each.
        let mut sizes = vec![200_000];
        sizes.extend(std::iter::repeat_n(1, 200_000));
        let t = flat(&sizes);
        let tiles = layout(&t, 0, Metric::Size, rect(800.0, 300.0));
        assert_eq!(tiles.len(), 2, "the folder and the big file");
        assert!(tiles[0].partial && !tiles[0].elided && !tiles[0].capped, "{:?}", tiles[0]);
        assert!(!tiles[1].partial);
        // The big file takes about half of the body; the rest of it shows no child.
        let (file, all) = (tiles[1].rect, body(&tiles[0]));
        let share = file.w * file.h / (all.w * all.h);
        assert!((share - 0.5).abs() < 0.01, "{share}");
        // Everything drawn: neither mark. Nothing drawn: elided, not partial.
        let whole = layout(&sample(), 1, Metric::Size, rect(800.0, 300.0));
        assert_eq!(whole.len(), 5);
        assert!(whole.iter().all(|x| !x.partial && !x.elided && !x.capped), "{whole:?}");
        // In the sample's root `top.txt` would be a strip below one point wide.
        let root = layout(&sample(), 0, Metric::Size, rect(800.0, 300.0));
        assert!(root[0].partial && root.iter().all(|x| x.node != 6));
        let none = layout(&flat(&vec![1; 100_000]), 0, Metric::Size, rect(300.0, 200.0));
        assert!(none[0].elided && !none[0].partial && !none[0].capped);
    }

    #[test]
    fn body_is_where_the_children_are_laid_out() {
        let t = sample();
        let tile = |w, h| Tile { node: 1, rect: Rect { x: 10.0, y: 20.0, w, h }, depth: 1, elided: false, partial: false, capped: false };
        // With a header strip, and too small for one.
        for folder in [tile(100.0, 50.0), tile(HEADER_MIN_W - 1.0, 50.0), tile(100.0, HEADER_MIN_H - 1.0)] {
            assert_eq!(body(&folder), content(folder.rect));
            assert_eq!(header_strip(&t, &folder).is_some(), body(&folder).y > folder.rect.y + PAD_PT);
        }
    }

    #[test]
    fn the_tile_limit_keeps_the_largest_entries_of_a_folder() {
        // 100 files of sizes 1..=100, all large enough for a tile.
        let sizes: Vec<u64> = (1..=100).collect();
        let t = flat(&sizes);
        let all = layout_with(&t, 0, Metric::Size, rect(800.0, 600.0), 0.0, 0.0, MAX_TILES);
        assert_eq!(all.len(), 101);
        assert!(!all[0].partial && !all[0].capped);
        let some = layout_with(&t, 0, Metric::Size, rect(800.0, 600.0), 0.0, 0.0, 11);
        assert_eq!(some.len(), 11);
        assert!(some[0].partial && some[0].capped && !some[0].elided, "{:?}", some[0]);
        // Node `i` has size `i`: the ten tiles are the ten largest files, placed as without a limit.
        let mut nodes: Vec<NodeId> = some[1..].iter().map(|x| x.node).collect();
        nodes.sort_unstable();
        assert_eq!(nodes, (91..=100).collect::<Vec<NodeId>>());
        for tile in &some[1..] {
            assert_eq!(Some(tile), all.iter().find(|x| x.node == tile.node));
        }
        // A limit that is exactly enough cuts nothing short.
        let exact = layout_with(&t, 0, Metric::Size, rect(800.0, 600.0), 0.0, 0.0, 101);
        assert_eq!(exact, all);
        // The tile of the shown folder is always there.
        for limit in [0, 1] {
            let alone = layout_with(&t, 0, Metric::Size, rect(800.0, 600.0), 0.0, 0.0, limit);
            assert_eq!(alone.len(), 1);
            assert!(alone[0].elided && alone[0].capped && !alone[0].partial);
        }
    }

    #[test]
    fn the_tile_limit_subdivides_the_largest_folders_first() {
        // Three folders of 50 equal files each; the folders hold 3/6, 2/6 and 1/6 of the space.
        let mut b = TreeBuilder::with_capacity(160, 64);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR); // 0
        let mut folders = Vec::new();
        for size in [1u64, 3, 2] {
            let d = b.push("d", root, 0, 0, 0, F_DIR);
            for _ in 0..50 {
                b.push("f", d, size, size, 0, 0);
            }
            folders.push(d);
        }
        let empty = b.push("empty", root, 0, 0, 0, F_DIR);
        let t = b.finish(root).0;
        let (small, large, middle) = (folders[0], folders[1], folders[2]);
        let bounds = rect(900.0, 600.0);
        let all = layout(&t, 0, Metric::Size, bounds);
        assert_eq!(all.len(), 1 + 3 + 150);
        assert!(all.iter().all(|x| !x.elided && !x.partial && !x.capped));

        // Room for the three folders, all of the largest one and 20 files of the next.
        let tiles = layout_with(&t, 0, Metric::Size, bounds, HEADER_PT, PAD_PT, 1 + 3 + 50 + 20);
        assert_eq!(tiles.len(), 74);
        let of = |node| *tiles.iter().find(|x| x.node == node).unwrap();
        let drawn = |dir| tiles.iter().filter(|x| t.parent(x.node) == Some(dir)).count();
        assert_eq!((drawn(large), drawn(middle), drawn(small)), (50, 20, 0));
        assert!(!of(large).elided && !of(large).partial && !of(large).capped);
        assert!(of(middle).partial && of(middle).capped && !of(middle).elided);
        assert!(of(small).elided && of(small).capped && !of(small).partial);
        // The root shows all its folders with content; the empty one hides nothing.
        assert!(!tiles[0].elided && !tiles[0].partial && !tiles[0].capped);
        assert!(tiles.iter().all(|x| x.node != empty));
        // Parents still come before their children, so the deepest tile is the last one hit.
        for (i, tile) in tiles.iter().enumerate().skip(1) {
            let parent = t.parent(tile.node).unwrap();
            assert!(tiles[..i].iter().any(|x| x.node == parent), "node {} before its parent", tile.node);
        }
        let file = tiles.iter().find(|x| t.parent(x.node) == Some(middle)).unwrap();
        assert_eq!(hit_test(&tiles, file.rect.x + file.rect.w / 2.0, file.rect.y + file.rect.h / 2.0), Some(file.node));
    }

    #[test]
    fn no_layout_has_more_tiles_than_the_limit() {
        // 400,000 equal files on a panel where each would get a tile of 5 points².
        let t = flat(&vec![1; 400_000]);
        let tiles = layout(&t, 0, Metric::Size, rect(2000.0, 1000.0));
        assert_eq!(tiles.len(), MAX_TILES);
        assert!(tiles[0].partial && tiles[0].capped);
    }

    #[test]
    fn header_strip_only_on_large_folder_tiles() {
        let t = sample();
        let folder = Tile { node: 1, rect: Rect { x: 10.0, y: 20.0, w: 100.0, h: 50.0 }, depth: 1, elided: false, partial: false, capped: false };
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
