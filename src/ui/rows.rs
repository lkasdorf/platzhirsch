//! Visible rows of the tree table: expanded folders, sort order, selection
//! and keyboard navigation. Pure logic, no GUI types.
//!
//! The row list changes only when something changes (expand/collapse, sort,
//! metric, reveal), never per frame. Expanding inserts the folder's visible
//! descendants after its row and collapsing removes them; a sort or metric
//! change rebuilds the whole list. Both walk the tree with an explicit stack,
//! so tree depth never costs call-stack space.
//!
//! `row_of` maps every node with a row to its row index and every other node
//! to `NOT_SHOWN`. Inserting or removing rows renumbers the rows behind the
//! change, which costs as much as moving them inside the row vector.

use std::collections::HashMap;

use crate::model::{Metric, NodeId, SortKey, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub node: NodeId,
    /// Nesting level; the root is 0.
    pub depth: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
}

/// `row_of` entry of a node without a row.
const NOT_SHOWN: u32 = u32::MAX;

pub struct TreeView {
    /// The selected node. Read only: callers must not assign it, but use
    /// `select_row` or `reveal`. It always has a row.
    pub selected: NodeId,
    metric: Metric,
    sort_key: SortKey,
    desc: bool,
    /// Indexed by node id. Kept when a folder is collapsed.
    expanded: Vec<bool>,
    /// Sorted child lists of folders that have a row; cleared when the sort
    /// order changes, dropped for the folders whose rows a collapse removes.
    sorted: HashMap<NodeId, Vec<NodeId>>,
    rows: Vec<Row>,
    /// Indexed by node id: the row index, or `NOT_SHOWN`.
    row_of: Vec<u32>,
    /// The last clicked table header was `% of Parent`; only decides where
    /// the table shows its sort triangle.
    percent_sorted: bool,
    /// Counts the changes of the row list.
    version: u64,
}

/// The sort key of a metric.
pub(super) fn metric_key(metric: Metric) -> SortKey {
    match metric {
        Metric::Allocated => SortKey::Allocated,
        Metric::Size => SortKey::Size,
    }
}

impl TreeView {
    pub fn new(tree: &Tree, metric: Metric) -> Self {
        let root = tree.root();
        let mut expanded = vec![false; tree.len()];
        expanded[root as usize] = true;
        let mut view = Self {
            selected: root,
            metric,
            sort_key: metric_key(metric),
            desc: true,
            expanded,
            sorted: HashMap::new(),
            rows: Vec::new(),
            row_of: vec![NOT_SHOWN; tree.len()],
            percent_sorted: false,
            version: 0,
        };
        view.rebuild(tree);
        view
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Changes whenever the row list changes.
    pub fn rows_version(&self) -> u64 {
        self.version
    }

    pub fn is_expanded(&self, node: NodeId) -> bool {
        self.expanded.get(node as usize).copied().unwrap_or(false)
    }

    /// The row index of `node`, if it has a row.
    fn row(&self, node: NodeId) -> Option<usize> {
        match self.row_of.get(node as usize) {
            Some(&i) if i != NOT_SHOWN => Some(i as usize),
            _ => None,
        }
    }

    fn selected_row(&self) -> usize {
        self.row(self.selected).unwrap_or(0)
    }

    /// Expands or collapses a folder. Files and empty folders are left alone.
    pub fn toggle(&mut self, tree: &Tree, node: NodeId) {
        if !tree.is_dir(node) || tree.children(node).is_empty() {
            return;
        }
        match self.row(node) {
            Some(at) if self.is_expanded(node) => self.collapse_at(at),
            Some(at) => self.expand_at(tree, at),
            // No row, so nothing below it has one either.
            None => self.expanded[node as usize] = !self.expanded[node as usize],
        }
    }

    /// Collapses the expanded folder in row `at`: removes the rows below it
    /// and the sorted lists of the folders among them. A selection inside
    /// moves to the folder.
    fn collapse_at(&mut self, at: usize) {
        let Row { node, depth } = self.rows[at];
        self.expanded[node as usize] = false;
        let end = self.rows[at + 1..]
            .iter()
            .position(|r| r.depth <= depth)
            .map_or(self.rows.len(), |i| at + 1 + i);
        let sel = self.selected_row();
        if sel > at && sel < end {
            self.selected = node;
        }
        for r in self.rows.drain(at + 1..end) {
            self.row_of[r.node as usize] = NOT_SHOWN;
            self.sorted.remove(&r.node);
        }
        self.renumber_from(at + 1);
    }

    /// Expands the collapsed folder in row `at`: inserts its visible
    /// descendants after it.
    fn expand_at(&mut self, tree: &Tree, at: usize) {
        let Row { node, depth } = self.rows[at];
        self.expanded[node as usize] = true;
        let mut below = Vec::new();
        let (key, desc) = (self.sort_key, self.desc);
        push_descendants(tree, &self.expanded, &mut self.sorted, key, desc, node, depth, &mut below);
        self.rows.splice(at + 1..at + 1, below);
        self.renumber_from(at + 1);
    }

    /// Brings `row_of` in step for the rows from `start` on.
    fn renumber_from(&mut self, start: usize) {
        for (i, r) in self.rows.iter().enumerate().skip(start) {
            self.row_of[r.node as usize] = i as u32;
        }
        self.version = self.version.wrapping_add(1);
    }

    pub fn sort(&self) -> (SortKey, bool) {
        (self.sort_key, self.desc)
    }

    /// Sorts the rows; this also clears the "% of Parent was clicked" flag.
    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool) {
        self.percent_sorted = false;
        self.apply_sort(tree, key, desc);
    }

