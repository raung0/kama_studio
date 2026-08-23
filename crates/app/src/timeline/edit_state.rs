use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
};

use super::{Clip, ClipboardClip, TimelineDocument};
use crate::project::{MediaId, VisualSource};



#[derive(Debug)]
pub struct TimelineEditState {
    pub(super) document: TimelineDocument,
    pub(super) selected: HashSet<u32>,
    pub(super) primary_selected: Option<u32>,
    pub(super) clipboard: Vec<ClipboardClip>,
}

impl TimelineEditState {
    pub(crate) fn new(document: TimelineDocument) -> Self {
        Self {
            document,
            selected: HashSet::new(),
            primary_selected: None,
            clipboard: Vec::new(),
        }
    }

    pub(crate) fn document(&self) -> &TimelineDocument {
        &self.document
    }

    pub(crate) fn selected_clip_id(&self) -> Option<u32> {
        self.primary_selected
            .filter(|id| self.selected.contains(id))
            .or_else(|| self.selected.iter().copied().min())
    }

    pub(crate) fn selected_clip(&self) -> Option<&Clip> {
        let id = self.selected_clip_id()?;
        self.clips.iter().find(|clip| clip.id == id)
    }

    #[cfg(test)]
    pub(super) fn selected_clip_mut(&mut self) -> Option<&mut Clip> {
        let id = self.selected_clip_id()?;
        self.clip_mut(id)
    }

    #[cfg(test)]
    pub(super) fn clip_mut(&mut self, id: u32) -> Option<&mut Clip> {
        self.document.clips.iter_mut().find(|clip| clip.id == id)
    }

    pub(super) fn track_mut(&mut self, id: u32) -> Option<&mut super::Track> {
        self.document.tracks.iter_mut().find(|track| track.id == id)
    }

    pub(crate) fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    pub(crate) fn is_clip_selected(&self, id: u32) -> bool {
        self.selected.contains(&id)
    }

    pub(crate) fn selected_media_ids(&self) -> HashSet<MediaId> {
        self.clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .filter_map(|clip| match clip.source {
                VisualSource::Media(media) | VisualSource::Audio(media) => Some(media),
                _ => None,
            })
            .collect()
    }
}

impl Deref for TimelineEditState {
    type Target = TimelineDocument;

    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl DerefMut for TimelineEditState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}
