//! Provides constructs for the superzent app version and release channel.

#![deny(missing_docs)]

use std::{env, str::FromStr, sync::LazyLock};

use gpui::{App, Global};
use semver::Version;

fn env_var(primary: &str, legacy: &str) -> Option<String> {
    env::var(primary).ok().or_else(|| env::var(legacy).ok())
}

fn env_flag(primary: &str, legacy: &str, predicate: impl Fn(&str) -> bool) -> bool {
    env_var(primary, legacy).as_deref().is_some_and(predicate)
}

fn build_release_channel_name() -> Option<&'static str> {
    option_env!("SUPERZENT_RELEASE_CHANNEL")
        .or(option_env!("ZED_RELEASE_CHANNEL"))
        .or(option_env!("RELEASE_CHANNEL"))
}

/// stable | dev
pub static RELEASE_CHANNEL_NAME: LazyLock<String> = LazyLock::new(|| {
    resolved_release_channel_name(
        build_release_channel_name(),
        env_var("SUPERZENT_RELEASE_CHANNEL", "ZED_RELEASE_CHANNEL"),
        include_str!("../../zed/RELEASE_CHANNEL").trim(),
        cfg!(debug_assertions),
        env_flag("SUPERZENT_TERM", "ZED_TERM", |value| value == "true"),
    )
});

#[doc(hidden)]
pub static RELEASE_CHANNEL: LazyLock<ReleaseChannel> =
    LazyLock::new(|| match ReleaseChannel::from_str(&RELEASE_CHANNEL_NAME) {
        Ok(channel) => channel,
        _ => panic!("invalid release channel {}", *RELEASE_CHANNEL_NAME),
    });

/// The app identifier for the current release channel, Windows only.
#[cfg(target_os = "windows")]
pub fn app_identifier() -> &'static str {
    match *RELEASE_CHANNEL {
        ReleaseChannel::Dev => "superzent-dev",
        ReleaseChannel::Stable => "superzent-stable",
    }
}

/// The Git commit SHA that superzent was built at.
#[derive(Clone, Eq, Debug, PartialEq)]
pub struct AppCommitSha(String);

struct GlobalAppCommitSha(AppCommitSha);

impl Global for GlobalAppCommitSha {}

impl AppCommitSha {
    /// Creates a new [`AppCommitSha`].
    pub fn new(sha: String) -> Self {
        AppCommitSha(sha)
    }

    /// Returns the global [`AppCommitSha`], if one is set.
    pub fn try_global(cx: &App) -> Option<AppCommitSha> {
        cx.try_global::<GlobalAppCommitSha>()
            .map(|sha| sha.0.clone())
    }

    /// Sets the global [`AppCommitSha`].
    pub fn set_global(sha: AppCommitSha, cx: &mut App) {
        cx.set_global(GlobalAppCommitSha(sha))
    }

    /// Returns the full commit SHA.
    pub fn full(&self) -> String {
        self.0.to_string()
    }

    /// Returns the short (7 character) commit SHA.
    pub fn short(&self) -> String {
        self.0.chars().take(7).collect()
    }
}

struct GlobalAppVersion(Version);

impl Global for GlobalAppVersion {}

/// The version of superzent.
pub struct AppVersion;

impl AppVersion {
    /// Load the app version from env.
    pub fn load(
        pkg_version: &str,
        build_id: Option<&str>,
        commit_sha: Option<AppCommitSha>,
    ) -> Version {
        let mut version: Version =
            if let Some(from_env) = env_var("SUPERZENT_APP_VERSION", "ZED_APP_VERSION") {
                from_env
                    .parse()
                    .expect("invalid SUPERZENT_APP_VERSION or ZED_APP_VERSION")
            } else {
                pkg_version.parse().expect("invalid version in Cargo.toml")
            };
        let mut pre = String::from(RELEASE_CHANNEL.dev_name());

        if let Some(build_id) = build_id {
            pre.push('.');
            pre.push_str(&build_id);
        }

        if let Some(sha) = commit_sha {
            pre.push('.');
            pre.push_str(&sha.0);
        }
        if let Ok(build) = semver::BuildMetadata::new(&pre) {
            version.build = build;
        }

        version
    }

    /// Returns the global version number.
    pub fn global(cx: &App) -> Version {
        if cx.has_global::<GlobalAppVersion>() {
            cx.global::<GlobalAppVersion>().0.clone()
        } else {
            Version::new(0, 0, 0)
        }
    }
}

