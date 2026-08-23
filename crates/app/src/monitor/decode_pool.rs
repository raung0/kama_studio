use std::path::{Path, PathBuf};

use super::VIDEO_DECODER_POOL_CAPACITY;
use crate::runtime::media::VideoDecoder;

struct VideoDecoderSlot {
    owner: u64,
    path: PathBuf,
    decoder: VideoDecoder,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct VideoDecoderPool {
    generation: u64,
    slots: Vec<VideoDecoderSlot>,
}

impl VideoDecoderPool {
    pub(super) fn begin_frame(&mut self) {
        let previous = self.generation;
        self.generation = self.generation.wrapping_add(1).max(1);
        if self.slots.len() <= VIDEO_DECODER_POOL_CAPACITY {
            return;
        }

        self.slots
            .sort_unstable_by_key(|slot| std::cmp::Reverse(slot.last_used));
        let concurrent = self
            .slots
            .iter()
            .take_while(|slot| slot.last_used == previous)
            .count();
        let keep = concurrent.max(VIDEO_DECODER_POOL_CAPACITY);
        self.slots.truncate(keep);
    }

    pub(super) fn get(&mut self, owner: u64, path: &Path, reserve: bool) -> &mut VideoDecoder {
        let generation = self.generation;
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.owner == owner && slot.path.as_path() == path)
        {
            if reserve {
                self.slots[index].last_used = generation;
            }
            return &mut self.slots[index].decoder;
        }

        if reserve {
            if let Some(index) = self
                .slots
                .iter()
                .position(|slot| slot.path.as_path() == path && slot.last_used != generation)
            {
                self.slots[index].owner = owner;
                self.slots[index].last_used = generation;
                return &mut self.slots[index].decoder;
            }
        } else {
            let previous_generation = generation.wrapping_sub(1);
            if let Some(index) = self.slots.iter().position(|slot| {
                slot.path.as_path() == path
                    && slot.last_used != generation
                    && slot.last_used != previous_generation
            }) {
                self.slots[index].owner = owner;
                return &mut self.slots[index].decoder;
            }
        }

        self.slots.push(VideoDecoderSlot {
            owner,
            path: path.to_path_buf(),
            decoder: VideoDecoder::new(path.to_path_buf()),
            last_used: if reserve {
                generation
            } else {
                Default::default()
            },
        });
        &mut self
            .slots
            .last_mut()
            .expect("decoder slot inserted")
            .decoder
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut VideoDecoder> {
        self.slots.iter_mut().map(|slot| &mut slot.decoder)
    }

    pub(super) fn clear(&mut self) {
        self.slots.clear();
    }
}
