//! Sound profile per output device: EQ, preamp, normalization and volume are
//! remembered for each device and come back when audio plays through it
//! again — headphones and speakers rarely want the same curve or level.
//!
//! The device is the one the player actually opened (a configured device, or
//! whatever "system default" resolved to, including a fallback), never just
//! the preference: the profile has to match what is heard. Profiles are keyed
//! by the device id (`OutputDevice::id`); builds before 0.6.1 keyed them by
//! cpal's bare endpoint name, which two devices can share ("Speakers"), and
//! such a profile moves to the first device of that name that opens.

use crate::audio_devices::OutputDevice;
use crate::error::{AppError, AppResult};
use crate::player::DspParams;
use crate::store::{keys, Store};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
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
    /// The device's name when it last played, so Settings can show the
    /// profile while the device is away. Absent in profiles from before
    /// 0.6.1, whose key is the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The stored profiles: `{"enabled": …, "profiles": {device id: profile}}`.
/// Each profile is read on its own, so one this build cannot read — damaged,
/// hand-edited, or written by a build that knows more — costs only itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioProfiles {
    pub enabled: bool,
    pub profiles: BTreeMap<String, DeviceProfile>,
    /// The entries that could not be read, as they are stored. They are
    /// written back untouched, until a profile of this build takes the
    /// device over.
    unreadable: BTreeMap<String, serde_json::Value>,
}

impl Serialize for AudioProfiles {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeStruct};
        let mut profiles: serde_json::Map<String, serde_json::Value> = self
            .unreadable
            .iter()
            .filter(|(id, _)| !self.profiles.contains_key(*id))
            .map(|(id, raw)| (id.clone(), raw.clone()))
            .collect();
        for (id, profile) in &self.profiles {
            profiles.insert(
                id.clone(),
                serde_json::to_value(profile).map_err(S::Error::custom)?,
            );
        }
        let mut out = serializer.serialize_struct("AudioProfiles", 2)?;
        out.serialize_field("enabled", &self.enabled)?;
        out.serialize_field("profiles", &profiles)?;
        out.end()
    }
}

impl<'de> Deserialize<'de> for AudioProfiles {
    /// Fails only when nothing can be read: not an object, or one whose
    /// `profiles` is not a map. A missing field takes its default, as it
    /// always did; an `enabled` that is not a flag reads as off.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_json::Value;
        let Value::Object(mut stored) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("the sound profiles are not an object"));
        };
        let mut read = Self {
            enabled: stored
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or_default(),
            ..Self::default()
        };
        match stored.remove("profiles") {
            None | Some(Value::Null) => {}
            Some(Value::Object(entries)) => {
                for (id, raw) in entries {
                    match DeviceProfile::deserialize(&raw) {
                        Ok(profile) => {
                            read.profiles.insert(id, profile);
                        }
                        Err(_) => {
                            read.unreadable.insert(id, raw);
                        }
                    }
                }
            }
            Some(_) => return Err(D::Error::custom("the sound profiles are not a map")),
        }
        Ok(read)
    }
}

/// What Settings shows: the switch, the device playing right now and the
/// devices that have a profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioProfilesInfo {
    pub enabled: bool,
    pub active_device: Option<OutputDevice>,
    /// By name; the id is what `delete` takes.
    pub devices: Vec<OutputDevice>,
}

/// Sent when opening a device swapped the sound settings for its profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileAppliedEvent {
    /// The device's name.
    pub device: String,
}

pub fn load(store: &Store) -> AudioProfiles {
    read(store).unwrap_or_default()
}

/// The stored profiles; `Err` with the stored value when nothing in it can
/// be read.
fn read(store: &Store) -> Result<AudioProfiles, String> {
    match store.get(keys::AUDIO_PROFILES) {
        None => Ok(AudioProfiles::default()),
        Some(json) => serde_json::from_str(&json).map_err(|_| json),
    }
}

