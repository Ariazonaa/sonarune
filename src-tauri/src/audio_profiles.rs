//! Sound profile per output device: EQ, preamp, normalization and volume are
//! remembered for each device and come back when audio plays through it
//! again — headphones and speakers rarely want the same curve or level.
//!
//! The device is the one the player actually opened (a configured device, or
//! whatever "system default" resolved to, including a fallback), never just
//! the preference: the profile has to match what is heard.

use crate::error::{AppError, AppResult};
use crate::player::DspParams;
use crate::store::{keys, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Serializes read-modify-write of the profile map: the settings command
/// (EQ) and the player worker (volume, device switch) write it from
/// different threads.
static WRITE: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProfile {
    pub dsp: DspParams,
    /// None until the volume was first changed on this device.
    #[serde(default)]
    pub volume: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioProfiles {
    pub enabled: bool,
    pub profiles: BTreeMap<String, DeviceProfile>,
}

/// What Settings shows: the switch, the device playing right now and the
/// devices that have a profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioProfilesInfo {
    pub enabled: bool,
    pub active_device: Option<String>,
    pub devices: Vec<String>,
}

/// Sent when opening a device swapped the sound settings for its profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileAppliedEvent {
    pub device: String,
}

pub fn load(store: &Store) -> AudioProfiles {
    store
        .get(keys::AUDIO_PROFILES)
        .and_then(|json| serde_json::from_str::<AudioProfiles>(&json).ok())
        .unwrap_or_default()
}

fn save(store: &Store, profiles: &AudioProfiles) -> AppResult<()> {
    let json = serde_json::to_string(profiles)
        .map_err(|e| AppError::Other(format!("cannot serialize audio profiles: {e}")))?;
    store.set(keys::AUDIO_PROFILES, &json)
}

fn update(store: &Store, change: impl FnOnce(&mut AudioProfiles)) -> AppResult<AudioProfiles> {
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut profiles = load(store);
    let before = profiles.clone();
    change(&mut profiles);
    if profiles != before {
        save(store, &profiles)?;
    }
    Ok(profiles)
}

pub fn info(store: &Store) -> AudioProfilesInfo {
    let profiles = load(store);
    AudioProfilesInfo {
        enabled: profiles.enabled,
        active_device: crate::audio_devices::active_device(),
        devices: profiles.profiles.keys().cloned().collect(),
    }
}

/// Switching on starts with the current settings as the profile of the
/// device playing now. Switching off keeps the saved profiles for later.
pub fn set_enabled(
    store: &Store,
    enabled: bool,
    current: &DspParams,
    volume: f32,
) -> AppResult<()> {
    let device = crate::audio_devices::active_device();
    update(store, |p| {
        p.enabled = enabled;
        if let (true, Some(device)) = (enabled, device) {
            p.profiles.entry(device).or_insert_with(|| DeviceProfile {
                dsp: current.clone(),
                volume: Some(volume),
            });
        }
    })
    .map(|_| ())
}

pub fn delete(store: &Store, device: &str) -> AppResult<()> {
    update(store, |p| {
        p.profiles.remove(device);
    })
    .map(|_| ())
}

/// The EQ/normalization settings changed: they now belong to the active
/// device (no-op while profiles are off).
pub fn remember_dsp(store: &Store, params: &DspParams) -> AppResult<()> {
    let Some(device) = crate::audio_devices::active_device() else {
        return Ok(());
    };
    update(store, |p| {
        if !p.enabled {
            return;
        }
        match p.profiles.get_mut(&device) {
            Some(profile) => profile.dsp = params.clone(),
            None => {
                p.profiles.insert(
                    device,
                    DeviceProfile {
                        dsp: params.clone(),
                        volume: None,
                    },
                );
            }
        }
    })
    .map(|_| ())
}

