use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// One output device, as Settings lists it and the player opens it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    /// cpal's device id, on Windows the endpoint id
    /// ("wasapi:{0.0.0.00000000}.{…}"): unique where names are not — Windows
    /// calls the onboard output and a USB headset both "Speakers" — and kept
    /// across restarts. Not for good, though: Windows makes a new endpoint,
    /// under a new id and mostly the old name, for a re-paired Bluetooth
    /// device, a USB device without a serial number in another port, or a
    /// driver that has one endpoint per mode (a Focusrite interface per
    /// sample rate). The output setting and the per-device profiles store
    /// the id and find such a device again by its name (`player::resolve_output`,
    /// `audio_profiles::on_device_opened`).
    pub id: String,
    /// What the system's sound settings show ("Speakers (Realtek(R) Audio)").
    pub name: String,
}

/// Runtime-only output state. The persisted playback setting remains the
/// user's requested device even while audio is temporarily routed through the
/// system default. Holds that stored value (a device id).
static FALLBACK_DEVICE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn fallback_device_slot() -> &'static Mutex<Option<String>> {
    FALLBACK_DEVICE.get_or_init(|| Mutex::new(None))
}

/// The device the open output actually plays through (resolved from "system
/// default" too). Keys the per-device sound profiles. Kept while the output
/// is closed after a long pause: the music goes on there.
static ACTIVE_DEVICE: Mutex<Option<OutputDevice>> = Mutex::new(None);

pub fn set_active_device(device: Option<OutputDevice>) {
    *ACTIVE_DEVICE.lock().unwrap() = device;
}

pub fn active_device() -> Option<OutputDevice> {
    ACTIVE_DEVICE.lock().unwrap().clone()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceSnapshot {
    pub devices: Vec<OutputDevice>,
    pub fallback_device: Option<String>,
    pub active_device: Option<OutputDevice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioOutputFallbackEvent {
    /// Name of the configured device the output could not stay on.
    pub requested_device: String,
}

pub fn snapshot() -> AudioDeviceSnapshot {
    AudioDeviceSnapshot {
        devices: crate::player::output_devices(),
        fallback_device: fallback_device(),
        active_device: active_device(),
    }
}

pub fn mark_fallback(device: String) {
    *fallback_device_slot().lock().unwrap() = Some(device);
}

pub fn clear_fallback() {
    *fallback_device_slot().lock().unwrap() = None;
}

pub fn fallback_device() -> Option<String> {
    fallback_device_slot().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_state_is_explicit_and_clearable() {
        clear_fallback();
        assert_eq!(fallback_device(), None);

        mark_fallback("USB DAC".to_string());
        assert_eq!(fallback_device().as_deref(), Some("USB DAC"));

        clear_fallback();
        assert_eq!(fallback_device(), None);
    }
}
