use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};

use anyhow::{Context, Result, bail};
use self_update::backends::github::{ReleaseList, Update};
use serde::{Deserialize, Serialize};

use crate::version::VERSION;

#[derive(Clone, Debug)]
pub(super) struct AvailableUpdate {
    pub(super) version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReleaseChannel {
    Alpha,
    Beta,
    Rc,
    Stable,
}

impl ReleaseChannel {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Alpha => 0,
            Self::Beta => 1,
            Self::Rc => 2,
            Self::Stable => 3,
        }
    }

    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Alpha),
            1 => Some(Self::Beta),
            2 => Some(Self::Rc),
            3 => Some(Self::Stable),
            _ => None,
        }
    }
}

static RELEASE_CHANNEL: OnceLock<AtomicU8> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct KamaVersion {
    year: u16,
    month: u8,
    channel: ReleaseChannel,
    revision: u32,
}

impl KamaVersion {
    fn parse(value: &str) -> Option<Self> {
        let (date, release) = value.split_once('-')?;
        let (year, month) = date.split_once('.')?;
        let (channel, revision) = release.split_once('.')?;
        let channel = match channel {
            "alpha" => ReleaseChannel::Alpha,
            "beta" => ReleaseChannel::Beta,
            "rc" => ReleaseChannel::Rc,
            "stable" => ReleaseChannel::Stable,
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

fn release_channel_from_version(value: &str) -> Option<ReleaseChannel> {
    KamaVersion::parse(value).map(|version| version.channel)
}

pub(crate) fn default_release_channel() -> ReleaseChannel {
    release_channel_from_version(VERSION).unwrap_or(ReleaseChannel::Stable)
}

fn release_channel_storage() -> &'static AtomicU8 {
    RELEASE_CHANNEL.get_or_init(|| AtomicU8::new(default_release_channel().as_u8()))
}

pub(crate) fn release_channel() -> ReleaseChannel {
    ReleaseChannel::from_u8(release_channel_storage().load(Ordering::Relaxed))
        .unwrap_or_else(default_release_channel)
}

pub(crate) fn set_release_channel(channel: ReleaseChannel) {
    release_channel_storage().store(channel.as_u8(), Ordering::Relaxed);
}

fn is_allowed_update(
    current: KamaVersion,
    candidate: KamaVersion,
    channel: ReleaseChannel,
) -> bool {
    candidate.channel >= channel && candidate > current
}

pub(super) fn enabled() -> bool {
    !VERSION.ends_with("-dev") && repository().is_some() && platform().is_some()
}

pub(super) fn check() -> Result<Option<AvailableUpdate>> {
    if !enabled() {
        return Ok(None);
    }
    let current = KamaVersion::parse(VERSION).context("invalid Kama release version")?;
    let channel = release_channel();
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
        .filter(|(version, _)| is_allowed_update(current, *version, channel))
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
                channel: ReleaseChannel::Rc,
                revision: 3,
            })
        );
        assert_eq!(KamaVersion::parse("2026.08-dev"), None);
        assert_eq!(
            release_channel_from_version("2026.08-beta.2"),
            Some(ReleaseChannel::Beta)
        );
    }

    #[test]
    fn orders_channels_from_alpha_to_stable() {
        assert!(KamaVersion::parse("2026.08-alpha.9") < KamaVersion::parse("2026.08-beta.1"));
        assert!(KamaVersion::parse("2026.08-beta.1") < KamaVersion::parse("2026.08-stable.1"));
    }

    #[test]
    fn selected_channel_controls_prerelease_updates() {
        let current = KamaVersion::parse("2026.08-stable.1").unwrap();
        let alpha = KamaVersion::parse("2026.09-alpha.1").unwrap();
        let beta = KamaVersion::parse("2026.09-beta.1").unwrap();
        let stable = KamaVersion::parse("2026.09-stable.1").unwrap();

        assert!(is_allowed_update(current, alpha, ReleaseChannel::Alpha));
        assert!(!is_allowed_update(current, alpha, ReleaseChannel::Beta));
        assert!(is_allowed_update(current, beta, ReleaseChannel::Beta));
        assert!(!is_allowed_update(current, beta, ReleaseChannel::Stable));
        assert!(is_allowed_update(current, stable, ReleaseChannel::Stable));
    }

    #[test]
    fn release_build_defaults_to_its_own_channel() {
        assert_eq!(
            release_channel_from_version("2026.08-alpha.4"),
            Some(ReleaseChannel::Alpha),
        );
        assert_eq!(
            release_channel_from_version("2026.08-stable.2"),
            Some(ReleaseChannel::Stable),
        );
    }

    #[test]
    fn does_not_move_a_stable_install_to_a_prerelease_channel() {
        let current = KamaVersion::parse("2026.08-stable.1");
        let candidate = KamaVersion::parse("2026.09-alpha.1");
        assert_eq!(
            current
                .zip(candidate)
                .map(|(current, candidate)| is_allowed_update(
                    current,
                    candidate,
                    ReleaseChannel::Stable
                )),
            Some(false),
        );
    }
}