pub fn remember_volume(
    store: &Store,
    device: &str,
    volume: f32,
    current: &DspParams,
) -> AppResult<()> {
    update(store, |p| {
        if !p.enabled {
            return;
        }
        p.profiles
            .entry(device.to_string())
            .or_insert_with(|| DeviceProfile {
                dsp: current.clone(),
                volume: None,
            })
            .volume = Some(volume);
    })
    .map(|_| ())
}

/// The player opened `device`. Returns the profile to switch to, if the
/// device has one; a device seen for the first time adopts the current
/// settings as its profile, so they come back after a trip elsewhere.
pub fn on_device_opened(
    store: &Store,
    device: &str,
    current: &DspParams,
    volume: f32,
) -> AppResult<Option<DeviceProfile>> {
    let profiles = update(store, |p| {
        if p.enabled && !p.profiles.contains_key(device) {
            p.profiles.insert(
                device.to_string(),
                DeviceProfile {
                    dsp: current.clone(),
                    volume: Some(volume),
                },
            );
        }
    })?;
    if !profiles.enabled {
        return Ok(None);
    }
    Ok(profiles
        .profiles
        .get(device)
        .cloned()
        .filter(|profile| profile.dsp != *current || profile.volume.is_some_and(|v| v != volume)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sonarune-profiles-{}", uuid::Uuid::new_v4()));
        (Store::open(&dir).expect("open store"), dir)
    }

    fn eq(gain: f32) -> DspParams {
        DspParams {
            eq_enabled: true,
            eq_gains_db: [gain; 10],
            ..DspParams::default()
        }
    }

    #[test]
    fn profiles_stay_inert_while_switched_off() {
        let (store, dir) = store();
        assert_eq!(
            on_device_opened(&store, "Speakers", &eq(1.0), 0.5).unwrap(),
            None
        );
        remember_volume(&store, "Speakers", 0.3, &eq(1.0)).unwrap();
        assert!(load(&store).profiles.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn each_device_gets_its_own_settings_back() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();

        // First visit: the device adopts what is playing, nothing to switch.
        assert_eq!(
            on_device_opened(&store, "Speakers", &eq(1.0), 0.8).unwrap(),
            None
        );
        assert_eq!(
            on_device_opened(&store, "Headphones", &eq(1.0), 0.8).unwrap(),
            None
        );
        // Tuned on the headphones.
        remember_volume(&store, "Headphones", 0.3, &eq(1.0)).unwrap();
        update(&store, |p| {
            p.profiles.get_mut("Headphones").unwrap().dsp = eq(-4.0)
        })
        .unwrap();

        // Back to the speakers: their own curve and level.
        let back = on_device_opened(&store, "Speakers", &eq(-4.0), 0.3)
            .unwrap()
            .unwrap();
        assert_eq!((back.dsp, back.volume), (eq(1.0), Some(0.8)));
        // And the headphones again.
        let again = on_device_opened(&store, "Headphones", &eq(1.0), 0.8)
            .unwrap()
            .unwrap();
        assert_eq!((again.dsp, again.volume), (eq(-4.0), Some(0.3)));
        // Already matching: no switch, no event.
        assert_eq!(
            on_device_opened(&store, "Headphones", &eq(-4.0), 0.3).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_a_profile_lets_the_device_start_over() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        on_device_opened(&store, "DAC", &eq(2.0), 1.0).unwrap();
        delete(&store, "DAC").unwrap();
        assert!(load(&store).profiles.is_empty());
        assert_eq!(
            on_device_opened(&store, "DAC", &eq(0.0), 1.0).unwrap(),
            None
        );
        assert_eq!(load(&store).profiles["DAC"].dsp, eq(0.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_profile_without_a_volume_leaves_the_volume_alone() {
        let profile: DeviceProfile = serde_json::from_str(&format!(
            "{{\"dsp\":{}}}",
            serde_json::to_string(&eq(1.0)).unwrap()
        ))
        .unwrap();
        assert_eq!(profile.volume, None);
    }
}
