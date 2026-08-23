use std::path::PathBuf;

use super::RenderPreset;

#[derive(Clone, Debug)]
pub(super) struct RenderSpec {
    pub(super) preset: RenderPreset,
    pub(super) output: PathBuf,
    pub(super) overwrite: bool,
    pub(super) begin_frame: u64,
    pub(super) end_frame: u64,
    pub(super) background: bool,
    pub(super) transcode: bool,
}
