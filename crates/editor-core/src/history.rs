use std::time::{Duration, Instant};

const COALESCE_WINDOW: Duration = Duration::from_millis(850);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryEntry<'a> {
    pub id: u64,
    pub label: &'a str,
    pub parent: Option<usize>,
    pub first_child: Option<usize>,
}

struct HistoryNode<S> {
    id: u64,
    label: String,
    parent: Option<usize>,
    children: Vec<usize>,
    preferred_child: Option<usize>,
    snapshot: S,
}

pub struct HistoryGraph<S> {
    nodes: Vec<HistoryNode<S>>,
    current: usize,
    next_id: u64,
    revision: u64,
    last_coalesce: Option<(String, Instant)>,
}

impl<S: Clone> HistoryGraph<S> {
    pub fn new(label: impl Into<String>, snapshot: S) -> Self {
        Self {
            nodes: vec![HistoryNode {
                id: 1,
                label: label.into(),
                parent: None,
                children: Vec::new(),
                preferred_child: None,
                snapshot,
            }],
            current: 0,
            next_id: 2,
            revision: 1,
            last_coalesce: None,
        }
    }

    pub fn reset(&mut self, label: impl Into<String>, snapshot: S) {
        *self = Self::new(label, snapshot);
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn current(&self) -> usize {
        self.current
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn entry(&self, index: usize) -> Option<HistoryEntry<'_>> {
        let node = self.nodes.get(index)?;
        Some(HistoryEntry {
            id: node.id,
            label: &node.label,
            parent: node.parent,
            first_child: node.children.first().copied(),
        })
    }

    pub fn record_after(
        &mut self,
        label: impl Into<String>,
        before: &S,
        after: S,
        coalesce: bool,
        equivalent: impl Fn(&S, &S) -> bool,
    ) -> bool {
        if equivalent(before, &after) {
            return false;
        }

        let label = label.into();
        let now = Instant::now();
        let can_coalesce = coalesce
            && self.current != 0
            && self.nodes[self.current].children.is_empty()
            && self.nodes[self.current].label == label
            && self.last_coalesce.as_ref().is_some_and(|(last, at)| {
                last == &label && now.saturating_duration_since(*at) <= COALESCE_WINDOW
            });
        if can_coalesce {
            self.nodes[self.current].snapshot = after;
            self.last_coalesce = Some((label, now));
            self.bump_revision();
            return true;
        }

        let parent = self.current;
        let index = self.nodes.len();
        self.nodes.push(HistoryNode {
            id: self.next_id,
            label: label.clone(),
            parent: Some(parent),
            children: Vec::new(),
            preferred_child: None,
            snapshot: after,
        });
        self.next_id += 1;
        self.nodes[parent].children.push(index);
        self.nodes[parent].preferred_child = Some(index);
        self.current = index;
        self.last_coalesce = coalesce.then_some((label, now));
        self.bump_revision();
        true
    }

    pub fn undo(&mut self) -> Option<S> {
        let child = self.current;
        let parent = self.nodes.get(child)?.parent?;
        self.nodes[parent].preferred_child = Some(child);
        self.select(parent)
    }

    pub fn redo(&mut self) -> Option<S> {
        let child = self.nodes.get(self.current)?.preferred_child?;
        self.select(child)
    }

    pub fn select(&mut self, index: usize) -> Option<S> {
        let snapshot = self.nodes.get(index)?.snapshot.clone();
        if index != self.current {
            self.bump_revision();
        }
        let mut child = index;
        while let Some(parent) = self.nodes[child].parent {
            self.nodes[parent].preferred_child = Some(child);
            child = parent;
        }
        self.current = index;
        self.last_coalesce = None;
        Some(snapshot)
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryGraph;

    #[test]
    fn undo_then_edit_keeps_both_branches() {
        let mut history = HistoryGraph::new("open", 0);
        assert!(history.record_after("one", &0, 1, false, PartialEq::eq));
        assert_eq!(history.undo(), Some(0));
        assert!(history.record_after("two", &0, 2, false, PartialEq::eq));
        assert_eq!(history.len(), 3);
        assert_eq!(history.undo(), Some(0));
        assert_eq!(history.redo(), Some(2));
        assert_eq!(history.select(1), Some(1));
    }

    #[test]
    fn no_op_does_not_create_a_commit() {
        let mut history = HistoryGraph::new("open", 4);
        assert!(!history.record_after("same", &4, 4, false, PartialEq::eq));
        assert_eq!(history.len(), 1);
    }
}
