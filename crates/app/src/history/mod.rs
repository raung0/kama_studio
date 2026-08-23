use crate::{project::Project, timeline::TimelineDocument};
use kama_editor_core::{HistoryEntry, HistoryGraph};

mod view;

pub(crate) use view::HistoryPanelState;

#[derive(Clone, Debug)]
pub struct HistorySnapshot {
    pub project: Project,
    signature: Vec<u8>,
}

impl HistorySnapshot {
    pub fn capture(project: &Project, timeline: &TimelineDocument) -> Self {
        let mut project = project.clone();
        project.sync_active_timeline(timeline.clone());
        
        
        
        let mut authored = project.clone();
        for composition in &mut authored.compositions {
            composition.timeline.view = Default::default();
        }
        let signature = authored.authored_signature();
        Self { project, signature }
    }

    fn same_document(&self, other: &Self) -> bool {
        self.signature == other.signature
    }
}

pub struct HistoryState {
    graph: HistoryGraph<HistorySnapshot>,
}

impl HistoryState {
    pub fn new(project: &Project, timeline: &TimelineDocument) -> Self {
        Self {
            graph: HistoryGraph::new(
                "Project opened",
                HistorySnapshot::capture(project, timeline),
            ),
        }
    }

    pub fn reset(&mut self, project: &Project, timeline: &TimelineDocument, label: &str) {
        self.graph
            .reset(label, HistorySnapshot::capture(project, timeline));
    }

    pub fn capture(&self, project: &Project, timeline: &TimelineDocument) -> HistorySnapshot {
        HistorySnapshot::capture(project, timeline)
    }

    pub fn revision(&self) -> u64 {
        self.graph.revision()
    }

    pub fn record_after(
        &mut self,
        label: impl Into<String>,
        before: HistorySnapshot,
        project: &Project,
        timeline: &TimelineDocument,
        coalesce: bool,
    ) -> bool {
        self.graph.record_after(
            label,
            &before,
            HistorySnapshot::capture(project, timeline),
            coalesce,
            HistorySnapshot::same_document,
        )
    }

    pub fn undo(&mut self) -> Option<HistorySnapshot> {
        self.graph.undo()
    }

    pub fn redo(&mut self) -> Option<HistorySnapshot> {
        self.graph.redo()
    }

    fn select(&mut self, index: usize) -> Option<HistorySnapshot> {
        self.graph.select(index)
    }

    fn len(&self) -> usize {
        self.graph.len()
    }

    fn current(&self) -> usize {
        self.graph.current()
    }

    fn entry(&self, index: usize) -> Option<HistoryEntry<'_>> {
        self.graph.entry(index)
    }
}
