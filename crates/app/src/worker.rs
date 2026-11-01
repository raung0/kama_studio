pub(super) fn run(kind: &str) -> anyhow::Result<()> {
    match kind {
        "audio" => crate::audio::run_audio_worker(),
        "media" => crate::media_worker::run(),
        "render" => crate::render::run_processing_worker(),
        "video" => crate::video_worker::run_video_worker(),
        _ => anyhow::bail!("unknown worker kind: {kind}"),
    }
}

pub(crate) fn is_processing_child() -> bool {
    std::env::args().any(|argument| {
        argument
            .strip_prefix("--kama-worker=")
            .is_some_and(|kind| matches!(kind, "audio" | "media" | "render" | "video"))
    }) || [
        "KAMA_AUDIO_WORKER_TEST_CHILD",
        "KAMA_MEDIA_WORKER_TEST_CHILD",
        "KAMA_RENDER_WORKER_TEST_CHILD",
        "KAMA_VIDEO_WORKER_TEST_CHILD",
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some())
}
