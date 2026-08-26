use anyhow::{Context, Result, bail};
use self_update::backends::github::{ReleaseList, Update};

use crate::version::VERSION;

#[derive(Clone, Debug)]
pub(super) struct AvailableUpdate {
    pub(super) version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Channel {
    Alpha,
    Beta,
    Rc,
    Stable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct KamaVersion {
    year: u16,
    month: u8,
    channel: Channel,
    revision: u32,
}

impl KamaVersion {
    fn parse(value: &str) -> Option<Self> {
        let (date, release) = value.split_once('-')?;
        let (year, month) = date.split_once('.')?;
        let (channel, revision) = release.split_once('.')?;
        let channel = match channel {
            "alpha" => Channel::Alpha,
            "beta" => Channel::Beta,
            "rc" => Channel::Rc,
            "stable" => Channel::Stable,
            _ => return None,
        };
        Some(Self {
            year: year.parse().ok()?,
            month: month.parse().ok()?,
            channel,
            revision: revision.parse().ok()?,
        })
    }
}

fn is_allowed_update(current: KamaVersion, candidate: KamaVersion) -> bool {
    candidate.channel >= current.channel && candidate > current
}

pub(super) fn enabled() -> bool {
    !VERSION.ends_with("-dev") && repository().is_some() && platform().is_some()
}

pub(super) fn check() -> Result<Option<AvailableUpdate>> {
    if !enabled() {
        return Ok(None);
    }
    let current = KamaVersion::parse(VERSION).context("invalid Kama release version")?;
    let (owner, repository) = repository().context("missing update repository")?;
    let releases = ReleaseList::configure()
        .repo_owner(owner)
        .repo_name(repository)
        .build()?
        .fetch()
        .context("fetch GitHub releases")?;

    let latest = releases
        .into_iter()
        .filter_map(|release| {
            KamaVersion::parse(&release.version).map(|version| (version, release.version))
        })
        .filter(|(version, _)| is_allowed_update(current, *version))
        .max_by_key(|(version, _)| *version);

    Ok(latest.map(|(_, version)| AvailableUpdate { version }))
}

pub(super) fn install(version: &str) -> Result<()> {
    let (owner, repository) = repository().context("missing update repository")?;
    let Some((identifier, bin_path)) = platform() else {
        bail!("this platform does not have a Kama release updater");
    };

    Update::configure()
        .repo_owner(owner)
        .repo_name(repository)
        .bin_name("kama")
        .bin_path_in_archive(bin_path)
        .identifier(identifier)
        .current_version(VERSION)
        .target_version_tag(version)
        .show_download_progress(false)
        .show_output(false)
        .no_confirm(true)
        .build()?
        .update()
        .context("install Kama update")?;
    Ok(())
}

fn repository() -> Option<(&'static str, &'static str)> {
    option_env!("APP_UPDATE_REPOSITORY")
        .filter(|value| !value.is_empty())
        .and_then(|value| value.split_once('/'))
}

fn platform() -> Option<(&'static str, &'static str)> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(("linux-x86_64", "kama"));
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some(("windows-x86_64", "kama.exe"));
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some(("macos-arm64", "Kama.app/Contents/MacOS/kama"));
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_versions() {
        assert_eq!(
            KamaVersion::parse("2026.08-rc.3"),
            Some(KamaVersion {
                year: 2026,
                month: 8,
                channel: Channel::Rc,
                revision: 3,
            })
        );
        assert_eq!(KamaVersion::parse("2026.08-dev"), None);
    }

    #[test]
    fn orders_channels_from_alpha_to_stable() {
        assert!(KamaVersion::parse("2026.08-alpha.9") < KamaVersion::parse("2026.08-beta.1"));
        assert!(KamaVersion::parse("2026.08-beta.1") < KamaVersion::parse("2026.08-stable.1"));
    }

    #[test]
    fn does_not_move_a_stable_install_to_a_prerelease_channel() {
        let current = KamaVersion::parse("2026.08-stable.1");
        let candidate = KamaVersion::parse("2026.09-alpha.1");
        assert_eq!(
            current
                .zip(candidate)
                .map(|(current, candidate)| is_allowed_update(current, candidate)),
            Some(false),
        );
    }
}