fn save(store: &Store, profiles: &AudioProfiles) -> AppResult<()> {
    let json = serde_json::to_string(profiles)
        .map_err(|e| AppError::Other(format!("cannot serialize audio profiles: {e}")))?;
    store.set(keys::AUDIO_PROFILES, &json)
}

fn update(store: &Store, change: impl FnOnce(&mut AudioProfiles)) -> AppResult<AudioProfiles> {
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let (mut profiles, unreadable) = match read(store) {
        Ok(profiles) => (profiles, None),
        Err(raw) => (AudioProfiles::default(), Some(raw)),
    };
    let before = profiles.clone();
    change(&mut profiles);
    if profiles != before {
        // Nothing in what is stored could be read: it is set aside before
        // the first profile is saved over it, not lost. A value set aside
        // before stays: the first is the likelier to hold the profiles.
        if let Some(raw) = unreadable {
            tracing::warn!("the stored sound profiles cannot be read; set aside, starting over");
            if store.get(keys::AUDIO_PROFILES_UNREADABLE).is_none() {
                store.set(keys::AUDIO_PROFILES_UNREADABLE, &raw)?;
            }
        }
        save(store, &profiles)?;
    }
    Ok(profiles)
}

pub fn info(store: &Store, active_device: Option<OutputDevice>) -> AudioProfilesInfo {
    let profiles = load(store);
    let mut devices: Vec<OutputDevice> = profiles
        .profiles
        .into_iter()
        .map(|(id, profile)| OutputDevice {
            name: profile.name.unwrap_or_else(|| id.clone()),
            id,
        })
        .collect();
    devices.sort_by_key(|device| device.name.to_lowercase());
    AudioProfilesInfo {
        enabled: profiles.enabled,
        active_device,
        devices,
    }
}

/// Switching on makes the current settings the profile of the device playing
/// now — also over a profile it kept from before profiles were last switched
/// off: that one has not followed any change since, and would bring an old
/// sound back at the next start. Switching off keeps the saved profiles for
/// later.
pub fn set_enabled(
    store: &Store,
    enabled: bool,
    device: Option<&OutputDevice>,
    current: &DspParams,
    volume: f32,
) -> AppResult<()> {
    update(store, |p| {
        p.enabled = enabled;
        if let (true, Some(device)) = (enabled, device) {
            p.profiles.insert(
                device.id.clone(),
                DeviceProfile {
                    dsp: current.clone(),
                    volume: Some(volume),
                    name: Some(device.name.clone()),
                },
            );
        }
    })
    .map(|_| ())
}

pub fn delete(store: &Store, device_id: &str) -> AppResult<()> {
    update(store, |p| {
        p.profiles.remove(device_id);
        p.unreadable.remove(device_id);
    })
    .map(|_| ())
}

/// The EQ/normalization settings changed: they now belong to `device`, the
/// one playing (no-op while profiles are off).
pub fn remember_dsp(store: &Store, device: &OutputDevice, params: &DspParams) -> AppResult<()> {
    update(store, |p| {
        if !p.enabled {
            return;
        }
        match p.profiles.get_mut(&device.id) {
            Some(profile) => profile.dsp = params.clone(),
            None => {
                p.profiles.insert(
                    device.id.clone(),
                    DeviceProfile {
                        dsp: params.clone(),
                        volume: None,
                        name: Some(device.name.clone()),
                    },
                );
            }
        }
    })
    .map(|_| ())
}

pub fn remember_volume(
    store: &Store,
    device: &OutputDevice,
    volume: f32,
    current: &DspParams,
) -> AppResult<()> {
    update(store, |p| {
        if !p.enabled {
            return;
        }
        p.profiles
            .entry(device.id.clone())
            .or_insert_with(|| DeviceProfile {
                dsp: current.clone(),
                volume: None,
                name: Some(device.name.clone()),
            })
            .volume = Some(volume);
    })
    .map(|_| ())
}

