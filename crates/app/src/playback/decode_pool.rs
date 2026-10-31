use std::path::{Path, PathBuf};

use crate::runtime::media::VideoDecoder;

const VIDEO_DECODER_POOL_CAPACITY: usize = 8;

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
        let existing = self
            .slots
            .iter()
            .position(|slot| slot.owner == owner && slot.path.as_path() == path);
        let reusable = existing.or_else(|| {
            if reserve {
                self.slots
                    .iter()
                    .position(|slot| slot.path.as_path() == path && slot.last_used != generation)
            } else {
                let previous_generation = generation.wrapping_sub(1);
                self.slots.iter().position(|slot| {
                    slot.path.as_path() == path
                        && slot.last_used != generation
                        && slot.last_used != previous_generation
                })
            }
        });
        if let Some(index) = reusable {
            #[expect(
                clippy::expect_used,
                reason = "index comes from position on the unchanged slots"
            )]
            let slot = self.slots.get_mut(index).expect("decoder slot exists");
            slot.owner = owner;
            if reserve {
                slot.last_used = generation;
            }
            return &mut slot.decoder;
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
        #[expect(clippy::expect_used, reason = "a decoder slot was just pushed")]
        let slot = self.slots.last_mut().expect("decoder slot inserted");
        &mut slot.decoder
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut VideoDecoder> {
        self.slots.iter_mut().map(|slot| &mut slot.decoder)
    }

    pub(super) fn clear(&mut self) {
        self.slots.clear();
    }
}
