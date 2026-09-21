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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DesktopSettings {
    pub close_behavior: CloseBehavior,
    pub start_minimized: bool,
    pub cover_cache_limit_mb: u64,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            close_behavior: CloseBehavior::Quit,
            start_minimized: false,
            cover_cache_limit_mb: DEFAULT_LIMIT_MB,
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
    use super::DesktopSettings;

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