    fn apply_sort(&mut self, tree: &Tree, key: SortKey, desc: bool) {
        if self.sort_key == key && self.desc == desc {
            return;
        }
        self.sort_key = key;
        self.desc = desc;
        self.sorted.clear();
        self.rebuild(tree);
    }

    /// True if the last clicked table header was `% of Parent`.
    pub fn percent_sorted(&self) -> bool {
        self.percent_sorted
    }

    pub fn set_percent_sorted(&mut self, percent_sorted: bool) {
        self.percent_sorted = percent_sorted;
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Switches the metric; rows sorted by a metric follow it. Keeps the
    /// "% of Parent was clicked" flag.
    pub fn set_metric(&mut self, tree: &Tree, metric: Metric) {
        self.metric = metric;
        if matches!(self.sort_key, SortKey::Allocated | SortKey::Size) {
            let (key, desc) = (metric_key(metric), self.desc);
            self.apply_sort(tree, key, desc);
        }
    }

    /// Expands all ancestors of `node`, selects it and returns its row index.
    /// A node that does not lead up to the root (orphan, parent cycle)
    /// selects the root instead.
    pub fn reveal(&mut self, tree: &Tree, node: NodeId) -> usize {
        let mut ancestors = Vec::new();
        let mut cur = tree.parent(node);
        while let Some(p) = cur {
            if ancestors.len() >= tree.len() {
                break; // parent cycle
            }
            ancestors.push(p);
            cur = tree.parent(p);
        }
        let top = ancestors.last().copied().unwrap_or(node);
        if cur.is_some() || top != tree.root() {
            self.selected = tree.root();
            return 0;
        }
        // The topmost collapsed ancestor has a row, because everything above
        // it is expanded. Expanding it after setting the flags of the ones
        // below inserts all of them at once.
        let mut first_collapsed = None;
        for &a in ancestors.iter().rev() {
            if first_collapsed.is_none() && !self.expanded[a as usize] {
                first_collapsed = Some(a);
            }
            self.expanded[a as usize] = true;
        }
        if let Some(a) = first_collapsed
            && let Some(at) = self.row(a)
        {
            self.expanded[a as usize] = false;
            self.expand_at(tree, at);
        }
        self.selected = if self.row(node).is_some() { node } else { tree.root() };
        self.selected_row()
    }

    /// Applies a key to the selection and returns the new selection's row index.
    pub fn key_nav(&mut self, tree: &Tree, key: NavKey) -> usize {
        let sel = self.selected;
        let sel_row = self.selected_row();
        match key {
            NavKey::Up => {
                if sel_row > 0 {
                    self.select_row(sel_row - 1);
                }
            }
            NavKey::Down => {
                if sel_row + 1 < self.rows.len() {
                    self.select_row(sel_row + 1);
                }
            }
            NavKey::Left => {
                if tree.is_dir(sel) && self.is_expanded(sel) {
                    self.toggle(tree, sel);
                } else if let Some(i) = tree.parent(sel).and_then(|p| self.row(p)) {
                    self.select_row(i);
                }
            }
            NavKey::Right => {
                if tree.is_dir(sel) && !tree.children(sel).is_empty() {
                    if !self.is_expanded(sel) {
                        self.toggle(tree, sel);
                    } else if sel_row + 1 < self.rows.len() {
                        self.select_row(sel_row + 1);
                    }
                }
            }
        }
        self.selected_row()
    }

    /// Selects the row at `index`; an index out of range changes nothing.
    pub fn select_row(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index) {
            self.selected = row.node;
        }
    }