/// A Zed release channel.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum ReleaseChannel {
    /// The development release channel.
    ///
    /// Used for local debug builds of Zed.
    #[default]
    Dev,

    /// The Stable release channel.
    Stable,
}

struct GlobalReleaseChannel(ReleaseChannel);

impl Global for GlobalReleaseChannel {}

/// Initializes the release channel.
pub fn init(app_version: Version, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(*RELEASE_CHANNEL))
}

/// Initializes the release channel for tests that rely on fake release channel.
pub fn init_test(app_version: Version, release_channel: ReleaseChannel, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(release_channel))
}

impl ReleaseChannel {
    /// Returns the global [`ReleaseChannel`].
    pub fn global(cx: &App) -> Self {
        cx.global::<GlobalReleaseChannel>().0
    }

    /// Returns the global [`ReleaseChannel`], if one is set.
    pub fn try_global(cx: &App) -> Option<Self> {
        cx.try_global::<GlobalReleaseChannel>()
            .map(|channel| channel.0)
    }

    /// Returns whether we want to poll for updates for this [`ReleaseChannel`]
    pub fn poll_for_updates(&self) -> bool {
        !matches!(self, ReleaseChannel::Dev)
    }

    /// Returns the display name for this [`ReleaseChannel`].
    pub fn display_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "superzent dev",
            ReleaseChannel::Stable => "superzent",
        }
    }

    /// Returns the programmatic name for this [`ReleaseChannel`].
    pub fn dev_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "dev",
            ReleaseChannel::Stable => "stable",
        }
    }

    /// Returns the application ID that's used by Wayland as application ID
    /// and WM_CLASS on X11.
    /// This also has to match the bundle identifier for Zed on macOS.
    pub fn app_id(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "ai.nangman.superzent-dev",
            ReleaseChannel::Stable => "ai.nangman.superzent",
        }
    }

    /// Returns the query parameter for this [`ReleaseChannel`].
    pub fn release_query_param(&self) -> Option<&'static str> {
        match self {
            Self::Dev => None,
            Self::Stable => None,
        }
    }
}

/// Error indicating that release channel string does not match any known release channel names.
#[derive(Copy, Clone, Debug, Hash, PartialEq)]
pub struct InvalidReleaseChannel;

impl FromStr for ReleaseChannel {
    type Err = InvalidReleaseChannel;

    fn from_str(channel: &str) -> Result<Self, Self::Err> {
        Ok(match channel {
            "dev" => ReleaseChannel::Dev,
            "stable" => ReleaseChannel::Stable,
            _ => return Err(InvalidReleaseChannel),
        })
    }
}

fn resolved_release_channel_name(
    build_release_channel: Option<&str>,
    env_release_channel: Option<String>,
    default_release_channel: &str,
    debug_assertions_enabled: bool,
    is_zed_terminal: bool,
) -> String {
    if debug_assertions_enabled && !is_zed_terminal {
        env_release_channel
            .or_else(|| build_release_channel.map(ToOwned::to_owned))
            .unwrap_or_else(|| default_release_channel.to_string())
    } else {
        build_release_channel
            .unwrap_or(default_release_channel)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::resolved_release_channel_name;

    #[test]
    fn debug_builds_use_build_channel_inside_zed_terminal() {
        assert_eq!(
            resolved_release_channel_name(
                Some("stable"),
                Some("dev".to_string()),
                "dev",
                true,
                true
            ),
            "stable"
        );
    }

    #[test]
    fn debug_builds_still_allow_runtime_override_outside_zed_terminal() {
        assert_eq!(
            resolved_release_channel_name(
                Some("stable"),
                Some("dev".to_string()),
                "dev",
                true,
                false
            ),
            "dev"
        );
    }

    #[test]
    fn non_debug_builds_use_build_channel() {
        assert_eq!(
            resolved_release_channel_name(
                Some("stable"),
                Some("dev".to_string()),
                "stable",
                false,
                false
            ),
            "stable"
        );
    }

    #[test]
    fn non_debug_builds_ignore_runtime_override_without_build_channel() {
        assert_eq!(
            resolved_release_channel_name(None, Some("dev".to_string()), "stable", false, false),
            "stable"
        );
    }
}
