

pub struct DocumentStatus {
    saved_content: Vec<u8>,
    saved_view: Vec<u8>,
    observed_revision: u64,
    observed_content: Vec<u8>,
    observed_view: Vec<u8>,
    unsaved: bool,
}

impl DocumentStatus {
    pub fn new(revision: u64, content: Vec<u8>, view: Vec<u8>) -> Self {
        Self {
            saved_content: content.clone(),
            saved_view: view.clone(),
            observed_revision: revision,
            observed_content: content,
            observed_view: view,
            unsaved: false,
        }
    }

    
    pub fn refresh(
        &mut self,
        revision: u64,
        view: Vec<u8>,
        content_if_changed: impl FnOnce() -> Vec<u8>,
    ) -> bool {
        let revision_changed = revision != self.observed_revision;
        let view_changed = view != self.observed_view;
        if !revision_changed && !view_changed {
            return false;
        }
        if revision_changed {
            self.observed_revision = revision;
            self.observed_content = content_if_changed();
        }
        if view_changed {
            self.observed_view = view;
        }
        let unsaved =
            self.observed_content != self.saved_content || self.observed_view != self.saved_view;
        let changed = unsaved != self.unsaved;
        self.unsaved = unsaved;
        changed
    }

    pub fn mark_saved(&mut self, revision: u64, content: Vec<u8>, view: Vec<u8>) {
        self.saved_content = content.clone();
        self.saved_view = view.clone();
        self.observed_revision = revision;
        self.observed_content = content;
        self.observed_view = view;
        self.unsaved = false;
    }

    pub fn is_unsaved(&self) -> bool {
        self.unsaved
    }

    pub fn differs_from_saved(&self, content: &[u8], view: &[u8]) -> bool {
        content != self.saved_content || view != self.saved_view
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentStatus;

    #[test]
    fn reports_only_dirty_flag_transitions() {
        let mut status = DocumentStatus::new(1, vec![1], vec![2]);
        assert!(!status.refresh(1, vec![2], || panic!("content must stay cached")));
        assert!(status.refresh(2, vec![2], || vec![3]));
        assert!(status.is_unsaved());
        assert!(!status.refresh(3, vec![2], || vec![4]));
        status.mark_saved(3, vec![4], vec![2]);
        assert!(!status.is_unsaved());
    }
}
