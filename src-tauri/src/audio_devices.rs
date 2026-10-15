use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// Runtime-only output state. The persisted playback setting remains the
/// user's requested device even while audio is temporarily routed through the
/// system default.
static FALLBACK_DEVICE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn fallback_device_slot() -> &'static Mutex<Option<String>> {
    FALLBACK_DEVICE.get_or_init(|| Mutex::new(None))
}

/// The device the open output actually plays through (resolved from "system
/// default" too). Keys the per-device sound profiles.
static ACTIVE_DEVICE: Mutex<Option<String>> = Mutex::new(None);

pub fn set_active_device(device: Option<String>) {
    *ACTIVE_DEVICE.lock().unwrap() = device;
}

pub fn active_device() -> Option<String> {
    ACTIVE_DEVICE.lock().unwrap().clone()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceSnapshot {
    pub devices: Vec<String>,
    pub fallback_device: Option<String>,
    pub active_device: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioOutputFallbackEvent {
    pub requested_device: String,
}

pub fn snapshot() -> AudioDeviceSnapshot {
    AudioDeviceSnapshot {
        devices: crate::player::output_device_names(),
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
