pub(super) fn run(kind: &str) -> anyhow::Result<()> {
    match kind {
        "audio" => crate::audio::run_audio_worker(),
        "video" => crate::video_worker::run_video_worker(),
        _ => anyhow::bail!("unknown worker kind: {kind}"),
    }
}