    /// Replaces the row list with a full walk from the root.
    fn rebuild(&mut self, tree: &Tree) {
        for r in &self.rows {
            self.row_of[r.node as usize] = NOT_SHOWN;
        }
        let (key, desc) = (self.sort_key, self.desc);
        self.rows = visible_rows(tree, &self.expanded, &mut self.sorted, key, desc);
        self.renumber_from(0);
        if self.row(self.selected).is_none() {
            self.selected = tree.root();
        }
    }

    /// The row list a full rebuild gives for the current state, without
    /// touching the view.
    #[cfg(test)]
    fn full_rows(&self, tree: &Tree) -> Vec<Row> {
        visible_rows(tree, &self.expanded, &mut HashMap::new(), self.sort_key, self.desc)
    }
}

/// All rows from the root down through the expanded folders.
fn visible_rows(
    tree: &Tree,
    expanded: &[bool],
    sorted: &mut HashMap<NodeId, Vec<NodeId>>,
    key: SortKey,
    desc: bool,
) -> Vec<Row> {
    let root = tree.root();
    let mut rows = vec![Row { node: root, depth: 0 }];
    if tree.is_dir(root) && expanded[root as usize] {
        push_descendants(tree, expanded, sorted, key, desc, root, 0, &mut rows);
    }
    rows
}

