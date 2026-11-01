#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    if let Some(kind) = std::env::args()
        .nth(1)
        .and_then(|arg| arg.strip_prefix("--kama-worker=").map(str::to_owned))
    {
        return kama::run_worker(&kind);
    }
    kama::run()
}
