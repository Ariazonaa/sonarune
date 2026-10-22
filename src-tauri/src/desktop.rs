use crate::cache::{normalize_limit_mb, DEFAULT_LIMIT_MB};
use crate::error::{AppError, AppResult};
use crate::store::{keys, Store};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseBehavior {
    #[default]
    Quit,
    Tray,
}

/// Which releases an update check considers (see updater.rs). Beta takes the
/// newest of both: a beta tester gets the stable release that follows a beta
/// too, instead of staying behind on the beta.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DesktopSettings {
    pub close_behavior: CloseBehavior,
    pub start_minimized: bool,
    pub cover_cache_limit_mb: u64,
    /// Ask the release endpoint once per start whether a newer version exists
    /// (see updater.rs). Off means the app never talks to GitHub on its own;
    /// the button in Settings still checks on demand.
    pub auto_check_updates: bool,
    /// Stable unless asked for. Leaving beta installs nothing older: the beta
    /// stays until a stable release is newer than it.
    pub update_channel: UpdateChannel,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            close_behavior: CloseBehavior::Quit,
            start_minimized: false,
            cover_cache_limit_mb: DEFAULT_LIMIT_MB,
            auto_check_updates: true,
            update_channel: UpdateChannel::Stable,
        }
    }
}

impl DesktopSettings {
    pub fn normalized(mut self) -> Self {
        self.cover_cache_limit_mb = normalize_limit_mb(self.cover_cache_limit_mb);
        self
    }
}

pub fn load(store: &Store) -> DesktopSettings {
    store
        .get(keys::DESKTOP)
        .and_then(|json| serde_json::from_str::<DesktopSettings>(&json).ok())
        .unwrap_or_default()
        .normalized()
}

pub fn save(store: &Store, settings: DesktopSettings) -> AppResult<DesktopSettings> {
    let settings = settings.normalized();
    let json = serde_json::to_string(&settings)
        .map_err(|e| AppError::Other(format!("cannot serialize desktop settings: {e}")))?;
    store.set(keys::DESKTOP, &json)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::{DesktopSettings, UpdateChannel};

    #[test]
    fn settings_from_before_the_channel_stay_on_stable() {
        let old: DesktopSettings = serde_json::from_str(
            r#"{"closeBehavior":"tray","startMinimized":false,"coverCacheLimitMb":256,"autoCheckUpdates":true}"#,
        )
        .unwrap();
        assert_eq!(old.update_channel, UpdateChannel::Stable);
        assert_eq!(serde_json::to_value(UpdateChannel::Beta).unwrap(), "beta");
    }

    #[test]
    fn cache_limit_is_bounded() {
        let settings = DesktopSettings {
            cover_cache_limit_mb: 1,
            ..DesktopSettings::default()
        };
        assert_eq!(settings.normalized().cover_cache_limit_mb, 32);

        let settings = DesktopSettings {
            cover_cache_limit_mb: u64::MAX,
            ..DesktopSettings::default()
        };
        assert_eq!(settings.normalized().cover_cache_limit_mb, 2_048);
    }
}
