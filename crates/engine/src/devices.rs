// SPDX-License-Identifier: MPL-2.0
//! Read-only native OS device discovery, separate from the selected microphone profile.

use cosmos_control::{AudioDevice, DeviceProfile, Phase, ProfileAvailability};
use pipewire as pw;
use pw::permissions::PermissionFlags;
use pw::spa::param::{ParamInfoFlags, ParamType};
use pw::spa::pod::Pod;

use super::{Observed, Properties};

const MAX_PROFILES: usize = 128;
const MAX_PROFILE_POD_BYTES: usize = 64 * 1024;
pub(super) struct DeviceListeners {
    pub _device: pw::device::DeviceListener,
    pub _proxy: pw::proxy::ProxyListener,
}

impl pw::proxy::Listener for DeviceListeners {}

#[derive(Default)]
pub(super) struct DeviceRecord {
    pub properties: Properties,
    pub permissions: Option<PermissionFlags>,
    pub profiles: std::collections::BTreeMap<i32, DeviceProfile>,
    pub current: Option<i32>,
    pub enum_readable: bool,
    pub profile_readable: bool,
    pub error: Option<String>,
}

impl DeviceRecord {
    pub fn update_flags(&mut self, id: ParamType, flags: ParamInfoFlags) {
        match id {
            ParamType::EnumProfile => {
                self.enum_readable = flags.contains(ParamInfoFlags::READ);
                if !self.enum_readable {
                    self.profiles.clear();
                }
            }
            ParamType::Profile => {
                self.profile_readable = flags.contains(ParamInfoFlags::READ);
                if !self.profile_readable {
                    self.current = None;
                }
            }
            _ => {}
        }
    }

    pub fn readable_params(&self) -> &'static [ParamType] {
        if !self
            .permissions
            .is_some_and(|flags| flags.contains(PermissionFlags::R | PermissionFlags::X))
        {
            return &[];
        }
        match (self.enum_readable, self.profile_readable) {
            (true, true) => &[ParamType::EnumProfile, ParamType::Profile],
            (true, false) => &[ParamType::EnumProfile],
            (false, true) => &[ParamType::Profile],
            (false, false) => &[],
        }
    }

    fn readable(&self) -> bool {
        self.readable_params().len() == 2
    }

    fn serial(&self) -> Option<u64> {
        self.properties
            .get("object.serial")
            .and_then(|value| value.parse().ok())
    }

    pub fn parameter(&mut self, id: ParamType, index: u32, pod: Option<&Pod>) {
        if !self.readable_params().contains(&id) {
            return;
        }
        if id == ParamType::EnumProfile && index == 0 {
            self.profiles.clear();
        }
        let Some(pod) = pod else {
            if id == ParamType::Profile {
                self.current = None;
            }
            return;
        };
        match parse_profile(pod) {
            Ok(profile) if id == ParamType::Profile => self.current = Some(profile.index),
            Ok(profile) => {
                if self.profiles.len() < MAX_PROFILES || self.profiles.contains_key(&profile.index)
                {
                    self.profiles.insert(profile.index, profile);
                } else {
                    self.error = Some(format!(
                        "device profile discovery limited to {MAX_PROFILES} choices"
                    ));
                }
            }
            Err(error) => {
                if id == ParamType::Profile {
                    self.current = None;
                }
                self.error = Some(error);
            }
        }
    }

    fn snapshot(&self, id: u32, generation: u64) -> AudioDevice {
        AudioDevice {
            id,
            generation,
            serial: self.serial(),
            name: self
                .properties
                .get("device.name")
                .cloned()
                .unwrap_or_default(),
            description: self
                .properties
                .get("device.description")
                .or_else(|| self.properties.get("device.nick"))
                .cloned()
                .unwrap_or_default(),
            current_profile: self.current,
            profiles: self.profiles.values().cloned().collect(),
            disabled: self
                .current
                .and_then(|index| self.profiles.get(&index))
                .is_some_and(|profile| profile.name == "off"),
            readable: self.readable(),
            error: self.error.clone(),
        }
    }
}

