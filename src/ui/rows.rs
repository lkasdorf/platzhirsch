//! Visible rows of the tree table: expanded folders, sort order, selection
//! and keyboard navigation. Pure logic, no GUI types.
//!
//! The row list is rebuilt only when something changes (expand/collapse,
//! sort, metric, reveal), never per frame. Rebuilding uses an explicit stack,
//! so tree depth never costs call-stack space.

use std::collections::HashMap;

use crate::model::{Metric, NodeId, SortKey, Tree};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Row {
    pub node: NodeId,
    /// Nesting level; saturates at `u16::MAX` for absurdly deep chains.
    pub depth: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
}

pub struct TreeView {
    /// The selected node. Read only: callers must not assign it, but use
    /// `select_row` or `reveal`, which keep the row index in step.
    pub selected: NodeId,
    metric: Metric,
    sort_key: SortKey,
    desc: bool,
    /// Indexed by node id. Kept when a folder is collapsed.
    expanded: Vec<bool>,
    /// Sorted child lists per folder; cleared when the sort order changes.
    sorted: HashMap<NodeId, Vec<NodeId>>,
    rows: Vec<Row>,
    /// Row index of `selected`, kept up to date by `rebuild`.
    selected_row: usize,
    /// The last clicked table header was `% of Parent`; only decides where
    /// the table shows its sort triangle.
    percent_sorted: bool,
}

fn metric_key(metric: Metric) -> SortKey {
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
            selected_row: 0,
            percent_sorted: false,
        };
        view.rebuild(tree);
        view
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn is_expanded(&self, node: NodeId) -> bool {
        self.expanded.get(node as usize).copied().unwrap_or(false)
    }

    /// Expands or collapses a folder. Files and empty folders are left alone.
    pub fn toggle(&mut self, tree: &Tree, node: NodeId) {
        if !tree.is_dir(node) || tree.children(node).is_empty() {
            return;
        }
        if self.is_expanded(node) {
            self.expanded[node as usize] = false;
            if self.is_below(tree, self.selected, node) {
                self.selected = node;
            }
        } else {
            self.expanded[node as usize] = true;
        }
        self.rebuild(tree);
    }

    pub fn sort(&self) -> (SortKey, bool) {
        (self.sort_key, self.desc)
    }

    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool) {
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

    pub fn set_metric(&mut self, tree: &Tree, metric: Metric) {
        self.metric = metric;
        if matches!(self.sort_key, SortKey::Allocated | SortKey::Size) {
            let (key, desc) = (metric_key(metric), self.desc);
            self.set_sort(tree, key, desc);
        }
    }

    /// Expands all ancestors of `node`, selects it and returns its row index.
    pub fn reveal(&mut self, tree: &Tree, node: NodeId) -> usize {
        let mut cur = tree.parent(node);
        while let Some(p) = cur {
            self.expanded[p as usize] = true;
            cur = tree.parent(p);
        }
        self.selected = node;
        self.rebuild(tree);
        self.selected_row
    }

    /// Applies a key to the selection and returns the new selection's row index.
    pub fn key_nav(&mut self, tree: &Tree, key: NavKey) -> usize {
        let sel = self.selected;
        match key {
            NavKey::Up => {
                if self.selected_row > 0 {
                    self.select_row(self.selected_row - 1);
                }
            }
            NavKey::Down => {
                if self.selected_row + 1 < self.rows.len() {
                    self.select_row(self.selected_row + 1);
                }
            }
            NavKey::Left => {
                if tree.is_dir(sel) && self.is_expanded(sel) {
                    self.toggle(tree, sel);
                } else if tree.parent(sel).is_some() {
                    // The parent is the nearest row above with a smaller depth.
                    let depth = self.rows[self.selected_row].depth;
                    if let Some(i) = self.rows[..self.selected_row]
                        .iter()
                        .rposition(|r| r.depth < depth)
                    {
                        self.select_row(i);
                    }
                }
            }
            NavKey::Right => {
                if tree.is_dir(sel) && !tree.children(sel).is_empty() {
                    if !self.is_expanded(sel) {
                        self.toggle(tree, sel);
                    } else if self.selected_row + 1 < self.rows.len() {
                        self.select_row(self.selected_row + 1);
                    }
                }
            }
        }
        self.selected_row
    }

    /// Selects the row at `index`; an index out of range changes nothing.
    pub fn select_row(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index) {
            self.selected = row.node;
            self.selected_row = index;
        }
    }

    /// True if `node` lies strictly below `ancestor`.
    fn is_below(&self, tree: &Tree, node: NodeId, ancestor: NodeId) -> bool {
        let mut cur = tree.parent(node);
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = tree.parent(p);
        }
        false
    }

    fn rebuild(&mut self, tree: &Tree) {
        self.rows.clear();
        let mut found = None;
        let mut stack = vec![(tree.root(), 0usize)];
        while let Some((node, depth)) = stack.pop() {
            if node == self.selected {
                found = Some(self.rows.len());
            }
            self.rows.push(Row {
                node,
                depth: depth.min(u16::MAX as usize) as u16,
            });
            if tree.is_dir(node) && self.expanded[node as usize] {
                let (key, desc) = (self.sort_key, self.desc);
                let kids = self
                    .sorted
                    .entry(node)
                    .or_insert_with(|| tree.sorted_children(node, key, desc));
                stack.extend(kids.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
        match found {
            Some(i) => self.selected_row = i,
            None => {
                // Selection is not reachable from the root (orphan).
                self.selected = tree.root();
                self.selected_row = 0;
            }
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
        v.select_row(5); // top.txt, parent is the root
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
}