/// Appends the visible descendants of the expanded folder `start` (at
/// `depth`) to `out`, in display order. Sorts each folder's children once
/// and keeps the list in `sorted`.
#[allow(clippy::too_many_arguments)]
fn push_descendants(
    tree: &Tree,
    expanded: &[bool],
    sorted: &mut HashMap<NodeId, Vec<NodeId>>,
    key: SortKey,
    desc: bool,
    start: NodeId,
    depth: u32,
    out: &mut Vec<Row>,
) {
    let mut stack: Vec<(NodeId, u32)> = Vec::new();
    let mut push_kids = |stack: &mut Vec<(NodeId, u32)>, node: NodeId, depth: u32| {
        let kids = sorted
            .entry(node)
            .or_insert_with(|| tree.sorted_children(node, key, desc));
        stack.extend(kids.iter().rev().map(|&c| (c, depth + 1)));
    };
    push_kids(&mut stack, start, depth);
    while let Some((node, depth)) = stack.pop() {
        out.push(Row { node, depth });
        if tree.is_dir(node) && expanded[node as usize] {
            push_kids(&mut stack, node, depth);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TreeBuilder, F_DIR, NO_PARENT};

    fn sample() -> Tree {
        let mut b = TreeBuilder::with_capacity(8, 64);
        let root = b.push("C:", NO_PARENT, 0, 0, 10, F_DIR); // 0
        let a = b.push("a", root, 0, 0, 20, F_DIR); // 1
        b.push("big.bin", a, 1000, 4096, 50, 0); // 2
        b.push("Small.txt", a, 10, 0, 30, 0); // 3
        let bd = b.push("b", a, 0, 0, 5, F_DIR); // 4
        b.push("x", bd, 5, 4096, 40, 0); // 5
        b.push("top.txt", root, 1, 4096, 1, 0); // 6
        b.push("lost", 99, 7, 7, 7, 0); // 7: orphan
        b.finish(root).0
    }

    fn nodes(v: &TreeView) -> Vec<NodeId> {
        v.rows().iter().map(|r| r.node).collect()
    }

    #[test]
    fn initial_rows_and_toggle() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        assert_eq!(v.selected, 0);
        assert!(v.sort() == (SortKey::Allocated, true));
        assert_eq!(
            v.rows(),
            &[Row { node: 0, depth: 0 }, Row { node: 1, depth: 1 }, Row { node: 6, depth: 1 }]
        );
        v.toggle(&t, 1);
        assert_eq!(nodes(&v), vec![0, 1, 4, 2, 3, 6]);
        assert_eq!(v.rows()[2].depth, 2);
        v.toggle(&t, 1);
        assert_eq!(v.rows().len(), 3);
        // files and unknown-children nodes do nothing
        v.toggle(&t, 6);
        v.toggle(&t, 5);
        assert_eq!(v.rows().len(), 3);
        assert!(!v.is_expanded(6));
    }

    #[test]
    fn reveal_expands_ancestors() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        assert_eq!(v.reveal(&t, 5), 3);
        assert_eq!(v.selected, 5);
        assert!(v.is_expanded(4) && v.is_expanded(1));
        assert!(!v.is_expanded(5));
        assert_eq!(nodes(&v), vec![0, 1, 4, 5, 2, 3, 6]);
    }

    #[test]
    fn sort_and_metric() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.set_metric(&t, Metric::Size);
        assert!(v.sort() == (SortKey::Size, true));
        assert!(v.metric() == Metric::Size);
        v.set_sort(&t, SortKey::Name, false);
        v.set_metric(&t, Metric::Allocated);
        assert!(v.sort() == (SortKey::Name, false));
        assert!(v.metric() == Metric::Allocated);
    }

    #[test]
    fn sort_change_reorders_rows() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.toggle(&t, 1);
        assert_eq!(nodes(&v), vec![0, 1, 4, 2, 3, 6]);
        v.set_sort(&t, SortKey::Name, true);
        assert_eq!(nodes(&v), vec![0, 1, 4, 3, 2, 6]);
    }

    #[test]
    fn collapse_keeps_descendant_state_and_moves_selection() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.reveal(&t, 5);
        v.toggle(&t, 1); // collapse a while 5 is selected
        assert_eq!(v.selected, 1);
        assert_eq!(nodes(&v), vec![0, 1, 6]);
        v.toggle(&t, 1);
        assert_eq!(nodes(&v), vec![0, 1, 4, 5, 2, 3, 6]); // b stayed expanded
    }

    #[test]
    fn keyboard_navigation() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        assert_eq!(v.key_nav(&t, NavKey::Up), 0); // clamp at top
        assert_eq!(v.key_nav(&t, NavKey::Down), 1);
        assert_eq!(v.selected, 1);
        assert_eq!(v.key_nav(&t, NavKey::Right), 1); // expands
        assert_eq!(v.selected, 1);
        assert!(v.is_expanded(1));
        assert_eq!(v.key_nav(&t, NavKey::Right), 2); // first child
        assert_eq!(v.selected, 4);
        assert_eq!(v.key_nav(&t, NavKey::Left), 1); // 4 is closed -> parent
        assert_eq!(v.selected, 1);
        assert_eq!(v.key_nav(&t, NavKey::Left), 1); // collapses
        assert!(!v.is_expanded(1));
        assert_eq!(v.key_nav(&t, NavKey::Up), 0);
        assert_eq!(v.selected, 0);
        assert_eq!(v.key_nav(&t, NavKey::Left), 0); // collapses the root
        assert_eq!(v.rows().len(), 1);
        assert_eq!(v.key_nav(&t, NavKey::Left), 0); // root: stays
        assert_eq!(v.key_nav(&t, NavKey::Down), 0); // clamp at bottom
        assert_eq!(v.key_nav(&t, NavKey::Right), 0);
        v.key_nav(&t, NavKey::Right);
        v.key_nav(&t, NavKey::Down);
        v.key_nav(&t, NavKey::Down); // top.txt (6) at row 2
        assert_eq!(v.selected, 6);
        assert_eq!(v.key_nav(&t, NavKey::Right), 2); // file: nothing
        assert_eq!(v.key_nav(&t, NavKey::Down), 2);
        assert_eq!(v.key_nav(&t, NavKey::Left), 0); // parent
    }

    #[test]
    fn select_row_then_down_moves_from_that_row() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.toggle(&t, 1); // rows: 0, 1, 4, 2, 3, 6
        v.select_row(4);
        assert_eq!(v.selected, 3);
        assert_eq!(v.key_nav(&t, NavKey::Down), 5);
        assert_eq!(v.selected, v.rows()[5].node);
    }

    #[test]
    fn select_row_on_file_then_left_selects_its_parent() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.toggle(&t, 1); // rows: 0, 1, 4, 2, 3, 6
        v.select_row(3); // big.bin, parent is a
        assert_eq!(v.selected, 2);
        assert_eq!(v.key_nav(&t, NavKey::Left), 1);
        assert_eq!(v.selected, 1);
    }

    #[test]
    fn select_row_out_of_range_changes_nothing() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.key_nav(&t, NavKey::Down);
        v.select_row(usize::MAX);
        assert_eq!(v.selected, 1);
        assert_eq!(v.key_nav(&t, NavKey::Down), 2);
    }

    #[test]
    fn deep_reveal() {
        const DEPTH: u32 = 100_000;
        let mut b = TreeBuilder::with_capacity(DEPTH as usize, DEPTH as usize);
        let root = b.push("d", NO_PARENT, 0, 0, 0, F_DIR);
        for i in 1..DEPTH {
            b.push("d", i - 1, 0, 0, 0, F_DIR);
        }
        let (t, _) = b.finish(root);
        let mut v = TreeView::new(&t, Metric::Size);
        assert_eq!(v.reveal(&t, DEPTH - 1), DEPTH as usize - 1);
        assert_eq!(v.rows().len(), DEPTH as usize);
        assert_eq!(v.selected, DEPTH - 1);
    }

    #[test]
    fn left_selects_the_real_parent_beyond_u16_depth() {
        const DEPTH: u32 = 70_000;
        let mut b = TreeBuilder::with_capacity(DEPTH as usize, DEPTH as usize);
        let root = b.push("d", NO_PARENT, 0, 0, 0, F_DIR);
        for i in 1..DEPTH {
            b.push("d", i - 1, 0, 0, 0, F_DIR);
        }
        let (t, _) = b.finish(root);
        let mut v = TreeView::new(&t, Metric::Size);
        v.reveal(&t, DEPTH - 1);
        assert_eq!(v.key_nav(&t, NavKey::Left), DEPTH as usize - 2);
        assert_eq!(v.selected, DEPTH - 2);
    }

    #[test]
    fn reveal_inside_a_parent_cycle_falls_back_to_the_root() {
        let mut b = TreeBuilder::with_capacity(4, 16);
        let root = b.push("C:", NO_PARENT, 0, 0, 0, F_DIR); // 0
        b.push("a", 2, 0, 0, 0, F_DIR); // 1: parent is 2
        b.push("b", 1, 0, 0, 0, F_DIR); // 2: parent is 1
        b.push("f", root, 1, 1, 0, 0); // 3
        let (t, stats) = b.finish(root);
        assert_eq!(stats.orphaned, 2);
        assert_eq!(t.parent(1), Some(2));
        assert_eq!(t.parent(2), Some(1));
        let mut v = TreeView::new(&t, Metric::Allocated);
        assert_eq!(v.reveal(&t, 1), 0);
        assert_eq!(v.selected, 0);
        assert_eq!(nodes(&v), vec![0, 3]);
    }

    #[test]
    fn set_sort_clears_the_percent_flag() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.set_percent_sorted(true);
        v.set_sort(&t, SortKey::Name, false);
        assert!(!v.percent_sorted());
    }

    #[test]
    fn set_metric_keeps_the_percent_flag() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.set_percent_sorted(true);
        v.set_metric(&t, Metric::Size);
        assert!(v.sort() == (SortKey::Size, true));
        assert!(v.percent_sorted());
    }

    /// Root with `FOLDERS` folders; each holds `SUB` folders with files and
    /// one more folder level, so collapsing a top folder removes many rows.
    fn wide_tree() -> Tree {
        const FOLDERS: u32 = 4;
        const SUB: u32 = 12;
        let mut b = TreeBuilder::with_capacity(1024, 8192);
        let root = b.push("R:", NO_PARENT, 0, 0, 0, F_DIR);
        let mut k = 1u64;
        let mut next = || {
            k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (k >> 33) % 1000
        };
        for f in 0..FOLDERS {
            let top = b.push(&format!("top{f}"), root, 0, 0, 0, F_DIR);
            for s in 0..SUB {
                let sub = b.push(&format!("sub{s}"), top, 0, 0, 0, F_DIR);
                for i in 0..3 {
                    let (size, time) = (next(), next() as i64);
                    b.push(&format!("file{i}"), sub, size, size * 4, time, 0);
                }
                let deep = b.push("deep", sub, 0, 0, 0, F_DIR);
                let size = next();
                b.push("leaf", deep, size, size, 0, 0);
            }
            b.push(&format!("loose{f}"), top, next(), 0, 0, 0);
        }
        b.finish(root).0
    }

    fn is_below(t: &Tree, node: NodeId, ancestor: NodeId) -> bool {
        let mut cur = t.parent(node);
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = t.parent(p);
        }
        false
    }

    /// The row list equals a full rebuild, `row_of` matches it exactly, the
    /// selection has a row, and only folders with a row keep a sorted list.
    fn check(v: &TreeView, t: &Tree, step: &str) {
        assert_eq!(v.rows(), v.full_rows(t).as_slice(), "rows after {step}");
        for (i, r) in v.rows().iter().enumerate() {
            assert_eq!(v.row_of[r.node as usize], i as u32, "row_of after {step}");
        }
        let shown = v.row_of.iter().filter(|&&i| i != NOT_SHOWN).count();
        assert_eq!(shown, v.rows().len(), "stale row_of entries after {step}");
        assert!(v.row(v.selected).is_some(), "selection without a row after {step}");
        for node in v.sorted.keys() {
            assert!(v.row(*node).is_some(), "sorted list of a hidden folder after {step}");
        }
    }

    #[test]
    fn in_place_toggles_match_a_full_rebuild() {
        let t = wide_tree();
        let mut v = TreeView::new(&t, Metric::Allocated);
        check(&v, &t, "new");

        // Expand everything below the second top folder, select a deep leaf.
        let top1 = t.child_by_name(t.root(), "top1").unwrap();
        let sub5 = t.child_by_name(top1, "sub5").unwrap();
        let deep = t.child_by_name(sub5, "deep").unwrap();
        let leaf = t.child_by_name(deep, "leaf").unwrap();
        v.reveal(&t, leaf);
        check(&v, &t, "reveal leaf");
        for &s in t.children(top1) {
            if t.is_dir(s) && !v.is_expanded(s) {
                v.toggle(&t, s);
                check(&v, &t, "expand sub");
            }
        }
        assert_eq!(v.selected, leaf);

        // Collapse the folder with many children while the deep row is selected.
        v.toggle(&t, top1);
        check(&v, &t, "collapse top1");
        assert_eq!(v.selected, top1);
        v.toggle(&t, top1);
        check(&v, &t, "expand top1 again");
        assert!(v.is_expanded(sub5) && v.is_expanded(deep));

        // A collapse above an unrelated selection leaves it alone.
        let top3 = t.child_by_name(t.root(), "top3").unwrap();
        v.reveal(&t, top3);
        check(&v, &t, "reveal top3");
        v.toggle(&t, top1);
        check(&v, &t, "collapse top1 with top3 selected");
        assert_eq!(v.selected, top3);

        // A scripted mix of toggles, selections, keys and sorts.
        let mut k = 7u64;
        let mut next = |n: usize| {
            k = k.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((k >> 33) as usize) % n
        };
        let sorts = [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Allocated];
        for step in 0..600 {
            let before = v.selected;
            match next(10) {
                0..=3 => {
                    let node = v.rows()[next(v.rows().len())].node;
                    let collapsing = t.is_dir(node) && v.is_expanded(node) && !t.children(node).is_empty();
                    v.toggle(&t, node);
                    if collapsing && is_below(&t, before, node) {
                        assert_eq!(v.selected, node, "step {step}: selection moves to the collapsed folder");
                    } else {
                        assert_eq!(v.selected, before, "step {step}: selection stays");
                    }
                }
                4 => v.select_row(next(v.rows().len())),
                5 => {
                    let keys = [NavKey::Up, NavKey::Down, NavKey::Left, NavKey::Right];
                    let row = v.key_nav(&t, keys[next(4)]);
                    assert_eq!(v.rows()[row].node, v.selected);
                }
                6 => v.set_sort(&t, sorts[next(sorts.len())], next(2) == 0),
                7 => v.set_metric(&t, if next(2) == 0 { Metric::Size } else { Metric::Allocated }),
                _ => {
                    let node = next(t.len()) as NodeId;
                    let row = v.reveal(&t, node);
                    assert_eq!(v.rows()[row].node, v.selected);
                }
            }
            check(&v, &t, &format!("step {step}"));
        }
    }

    #[test]
    fn collapse_drops_the_sorted_lists_below() {
        let t = sample();
        let mut v = TreeView::new(&t, Metric::Allocated);
        v.reveal(&t, 5);
        assert!(v.sorted.contains_key(&4));
        v.toggle(&t, 1);
        assert!(!v.sorted.contains_key(&4));
        assert!(v.sorted.contains_key(&1));
    }
}