fn parse_profile(pod: &Pod) -> Result<DeviceProfile, String> {
    if pod.as_bytes().len() > MAX_PROFILE_POD_BYTES {
        return Err("oversized native device profile ignored".into());
    }
    let object = pod
        .as_object()
        .map_err(|_| "native device profile is not an object".to_owned())?;
    if object.type_().as_raw() != pw::spa::sys::SPA_TYPE_OBJECT_ParamProfile {
        return Err("native device profile has the wrong object type".into());
    }
    let mut index = None;
    let mut name = String::new();
    let mut description = String::new();
    let mut availability = ProfileAvailability::Unknown;
    for property in object.props() {
        let value = property.value();
        match property.key().0 {
            pw::spa::sys::SPA_PARAM_PROFILE_index => {
                index = value.get_int().ok().filter(|index| *index >= 0)
            }
            pw::spa::sys::SPA_PARAM_PROFILE_name => {
                name = value
                    .get_string_raw()
                    .ok()
                    .flatten()
                    .and_then(|value| value.to_str().ok())
                    .map(bounded)
                    .unwrap_or_default();
            }
            pw::spa::sys::SPA_PARAM_PROFILE_description => {
                description = value
                    .get_string_raw()
                    .ok()
                    .flatten()
                    .and_then(|value| value.to_str().ok())
                    .map(bounded)
                    .unwrap_or_default();
            }
            pw::spa::sys::SPA_PARAM_PROFILE_available => {
                availability = match value.get_id().ok().map(|value| value.0) {
                    Some(pw::spa::sys::SPA_PARAM_AVAILABILITY_no) => ProfileAvailability::No,
                    Some(pw::spa::sys::SPA_PARAM_AVAILABILITY_yes) => ProfileAvailability::Yes,
                    _ => ProfileAvailability::Unknown,
                };
            }
            _ => {}
        }
    }
    Ok(DeviceProfile {
        index: index.ok_or("native device profile lacks a valid index")?,
        name,
        description,
        availability,
    })
}

fn bounded(value: &str) -> String {
    value.chars().take(512).collect()
}

impl Observed {
    pub fn device_snapshot(&self) -> Vec<AudioDevice> {
        if self.status.phase != Phase::Connected {
            return Vec::new();
        }
        self.devices
            .iter()
            .map(|(&id, record)| record.snapshot(id, self.status.generation))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_discovery_retains_off_state_and_clears_unreadable_metadata() {
        let mut observed = Observed::new();
        observed.status.phase = Phase::Connected;
        let mut record = DeviceRecord {
            permissions: Some(PermissionFlags::R | PermissionFlags::X),
            enum_readable: true,
            profile_readable: true,
            current: Some(0),
            ..DeviceRecord::default()
        };
        record.profiles.insert(
            0,
            DeviceProfile {
                index: 0,
                name: "off".into(),
                description: "Off".into(),
                availability: ProfileAvailability::Yes,
            },
        );
        record.profiles.insert(
            1,
            DeviceProfile {
                index: 1,
                name: "input".into(),
                description: "Microphone".into(),
                availability: ProfileAvailability::Yes,
            },
        );
        observed.devices.insert(4, record);
        let snapshot = observed.device_snapshot();
        assert!(snapshot[0].disabled && snapshot[0].readable);
        assert_eq!(snapshot[0].profiles.len(), 2);
        let record = observed.devices.get_mut(&4).unwrap();
        record.update_flags(ParamType::Profile, ParamInfoFlags::empty());
        assert_eq!(record.current, None);
        record.update_flags(ParamType::EnumProfile, ParamInfoFlags::empty());
        assert!(record.profiles.is_empty());
        observed.reset();
        assert!(observed.device_snapshot().is_empty());
    }

    #[test]
    fn unreadable_native_parameters_are_never_enumerated() {
        for flags in [
            PermissionFlags::R,
            PermissionFlags::X,
            PermissionFlags::empty(),
        ] {
            let record = DeviceRecord {
                permissions: Some(flags),
                enum_readable: true,
                profile_readable: true,
                ..DeviceRecord::default()
            };
            assert!(record.readable_params().is_empty());
        }
        assert_eq!(bounded(&"é".repeat(600)).chars().count(), 512);
    }
}