/// The player opened `device`. Returns the profile to switch to, if the
/// device has one; a device seen for the first time adopts the current
/// settings as its profile, so they come back after a trip elsewhere.
///
/// A device without a profile under its id may have one under a key it had
/// before, and the profile moves over:
/// - `legacy_name` is cpal's bare endpoint name of the device, the key an
///   older build saved its profile under;
/// - Windows lists a device under a new id now and then (see
///   `OutputDevice::id`), mostly with the name it had. A profile saved under
///   the device's name for an id that is not listed now (`listed`: the ids
///   of the devices listed now, None when unknown) is the device's own. Two
///   such profiles of one name are left alone, and so is the profile of a
///   device that is listed: two devices of one name keep two profiles.
pub fn on_device_opened(
    store: &Store,
    device: &OutputDevice,
    legacy_name: Option<&str>,
    listed: Option<&[String]>,
    current: &DspParams,
    volume: f32,
) -> AppResult<Option<DeviceProfile>> {
    let profiles = update(store, |p| {
        if !p.profiles.contains_key(&device.id) {
            let former = legacy_name
                .filter(|name| p.profiles.contains_key(*name))
                .map(str::to_string)
                .or_else(|| former_id(p, device, listed?));
            if let Some(profile) = former.and_then(|key| p.profiles.remove(&key)) {
                p.profiles.insert(device.id.clone(), profile);
            }
        }
        if p.enabled && !p.profiles.contains_key(&device.id) {
            p.profiles.insert(
                device.id.clone(),
                DeviceProfile {
                    dsp: current.clone(),
                    volume: Some(volume),
                    name: Some(device.name.clone()),
                },
            );
        }
        // Kept current, for Settings while the device is away (a renamed
        // endpoint shows its new name).
        if let Some(profile) = p.profiles.get_mut(&device.id) {
            if profile.name.as_deref() != Some(device.name.as_str()) {
                profile.name = Some(device.name.clone());
            }
        }
    })?;
    if !profiles.enabled {
        return Ok(None);
    }
    Ok(profiles
        .profiles
        .get(&device.id)
        .cloned()
        .filter(|profile| profile.dsp != *current || profile.volume.is_some_and(|v| v != volume)))
}

