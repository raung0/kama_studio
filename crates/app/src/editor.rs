use std::{collections::HashMap, path::PathBuf};

use kama_editor_core::DocumentStatus;

use crate::{
    app_shared::{document_content_signature, document_view_signature},
    history::{HistorySnapshot, HistoryState},
    plugin::PluginRegistry,
    project::Project,
    timeline::TimelineState,
};



pub(crate) struct EditorSession {
    pub(crate) project: Project,
    pub(crate) project_path: Option<PathBuf>,
    pub(crate) timeline: TimelineState,
    pub(crate) history: HistoryState,
    pub(crate) history_gesture: Option<(HistorySnapshot, String)>,
    status: DocumentStatus,
}

impl EditorSession {
    pub(crate) fn new(
        project: Project,
        timeline: TimelineState,
        project_path: Option<PathBuf>,
    ) -> Self {
        let history = HistoryState::new(&project, &timeline.document());
        let content_signature = document_content_signature(&project, &timeline);
        let view_signature = document_view_signature(&project, &timeline);
        Self {
            project,
            project_path,
            timeline,
            status: DocumentStatus::new(history.revision(), content_signature, view_signature),
            history,
            history_gesture: None,
        }
    }

    pub(crate) fn capture(&self) -> HistorySnapshot {
        self.history.capture(&self.project, &self.timeline)
    }

    pub(crate) fn begin_gesture(&mut self, label: impl Into<String>) {
        if self.history_gesture.is_none() {
            self.history_gesture = Some((self.capture(), label.into()));
        }
    }

    pub(crate) fn set_gesture_label(&mut self, label: impl Into<String>) {
        if let Some((_, current)) = &mut self.history_gesture {
            *current = label.into();
        }
    }

    pub(crate) fn finish_gesture(&mut self) -> bool {
        let Some((before, label)) = self.history_gesture.take() else {
            return false;
        };
        self.record_after(label, before, false)
    }

    pub(crate) fn record_after(
        &mut self,
        label: impl Into<String>,
        before: HistorySnapshot,
        coalesce: bool,
    ) -> bool {
        self.history
            .record_after(label, before, &self.project, &self.timeline, coalesce)
    }

    pub(crate) fn restore_snapshot(&mut self, snapshot: HistorySnapshot, plugins: &PluginRegistry) {
        let active = self.project.active_composition;
        let mut current_views = self
            .project
            .compositions
            .iter()
            .map(|composition| (composition.id, composition.timeline.view))
            .collect::<HashMap<_, _>>();
        current_views.insert(active, self.timeline.document().view);
        self.project = snapshot.project;
        for composition in &mut self.project.compositions {
            if let Some(view) = current_views.remove(&composition.id) {
                composition.timeline.view = view;
            }
        }
        self.timeline
            .load_history_document(self.project.active_composition().timeline.clone());
        self.timeline.ensure_composition_visual_pipelines(plugins);
        self.timeline.reconcile_pipeline_overrides(&self.project);
        self.history_gesture = None;
    }

    pub(crate) fn refresh_dirty_state(&mut self) -> bool {
        let project = &self.project;
        let timeline = &self.timeline;
        self.status.refresh(
            self.history.revision(),
            document_view_signature(project, timeline),
            || document_content_signature(project, timeline),
        )
    }

    pub(crate) fn mark_saved(&mut self) {
        self.status.mark_saved(
            self.history.revision(),
            document_content_signature(&self.project, &self.timeline),
            document_view_signature(&self.project, &self.timeline),
        );
    }

    pub(crate) fn is_unsaved(&self) -> bool {
        self.status.is_unsaved()
    }

    pub(crate) fn has_unsaved_changes(&self) -> bool {
        self.status.differs_from_saved(
            &document_content_signature(&self.project, &self.timeline),
            &document_view_signature(&self.project, &self.timeline),
        )
    }
}
