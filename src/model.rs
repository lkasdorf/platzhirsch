//! In-memory tree filled by the scanners and read by the GUI.
//!
//! Struct-of-arrays layout with one shared name buffer. Nothing in here
//! recurses over tree depth: reachability is a breadth-first search and
//! the aggregates are summed in reverse BFS order.

use std::cmp::Ordering;

pub type NodeId = u32;
pub const NO_PARENT: NodeId = u32::MAX;
pub const F_DIR: u8 = 1;
pub const F_UNREADABLE: u8 = 2;
pub const F_LINK: u8 = 4;
pub const F_ONLINE_ONLY: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Allocated,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Allocated,
    Size,
    Files,
    Dirs,
    Modified,
}

pub struct BuildStats {
    /// Entries not reachable from the root (missing parent, cycle).
    pub orphaned: u64,
}

pub struct TreeBuilder {
    names: String,
    name_start: Vec<u32>,
    parent: Vec<NodeId>,
    size: Vec<u64>,
    allocated: Vec<u64>,
    mtime: Vec<i64>,
    flags: Vec<u8>,
}

impl TreeBuilder {
    pub fn with_capacity(entries: usize, name_bytes: usize) -> Self {
        Self {
            names: String::with_capacity(name_bytes),
            name_start: Vec::with_capacity(entries + 1),
            parent: Vec::with_capacity(entries),
            size: Vec::with_capacity(entries),
            allocated: Vec::with_capacity(entries),
            mtime: Vec::with_capacity(entries),
            flags: Vec::with_capacity(entries),
        }
    }

    /// Ids are assigned in call order (0, 1, 2 ...).
    /// `parent` may point to an id that is pushed later.
    pub fn push(
        &mut self,
        name: &str,
        parent: NodeId,
        size: u64,
        allocated: u64,
        mtime: i64,
        flags: u8,
    ) -> NodeId {
        let id = self.parent.len();
        assert!(id < NO_PARENT as usize, "too many entries");
        self.name_start
            .push(u32::try_from(self.names.len()).expect("name buffer exceeds 4 GiB"));
        self.names.push_str(name);
        self.parent.push(parent);
        self.size.push(size);
        self.allocated.push(allocated);
        self.mtime.push(mtime);
        self.flags.push(flags);
        id as NodeId
    }

    /// Sets extra flag bits on an entry pushed earlier (e.g. `F_UNREADABLE`
    /// once its listing has failed).
    pub fn add_flags(&mut self, id: NodeId, flags: u8) {
        self.flags[id as usize] |= flags;
    }

    pub fn finish(self, root: NodeId) -> (Tree, BuildStats) {
        let Self {
            names,
            mut name_start,
            mut parent,
            mut size,
            mut allocated,
            mut mtime,
            flags,
        } = self;
        let n = parent.len();
        assert!((root as usize) < n, "root id out of range");
        name_start.push(u32::try_from(names.len()).expect("name buffer exceeds 4 GiB"));

        // A parent is only valid if it exists and is a directory. Anything
        // else (and the root itself) is detached and can only stay unreachable.
        for (i, p) in parent.iter_mut().enumerate() {
            let valid = (*p as usize) < n && *p as usize != i && flags[*p as usize] & F_DIR != 0;
            if !valid || i == root as usize {
                *p = NO_PARENT;
            }
        }

        // Counting sort of all attached entries by parent id.
        // Afterwards `first[p]..first[p + 1]` indexes the children of `p` in `by_parent`.
        let mut first = vec![0u32; n + 1];
        for &p in &parent {
            if p != NO_PARENT {
                first[p as usize] += 1;
            }
        }
        for i in 1..=n {
            first[i] += first[i - 1];
        }
        let mut by_parent = vec![0u32; first[n] as usize];
        for i in (0..n).rev() {
            let p = parent[i];
            if p != NO_PARENT {
                first[p as usize] -= 1;
                by_parent[first[p as usize] as usize] = i as NodeId;
            }
        }
        // `first` now holds the start offsets; `first[n]` is still the total
        // because no entry has parent `n`.

        // Breadth-first search from the root. Children of a directory are
        // appended together, so they are contiguous in `order`.
        let mut order: Vec<NodeId> = Vec::with_capacity(n);
        let mut child_start = vec![0u32; n];
        let mut child_len = vec![0u32; n];
        order.push(root);
        let mut head = 0;
        while head < order.len() {
            let u = order[head] as usize;
            head += 1;
            let kids = &by_parent[first[u] as usize..first[u + 1] as usize];
            child_start[u] = order.len() as u32;
            child_len[u] = kids.len() as u32;
            order.extend_from_slice(kids);
        }
        drop(by_parent);
        drop(first);
        let orphaned = (n - order.len()) as u64;

        // Subtree sums in reverse BFS order: every child is final before its parent.
        let mut file_count = vec![0u32; n];
        let mut dir_count = vec![0u32; n];
        for &u in order[1..].iter().rev() {
            let (u, p) = (u as usize, parent[u as usize] as usize);
            size[p] = size[p].saturating_add(size[u]);
            allocated[p] = allocated[p].saturating_add(allocated[u]);
            mtime[p] = mtime[p].max(mtime[u]);
            if flags[u] & F_DIR != 0 {
                file_count[p] += file_count[u];
                dir_count[p] += dir_count[u] + 1;
            } else {
                file_count[p] += 1;
            }
        }

        let tree = Tree {
            root,
            names,
            name_start,
            parent,
            size,
            allocated,
            mtime,
            flags,
            file_count,
            dir_count,
            child_start,
            child_len,
            order,
        };
        (tree, BuildStats { orphaned })
    }
}