/// The one profile saved under `device`'s name for a device not listed now.
fn former_id(profiles: &AudioProfiles, device: &OutputDevice, listed: &[String]) -> Option<String> {
    let mut former = profiles
        .profiles
        .iter()
        .filter(|(id, profile)| {
            profile.name.as_deref() == Some(device.name.as_str()) && !listed.contains(id)
        })
        .map(|(id, _)| id);
    match (former.next(), former.next()) {
        (Some(id), None) => Some(id.clone()),
        _ => None,
    }
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

    fn device(id: &str, name: &str) -> OutputDevice {
        OutputDevice {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn speakers() -> OutputDevice {
        device("wasapi:{speakers}", "Speakers (Realtek(R) Audio)")
    }

    fn headphones() -> OutputDevice {
        device("wasapi:{headphones}", "Headphones (USB Audio)")
    }

    #[test]
    fn profiles_stay_inert_while_switched_off() {
        let (store, dir) = store();
        assert_eq!(
            on_device_opened(&store, &speakers(), Some("Speakers"), None, &eq(1.0), 0.5).unwrap(),
            None
        );
        remember_volume(&store, &speakers(), 0.3, &eq(1.0)).unwrap();
        remember_dsp(&store, &speakers(), &eq(2.0)).unwrap();
        assert!(load(&store).profiles.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn each_device_gets_its_own_settings_back() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();

        // First visit: the device adopts what is playing, nothing to switch.
        assert_eq!(
            on_device_opened(&store, &speakers(), None, None, &eq(1.0), 0.8).unwrap(),
            None
        );
        assert_eq!(
            on_device_opened(&store, &headphones(), None, None, &eq(1.0), 0.8).unwrap(),
            None
        );
        // Tuned on the headphones.
        remember_volume(&store, &headphones(), 0.3, &eq(1.0)).unwrap();
        remember_dsp(&store, &headphones(), &eq(-4.0)).unwrap();

        // Back to the speakers: their own curve and level.
        let back = on_device_opened(&store, &speakers(), None, None, &eq(-4.0), 0.3)
            .unwrap()
            .unwrap();
        assert_eq!((back.dsp, back.volume), (eq(1.0), Some(0.8)));
        // And the headphones again.
        let again = on_device_opened(&store, &headphones(), None, None, &eq(1.0), 0.8)
            .unwrap()
            .unwrap();
        assert_eq!((again.dsp, again.volume), (eq(-4.0), Some(0.3)));
        // Already matching: no switch, no event.
        assert_eq!(
            on_device_opened(&store, &headphones(), None, None, &eq(-4.0), 0.3).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_devices_with_one_name_keep_two_profiles() {
        // Windows names the onboard output and a USB headset both
        // "Speakers"; by id they are two devices with two sounds.
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        let onboard = device("wasapi:{onboard}", "Speakers (Realtek(R) Audio)");
        let usb = device("wasapi:{usb}", "Speakers (USB Audio Device)");
        on_device_opened(&store, &onboard, Some("Speakers"), None, &eq(3.0), 0.9).unwrap();
        on_device_opened(&store, &usb, Some("Speakers"), None, &eq(3.0), 0.9).unwrap();
        remember_dsp(&store, &usb, &eq(-6.0)).unwrap();

        let back = on_device_opened(&store, &onboard, Some("Speakers"), None, &eq(-6.0), 0.9)
            .unwrap()
            .unwrap();
        assert_eq!(back.dsp, eq(3.0));
        let names: Vec<String> = info(&store, None)
            .devices
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            names,
            ["Speakers (Realtek(R) Audio)", "Speakers (USB Audio Device)"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_profile_saved_under_the_old_name_moves_to_the_device_id() {
        // 0.6.0 keyed profiles by cpal's bare endpoint name.
        let (store, dir) = store();
        let legacy = format!(
            r#"{{"enabled":true,"profiles":{{"Speakers":{{"dsp":{},"volume":0.4}}}}}}"#,
            serde_json::to_string(&eq(5.0)).unwrap()
        );
        store.set(keys::AUDIO_PROFILES, &legacy).unwrap();

        let applied = on_device_opened(&store, &speakers(), Some("Speakers"), None, &eq(0.0), 1.0)
            .unwrap()
            .unwrap();
        assert_eq!((applied.dsp, applied.volume), (eq(5.0), Some(0.4)));
        let profiles = load(&store).profiles;
        assert_eq!(profiles.keys().collect::<Vec<_>>(), [&speakers().id]);
        assert_eq!(
            profiles[&speakers().id].name.as_deref(),
            Some("Speakers (Realtek(R) Audio)")
        );
        // Taken once: a second device of that name starts with its own.
        on_device_opened(&store, &headphones(), Some("Speakers"), None, &eq(5.0), 0.4).unwrap();
        assert_eq!(load(&store).profiles.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn ids(devices: &[&OutputDevice]) -> Vec<String> {
        devices.iter().map(|device| device.id.clone()).collect()
    }

    #[test]
    fn a_device_under_a_new_id_keeps_its_profile() {
        // Windows made a new endpoint for the same device (a Focusrite
        // interface switched to 48 kHz, a re-paired Bluetooth headset, a
        // USB headset without a serial number in another port): new id,
        // same name.
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        let name = "Speakers (Focusrite USB Audio)";
        let (at_44k, at_48k) = (device("wasapi:{44k}", name), device("wasapi:{48k}", name));
        on_device_opened(&store, &at_44k, None, Some(&ids(&[&at_44k])), &eq(1.0), 0.8).unwrap();
        remember_dsp(&store, &at_44k, &eq(-2.0)).unwrap();

        let applied =
            on_device_opened(&store, &at_48k, None, Some(&ids(&[&at_48k])), &eq(9.0), 0.5)
                .unwrap()
                .expect("the device's own sound comes back");
        assert_eq!((applied.dsp, applied.volume), (eq(-2.0), Some(0.8)));
        assert_eq!(
            info(&store, None).devices,
            std::slice::from_ref(&at_48k),
            "one device, one profile"
        );
        // And back under the first id: the profile follows again.
        remember_dsp(&store, &at_48k, &eq(-3.0)).unwrap();
        let back = on_device_opened(&store, &at_44k, None, Some(&ids(&[&at_44k])), &eq(0.0), 0.8)
            .unwrap()
            .unwrap();
        assert_eq!(back.dsp, eq(-3.0));
        assert_eq!(
            load(&store).profiles.keys().collect::<Vec<_>>(),
            [&at_44k.id]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_profile_moves_only_from_a_device_that_is_gone() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        let name = "Headphones (USB Audio Device)";
        let (first, second, third) = (
            device("wasapi:{first}", name),
            device("wasapi:{second}", name),
            device("wasapi:{third}", name),
        );
        on_device_opened(&store, &first, None, Some(&ids(&[&first])), &eq(1.0), 0.8).unwrap();

        // Two such headsets plugged in at once are two devices.
        let both = ids(&[&first, &second]);
        assert_eq!(
            on_device_opened(&store, &second, None, Some(&both), &eq(4.0), 0.3).unwrap(),
            None,
            "the second one starts with what plays"
        );
        assert_eq!(load(&store).profiles[&first.id].dsp, eq(1.0));
        assert_eq!(load(&store).profiles[&second.id].dsp, eq(4.0));

        // Both away, a third id: which one it was is anyone's guess.
        on_device_opened(&store, &third, None, Some(&ids(&[&third])), &eq(7.0), 0.5).unwrap();
        assert_eq!(load(&store).profiles.len(), 3);
        assert_eq!(load(&store).profiles[&third.id].dsp, eq(7.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_a_device_list_no_profile_moves() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        let name = "Speakers (Focusrite USB Audio)";
        let (old, new) = (device("wasapi:{old}", name), device("wasapi:{new}", name));
        on_device_opened(&store, &old, None, None, &eq(1.0), 0.8).unwrap();
        on_device_opened(&store, &new, None, None, &eq(2.0), 0.8).unwrap();
        assert_eq!(load(&store).profiles.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn switching_on_again_keeps_the_sound_that_plays_now() {
        // On with EQ A, off, EQ B, on again: B is what plays and what the
        // device's profile has to say, or the next start brings A back.
        let (store, dir) = store();
        set_enabled(&store, true, Some(&speakers()), &eq(2.0), 0.8).unwrap();
        set_enabled(&store, false, Some(&speakers()), &eq(2.0), 0.8).unwrap();
        set_enabled(&store, true, Some(&speakers()), &eq(-3.0), 0.5).unwrap();
        assert_eq!(
            on_device_opened(&store, &speakers(), None, None, &eq(-3.0), 0.5).unwrap(),
            None,
            "the next device open must not revert to the old profile"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_a_profile_lets_the_device_start_over() {
        let (store, dir) = store();
        update(&store, |p| p.enabled = true).unwrap();
        let dac = device("wasapi:{dac}", "DAC");
        on_device_opened(&store, &dac, None, None, &eq(2.0), 1.0).unwrap();
        delete(&store, &dac.id).unwrap();
        assert!(load(&store).profiles.is_empty());
        assert_eq!(
            on_device_opened(&store, &dac, None, None, &eq(0.0), 1.0).unwrap(),
            None
        );
        assert_eq!(load(&store).profiles[&dac.id].dsp, eq(0.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_profile_costs_only_itself() {
        let (store, dir) = store();
        let broken = device("wasapi:{broken}", "Broken DAC");
        let stored = serde_json::json!({
            "enabled": true,
            "profiles": {
                speakers().id: { "dsp": eq(2.0), "volume": 0.5 },
                // A curve of the wrong length: hand-edited, or damaged.
                broken.id.clone(): { "dsp": { "eqGainsDb": [1.0] } },
            },
        });
        store
            .set(keys::AUDIO_PROFILES, &stored.to_string())
            .unwrap();
        let loaded = load(&store);
        assert!(loaded.enabled);
        assert_eq!(loaded.profiles.keys().collect::<Vec<_>>(), [&speakers().id]);
        // The readable one works as ever.
        let applied = on_device_opened(&store, &speakers(), None, None, &eq(0.0), 1.0)
            .unwrap()
            .expect("the speakers' own sound");
        assert_eq!((applied.dsp, applied.volume), (eq(2.0), Some(0.5)));
        // Saving keeps the unreadable one as it was.
        remember_dsp(&store, &speakers(), &eq(3.0)).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&store.get(keys::AUDIO_PROFILES).unwrap()).unwrap();
        assert_eq!(
            saved["profiles"][&broken.id],
            stored["profiles"][&broken.id]
        );
        // Its device starts over with what plays.
        assert_eq!(
            on_device_opened(&store, &broken, None, None, &eq(1.0), 0.7).unwrap(),
            None
        );
        assert_eq!(load(&store).profiles[&broken.id].dsp, eq(1.0));
        assert_eq!(load(&store).profiles[&speakers().id].dsp, eq(3.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_value_nothing_can_be_read_from_is_set_aside_before_it_is_saved_over() {
        let (store, dir) = store();
        store.set(keys::AUDIO_PROFILES, "{\"enabled\":tru").unwrap();
        assert_eq!(load(&store), AudioProfiles::default());
        // Looking changes nothing.
        let _ = info(&store, None);
        on_device_opened(&store, &speakers(), None, None, &eq(0.0), 1.0).unwrap();
        assert_eq!(store.get(keys::AUDIO_PROFILES_UNREADABLE), None);
        // The first profile saved puts it aside.
        set_enabled(&store, true, Some(&speakers()), &eq(1.0), 0.8).unwrap();
        assert_eq!(
            store.get(keys::AUDIO_PROFILES_UNREADABLE).as_deref(),
            Some("{\"enabled\":tru")
        );
        assert!(load(&store).enabled);
        // A second such value does not push the first one out.
        store.set(keys::AUDIO_PROFILES, "[]").unwrap();
        set_enabled(&store, true, Some(&speakers()), &eq(1.0), 0.8).unwrap();
        assert_eq!(
            store.get(keys::AUDIO_PROFILES_UNREADABLE).as_deref(),
            Some("{\"enabled\":tru")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saved_profiles_read_in_older_builds() {
        // 0.6.0 and 0.6.1 read the value whole, as this type.
        #[derive(Debug, Default, Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        struct Before {
            enabled: bool,
            profiles: BTreeMap<String, DeviceProfile>,
        }
        let (store, dir) = store();
        set_enabled(&store, true, Some(&speakers()), &eq(1.0), 0.8).unwrap();
        remember_volume(&store, &headphones(), 0.3, &eq(-2.0)).unwrap();
        let before: Before = serde_json::from_str(&store.get(keys::AUDIO_PROFILES).unwrap())
            .expect("an older build reads it");
        assert!(before.enabled);
        assert_eq!(before.profiles, load(&store).profiles);
        // An entry this build could not read either is kept as it was: an
        // older build reads none of the value then, which is how it read
        // that value before.
        let mut stored: serde_json::Value =
            serde_json::from_str(&store.get(keys::AUDIO_PROFILES).unwrap()).unwrap();
        stored["profiles"]["wasapi:{broken}"] = serde_json::json!({ "dsp": 1 });
        store
            .set(keys::AUDIO_PROFILES, &stored.to_string())
            .unwrap();
        remember_dsp(&store, &speakers(), &eq(4.0)).unwrap();
        let saved = store.get(keys::AUDIO_PROFILES).unwrap();
        assert!(saved.contains(r#""wasapi:{broken}":{"dsp":1}"#), "{saved}");
        assert!(serde_json::from_str::<Before>(&saved).is_err());
        assert_eq!(load(&store).profiles[&speakers().id].dsp, eq(4.0));
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
        assert_eq!(profile.name, None);
    }
}
