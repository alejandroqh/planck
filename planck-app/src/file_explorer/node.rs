use floem::views::VirtualVector;
use planck_rpc::file::{FileNodeItem, FileNodeViewData, Naming};

pub struct FileNodeVirtualList {
    roots: Vec<FileNodeItem>,
    naming: Naming,
}

impl FileNodeVirtualList {
    pub fn new(roots: Vec<FileNodeItem>, naming: Naming) -> Self {
        Self { roots, naming }
    }
}

impl VirtualVector<FileNodeViewData> for FileNodeVirtualList {
    fn total_len(&self) -> usize {
        self.roots
            .iter()
            .map(|root| root.children_open_count + 1)
            .sum()
    }

    fn slice(
        &mut self,
        range: std::ops::Range<usize>,
    ) -> impl Iterator<Item = FileNodeViewData> {
        let naming = &self.naming;

        let min = range.start;
        let max = range.end;
        let mut view_items = Vec::new();

        // Roots follow each other; `append_view_slice` returns the last row it covered.
        let mut next = 0;
        for root in &self.roots {
            next =
                root.append_view_slice(&mut view_items, naming, min, max, next, 1)
                    + 1;
            if next > max {
                break;
            }
        }

        view_items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use planck_rpc::file::FileNodeViewKind;

    use super::*;

    fn node(path: &str, open: bool, children: &[&str]) -> FileNodeItem {
        let children: HashMap<PathBuf, FileNodeItem> = children
            .iter()
            .map(|c| (PathBuf::from(c), node(c, false, &[])))
            .collect();
        FileNodeItem {
            path: PathBuf::from(path),
            is_dir: true,
            read: true,
            open,
            children_open_count: if open { children.len() } else { 0 },
            children,
        }
    }

    fn rows(
        list: &mut FileNodeVirtualList,
        range: std::ops::Range<usize>,
    ) -> Vec<String> {
        list.slice(range)
            .map(|item| match item.kind {
                FileNodeViewKind::Path(p) => p.to_string_lossy().to_string(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn roots_follow_each_other() {
        let mut list = FileNodeVirtualList::new(
            vec![
                node("/a", true, &["/a/x", "/a/y"]),
                node("/b", false, &["/b/z"]),
            ],
            Naming::None,
        );
        assert_eq!(list.total_len(), 4);
        assert_eq!(rows(&mut list, 0..4), ["/a", "/a/x", "/a/y", "/b"]);
        assert_eq!(rows(&mut list, 3..4), ["/b"]);
    }
}