pub struct Tree {
    root: NodeId,
    names: String,
    /// `len + 1` entries; the last one is the end of the name buffer.
    name_start: Vec<u32>,
    parent: Vec<NodeId>,
    size: Vec<u64>,
    allocated: Vec<u64>,
    mtime: Vec<i64>,
    flags: Vec<u8>,
    file_count: Vec<u32>,
    dir_count: Vec<u32>,
    /// Position of a directory's children inside `order`.
    child_start: Vec<u32>,
    child_len: Vec<u32>,
    /// BFS order from the root. Children of a directory are contiguous.
    order: Vec<NodeId>,
}

impl Tree {
    pub fn root(&self) -> NodeId {
        self.root
    }
    pub fn len(&self) -> usize {
        self.parent.len()
    }
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }
    pub fn name(&self, id: NodeId) -> &str {
        let i = id as usize;
        &self.names[self.name_start[i] as usize..self.name_start[i + 1] as usize]
    }
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        match self.parent[id as usize] {
            NO_PARENT => None,
            p => Some(p),
        }
    }
    pub fn flags(&self, id: NodeId) -> u8 {
        self.flags[id as usize]
    }
    pub fn is_dir(&self, id: NodeId) -> bool {
        self.flags(id) & F_DIR != 0
    }
    pub fn size(&self, id: NodeId) -> u64 {
        self.size[id as usize]
    }
    pub fn allocated(&self, id: NodeId) -> u64 {
        self.allocated[id as usize]
    }
    pub fn metric(&self, id: NodeId, m: Metric) -> u64 {
        match m {
            Metric::Allocated => self.allocated(id),
            Metric::Size => self.size(id),
        }
    }
    pub fn file_count(&self, id: NodeId) -> u32 {
        self.file_count[id as usize]
    }
    pub fn dir_count(&self, id: NodeId) -> u32 {
        self.dir_count[id as usize]
    }
    pub fn mtime(&self, id: NodeId) -> i64 {
        self.mtime[id as usize]
    }
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let start = self.child_start[id as usize] as usize;
        &self.order[start..start + self.child_len[id as usize] as usize]
    }

    /// Names from the root joined with `\`. Walks parent links iteratively;
    /// the step limit keeps entries inside a parent cycle from looping.
    pub fn path(&self, id: NodeId) -> String {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if chain.len() >= self.len() {
                break;
            }
            chain.push(c);
            cur = self.parent(c);
        }
        let mut out = String::new();
        for (i, &c) in chain.iter().rev().enumerate() {
            if i > 0 {
                out.push('\\');
            }
            out.push_str(self.name(c));
        }
        out
    }

    /// Directories first, then files; each group ordered by `key`
    /// (`desc` reverses it). Ties are broken by name, case-insensitive, ascending.
    pub fn sorted_children(&self, id: NodeId, key: SortKey, desc: bool) -> Vec<NodeId> {
        let mut out = self.children(id).to_vec();
        out.sort_by(|&a, &b| {
            self.is_dir(b)
                .cmp(&self.is_dir(a))
                .then_with(|| {
                    let by_key = match key {
                        SortKey::Name => self.cmp_names(a, b),
                        SortKey::Allocated => self.allocated(a).cmp(&self.allocated(b)),
                        SortKey::Size => self.size(a).cmp(&self.size(b)),
                        SortKey::Files => self.file_count(a).cmp(&self.file_count(b)),
                        SortKey::Dirs => self.dir_count(a).cmp(&self.dir_count(b)),
                        SortKey::Modified => self.mtime(a).cmp(&self.mtime(b)),
                    };
                    if desc { by_key.reverse() } else { by_key }
                })
                .then_with(|| self.cmp_names(a, b))
        });
        out
    }

    /// The child of `dir` called `name`, compared case-insensitively.
    pub fn child_by_name(&self, dir: NodeId, name: &str) -> Option<NodeId> {
        let wanted = || name.chars().flat_map(char::to_lowercase);
        self.children(dir)
            .iter()
            .copied()
            .find(|&c| self.name(c).chars().flat_map(char::to_lowercase).eq(wanted()))
    }

    fn cmp_names(&self, a: NodeId, b: NodeId) -> Ordering {
        let lower = |id| self.name(id).chars().flat_map(char::to_lowercase);
        lower(a).cmp(lower(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Tree, BuildStats) {
        let mut b = TreeBuilder::with_capacity(8, 64);
        let root = b.push("C:", NO_PARENT, 0, 0, 10, F_DIR); // 0
        let a = b.push("a", root, 0, 0, 20, F_DIR); // 1
        b.push("big.bin", a, 1000, 4096, 50, 0); // 2
        b.push("Small.txt", a, 10, 0, 30, 0); // 3
        let bd = b.push("b", a, 0, 0, 5, F_DIR); // 4
        b.push("x", bd, 5, 4096, 40, 0); // 5
        b.push("top.txt", root, 1, 4096, 1, 0); // 6
        b.push("lost", 99, 7, 7, 7, 0); // 7: parent missing
        b.finish(root)
    }

    #[test]
    fn aggregates() {
        let (t, s) = sample();
        assert_eq!(
            (t.size(0), t.allocated(0), t.file_count(0), t.dir_count(0), t.mtime(0)),
            (1016, 12288, 4, 2, 50)
        );
        assert_eq!((t.size(1), t.file_count(1), t.dir_count(1)), (1015, 3, 1));
        assert_eq!(t.file_count(2), 0);
        assert_eq!(s.orphaned, 1);
        assert_eq!(t.path(5), "C:\\a\\b\\x");
        assert_eq!(t.metric(1, Metric::Allocated), 8192);
    }

    #[test]
    fn accessors_and_orphan_stay_valid() {
        let (t, _) = sample();
        assert_eq!(t.len(), 8);
        assert_eq!(t.root(), 0);
        assert_eq!(t.name(3), "Small.txt");
        assert_eq!(t.parent(0), None);
        assert_eq!(t.parent(4), Some(1));
        assert!(t.is_dir(4) && !t.is_dir(5));
        assert_eq!(t.children(1), &[2, 3, 4]);
        assert_eq!(t.children(0), &[1, 6]);
        // The orphan keeps its id and data but hangs in no children list.
        assert_eq!(t.name(7), "lost");
        assert_eq!(t.size(7), 7);
        assert!(t.children(0).iter().chain(t.children(1)).all(|&c| c != 7));
    }

    #[test]
    fn sorting_dirs_first() {
        let (t, _) = sample();
        assert_eq!(t.sorted_children(1, SortKey::Allocated, true), vec![4, 2, 3]);
        assert_eq!(t.sorted_children(1, SortKey::Name, false), vec![4, 2, 3]); // b | big.bin, Small.txt
        assert_eq!(t.sorted_children(1, SortKey::Name, true), vec![4, 3, 2]);
    }

    #[test]
    fn sorting_other_keys_and_ties() {
        let (t, _) = sample();
        assert_eq!(t.sorted_children(1, SortKey::Size, false), vec![4, 3, 2]);
        assert_eq!(t.sorted_children(1, SortKey::Modified, true), vec![4, 2, 3]);
        // Files are all 0 -> tie, broken by name ascending (big.bin < Small.txt).
        assert_eq!(t.sorted_children(1, SortKey::Files, true), vec![4, 2, 3]);
        assert_eq!(t.sorted_children(0, SortKey::Dirs, true), vec![1, 6]);
    }

    #[test]
    fn child_lookup_ignores_case() {
        let (t, _) = sample();
        assert_eq!(t.child_by_name(1, "small.TXT"), Some(3));
        assert_eq!(t.child_by_name(0, "A"), Some(1));
        assert_eq!(t.child_by_name(0, "x"), None); // grandchild, not a child
        assert_eq!(t.child_by_name(5, "x"), None); // file has no children
        assert_eq!(t.child_by_name(0, "lost"), None); // orphan
    }

    #[test]
    fn add_flags_keeps_existing_bits() {
        let mut b = TreeBuilder::with_capacity(1, 4);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR);
        b.add_flags(root, F_UNREADABLE);
        let (t, _) = b.finish(root);
        assert_eq!(t.flags(root), F_DIR | F_UNREADABLE);
    }

    #[test]
    fn parent_may_come_later() {
        let mut b = TreeBuilder::with_capacity(2, 8);
        b.push("f", 1, 3, 3, 0, 0); // id 0, parent pushed afterwards
        let root = b.push("root", NO_PARENT, 0, 0, 0, F_DIR); // id 1
        let (t, s) = b.finish(root);
        assert_eq!(t.size(1), 3);
        assert_eq!(t.file_count(1), 1);
        assert_eq!(s.orphaned, 0);
    }

    #[test]
    fn cycle_is_orphaned() {
        let mut b = TreeBuilder::with_capacity(3, 8);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR); // 0
        b.push("a", 2, 0, 0, 0, F_DIR); // 1, parent 2
        b.push("b", 1, 0, 0, 0, F_DIR); // 2, parent 1
        let (t, s) = b.finish(root);
        assert_eq!(s.orphaned, 2);
        assert!(t.children(root).is_empty());
        assert!(t.children(1).is_empty());
        // path of a cycle member ends after one step per entry
        assert_eq!(t.path(1), "a\\b\\a");
        assert_eq!(t.path(2), "b\\a\\b");
    }

    #[test]
    fn child_of_a_file_is_orphaned() {
        let mut b = TreeBuilder::with_capacity(3, 8);
        let root = b.push("r", NO_PARENT, 0, 0, 0, F_DIR); // 0
        let file = b.push("f", root, 5, 5, 0, 0); // 1
        let c = b.push("c", file, 7, 7, 0, 0); // 2, parent is a file
        let (t, s) = b.finish(root);
        assert_eq!(s.orphaned, 1);
        assert_eq!(t.parent(c), None);
        assert!(t.children(file).is_empty());
        assert_eq!(t.children(root), &[file]);
        assert_eq!((t.size(root), t.file_count(root)), (5, 1));
    }

    #[test]
    fn deep_chain_no_stack_overflow() {
        const DEPTH: u32 = 100_000;
        let mut b = TreeBuilder::with_capacity(DEPTH as usize + 1, DEPTH as usize + 8);
        let root = b.push("d", NO_PARENT, 0, 0, 0, F_DIR);
        for i in 1..DEPTH {
            b.push("d", i - 1, 0, 0, 0, F_DIR);
        }
        let file = b.push("f", DEPTH - 1, 1, 1, 0, 0);
        let (t, s) = b.finish(root);
        assert_eq!(s.orphaned, 0);
        assert_eq!(t.size(root), 1);
        assert_eq!(t.dir_count(root), DEPTH - 1);
        assert!(t.path(file).len() > 100_000);
        assert_eq!(t.sorted_children(root, SortKey::Size, true), vec![1]);
    }

    #[test]
    fn tree_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Tree>();
    }
}
