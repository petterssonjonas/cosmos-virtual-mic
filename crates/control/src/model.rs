// SPDX-License-Identifier: MPL-2.0
//! Validated desired state. Durable selectors never contain PipeWire object IDs.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use serde::{Deserialize, Serialize};

use crate::invalid;

pub const PROFILE_VERSION: u32 = 1;
pub const MAX_PROFILE_BYTES: usize = 64 * 1024;
pub const MAX_BUSES: usize = 16;
pub const MAX_ROUTES: usize = 128;
pub const MAX_DEVICE_SETTINGS: usize = 128;
pub const MAX_EFFECTS: usize = 16;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub buses: Vec<Bus>,
    pub devices: Vec<DeviceSettings>,
    pub routes: Vec<Route>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            buses: Vec::new(),
            devices: Vec::new(),
            routes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Channels {
    Mono,
    Stereo,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bus {
    pub id: String,
    pub name: String,
    pub channels: Channels,
    pub enabled: bool,
    pub hidden: bool,
    pub muted: bool,
    #[serde(default)]
    pub gain_db: f32,
    pub effects: Vec<EffectInstance>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeRef {
    Hardware { identity: BTreeMap<String, String> },
    Bus { id: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub from: NodeRef,
    pub to: NodeRef,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceSettings {
    pub identity: BTreeMap<String, String>,
    pub alias: String,
    pub enabled: bool,
    pub hidden: bool,
    pub muted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EffectInstance {
    pub id: String,
    pub enabled: bool,
    pub effect: Effect,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    Gate {
        threshold_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
    },
    Compressor {
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
    },
    Equalizer {
        frequency_hz: f32,
        gain_db: f32,
        q: f32,
    },
    Limiter {
        ceiling_db: f32,
        release_ms: f32,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Disabled,
    Pending,
    Ready,
    Unavailable,
    Error,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendStatus {
    pub ready: bool,
    pub error: Option<String>,
    pub buses: Vec<BusStatus>,
    pub routes: Vec<RouteStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeterSnapshot {
    pub channels: u32,
    pub peak: [f32; 2],
    pub rms: [f32; 2],
    pub frames: u32,
    pub sequence: u64,
    pub fresh: bool,
    pub silent: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BusStatus {
    pub id: String,
    pub state: RuntimeState,
    pub error: Option<String>,
    pub input_meter: Option<MeterSnapshot>,
    pub output_meter: Option<MeterSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteStatus {
    pub id: String,
    pub state: RuntimeState,
    pub error: Option<String>,
}

fn text(value: &str, label: &str, max: usize, empty: bool) -> io::Result<()> {
    if (!empty && value.trim().is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "{label} must be {}at most {max} bytes with no control characters",
            if empty { "" } else { "nonempty and " }
        )));
    }
    Ok(())
}

fn identifier(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid(
            "IDs must contain 1..64 ASCII letters, digits, '.', '_' or '-'",
        ));
    }
    Ok(())
}

fn identity(value: &BTreeMap<String, String>) -> io::Result<()> {
    const KEYS: &[&str] = &[
        "node.name",
        "object.path",
        "device.name",
        "device.serial",
        "device.bus-id",
        "device.bus-path",
        "device.api",
        "api.alsa.path",
        "api.alsa.pcm.device",
        "api.alsa.pcm.subdevice",
        "api.bluez5.address",
        "api.bluez5.profile",
    ];
    if value.is_empty() || value.len() > KEYS.len() {
        return Err(invalid(
            "hardware identity requires durable discovery properties",
        ));
    }
    for (key, value) in value {
        if !KEYS.contains(&key.as_str()) {
            return Err(invalid(format!("unsupported identity property: {key}")));
        }
        text(value, "identity value", 512, false)?;
        if key == "node.name" && value.starts_with("cosmos.") {
            return Err(invalid("app-owned endpoints must use a bus reference"));
        }
    }
    Ok(())
}

fn range(value: f32, min: f32, max: f32, label: &str) -> io::Result<()> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(invalid(format!(
            "{label} must be finite and in {min}..={max}"
        )));
    }
    Ok(())
}

impl Effect {
    pub fn validate(&self) -> io::Result<()> {
        match *self {
            Self::Gate {
                threshold_db,
                attack_ms,
                hold_ms,
                release_ms,
            } => {
                range(threshold_db, -60.0, 0.0, "gate threshold_db")?;
                range(attack_ms, 0.1, 2000.0, "gate attack_ms")?;
                range(hold_ms, 0.0, 1000.0, "gate hold_ms")?;
                range(release_ms, 1.0, 5000.0, "gate release_ms")
            }
            Self::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => {
                range(threshold_db, -60.0, 0.0, "compressor threshold_db")?;
                range(ratio, 1.0, 100.0, "compressor ratio")?;
                range(attack_ms, 0.1, 2000.0, "compressor attack_ms")?;
                range(release_ms, 1.0, 5000.0, "compressor release_ms")?;
                range(makeup_db, -24.0, 24.0, "compressor makeup_db")
            }
            Self::Equalizer {
                frequency_hz,
                gain_db,
                q,
            } => {
                range(frequency_hz, 20.0, 20000.0, "equalizer frequency_hz")?;
                range(gain_db, -24.0, 24.0, "equalizer gain_db")?;
                range(q, 0.1, 20.0, "equalizer q")
            }
            Self::Limiter {
                ceiling_db,
                release_ms,
            } => {
                range(ceiling_db, -48.0, 0.0, "limiter ceiling_db")?;
                range(release_ms, 0.25, 20.0, "limiter release_ms")
            }
        }
    }
}

// Two partial selectors can identify the same node unless a shared property conflicts.
fn overlaps(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> bool {
    a.iter()
        .all(|(key, value)| b.get(key).is_none_or(|other| other == value))
}

impl Profile {
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(invalid("profile exceeds byte limit"));
        }
        let profile: Self = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.version != PROFILE_VERSION {
            return Err(invalid("unsupported profile version"));
        }
        if self.buses.len() > MAX_BUSES
            || self.routes.len() > MAX_ROUTES
            || self.devices.len() > MAX_DEVICE_SETTINGS
        {
            return Err(invalid("profile exceeds bus, route or device count limit"));
        }
        let mut buses = BTreeSet::new();
        for bus in &self.buses {
            identifier(&bus.id)?;
            text(&bus.name, "bus name", 128, false)?;
            range(bus.gain_db, -60.0, 12.0, "bus gain_db")?;
            if !buses.insert(bus.id.as_str()) {
                return Err(invalid("duplicate bus ID"));
            }
            if bus.effects.len() > MAX_EFFECTS {
                return Err(invalid("bus exceeds effect count limit"));
            }
            let mut effects = BTreeSet::new();
            for effect in &bus.effects {
                identifier(&effect.id)?;
                if !effects.insert(effect.id.as_str()) {
                    return Err(invalid("duplicate effect ID in bus"));
                }
                effect.effect.validate()?;
            }
        }
        for (index, device) in self.devices.iter().enumerate() {
            identity(&device.identity)?;
            text(&device.alias, "device alias", 128, true)?;
            if self.devices[..index]
                .iter()
                .any(|other| overlaps(&device.identity, &other.identity))
            {
                return Err(invalid(
                    "device settings selectors overlap; use distinct durable identities",
                ));
            }
        }
        let mut routes = BTreeSet::new();
        let mut endpoints = Vec::new();
        for route in &self.routes {
            identifier(&route.id)?;
            if !routes.insert(route.id.as_str()) {
                return Err(invalid("duplicate route ID"));
            }
            for node in [&route.from, &route.to] {
                match node {
                    NodeRef::Hardware { identity: selector } => identity(selector)?,
                    NodeRef::Bus { id } if !buses.contains(id.as_str()) => {
                        return Err(invalid(format!("route references unknown bus: {id}")));
                    }
                    NodeRef::Bus { .. } => {}
                }
                if !endpoints.contains(&node) {
                    endpoints.push(node);
                }
            }
        }
        // Collapse potentially identical hardware selectors before checking the directed graph.
        // A bus is one vertex: playback input flows to recording output even without a route.
        let mut groups: Vec<usize> = (0..endpoints.len()).collect();
        for (i, a) in endpoints.iter().enumerate() {
            for (j, b) in endpoints[..i].iter().enumerate() {
                if let (NodeRef::Hardware { identity: a }, NodeRef::Hardware { identity: b }) =
                    (a, b)
                    && overlaps(a, b)
                {
                    let old = groups[i];
                    let new = groups[j];
                    for group in &mut groups {
                        if *group == old {
                            *group = new;
                        }
                    }
                }
            }
        }
        let mut edges = vec![Vec::new(); endpoints.len()];
        let mut incoming = vec![0usize; endpoints.len()];
        for route in self.routes.iter().filter(|r| r.enabled) {
            let from = endpoints
                .iter()
                .position(|n| **n == route.from)
                .ok_or_else(|| invalid("missing route source"))?;
            let to = endpoints
                .iter()
                .position(|n| **n == route.to)
                .ok_or_else(|| invalid("missing route destination"))?;
            edges[groups[from]].push(groups[to]);
            incoming[groups[to]] += 1;
        }
        let mut ready: Vec<_> = incoming
            .iter()
            .enumerate()
            .filter_map(|(i, &n)| (n == 0).then_some(i))
            .collect();
        let mut visited = 0;
        while let Some(node) = ready.pop() {
            visited += 1;
            for &target in &edges[node] {
                incoming[target] -= 1;
                if incoming[target] == 0 {
                    ready.push(target);
                }
            }
        }
        if visited != endpoints.len() {
            return Err(invalid(
                "routes contain a feedback cycle, including bus flow or overlapping hardware selectors",
            ));
        }
        crate::encode(self, MAX_PROFILE_BYTES)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(id: &str) -> Bus {
        Bus {
            id: id.into(),
            name: id.into(),
            channels: Channels::Mono,
            enabled: true,
            hidden: false,
            muted: false,
            gain_db: 0.0,
            effects: Vec::new(),
        }
    }

    #[test]
    fn bus_gain_defaults_for_version_one_and_round_trips_valid_boundaries() {
        let legacy = br#"{"version":1,"buses":[{"id":"voice","name":"Voice","channels":"mono","enabled":true,"hidden":false,"muted":false,"effects":[]}],"devices":[],"routes":[]}"#;
        let mut profile = Profile::decode(legacy).unwrap();
        assert_eq!(profile.buses[0].gain_db, 0.0);
        for gain_db in [-60.0, -6.0, 0.0, 12.0] {
            profile.buses[0].gain_db = gain_db;
            profile.validate().unwrap();
            let bytes = crate::encode(&profile, MAX_PROFILE_BYTES).unwrap();
            assert_eq!(Profile::decode(&bytes).unwrap(), profile);
        }
    }

    #[test]
    fn bus_gain_rejects_nonfinite_and_out_of_range_values_even_when_muted() {
        let mut profile = Profile {
            buses: vec![bus("voice")],
            ..Profile::default()
        };
        profile.buses[0].muted = true;
        for gain_db in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -60.01, 12.01] {
            profile.buses[0].gain_db = gain_db;
            assert!(profile.validate().is_err(), "accepted gain {gain_db}");
        }
        for gain_db in [-61, 13] {
            let mut value = serde_json::to_value(Profile {
                buses: vec![bus("voice")],
                ..Profile::default()
            })
            .unwrap();
            value["buses"][0]["gain_db"] = gain_db.into();
            assert!(Profile::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let overflow = br#"{"version":1,"buses":[{"id":"voice","name":"Voice","channels":"mono","enabled":true,"hidden":false,"muted":false,"gain_db":1e999,"effects":[]}],"devices":[],"routes":[]}"#;
        assert!(Profile::decode(overflow).is_err());
    }

    #[test]
    fn cycles_include_bus_flow_and_overlapping_hardware_selectors() {
        let a = NodeRef::Bus { id: "a".into() };
        let b = NodeRef::Bus { id: "b".into() };
        let mut profile = Profile {
            buses: vec![bus("a"), bus("b")],
            ..Profile::default()
        };
        profile.routes = vec![
            Route {
                id: "one".into(),
                from: a.clone(),
                to: b.clone(),
                enabled: true,
            },
            Route {
                id: "two".into(),
                from: b,
                to: a,
                enabled: true,
            },
        ];
        assert!(profile.validate().is_err());
        profile.routes[1].enabled = false;
        assert!(profile.validate().is_ok());
        profile.routes[0].from = NodeRef::Hardware {
            identity: BTreeMap::from([("node.name".into(), "source".into())]),
        };
        profile.routes[1].to = NodeRef::Hardware {
            identity: BTreeMap::from([("device.serial".into(), "123".into())]),
        };
        profile.routes[1].enabled = true;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn complete_persisted_profile_fits_the_decode_limit() {
        let mut profile = Profile {
            devices: (0..MAX_DEVICE_SETTINGS)
                .map(|index| DeviceSettings {
                    identity: BTreeMap::from([
                        ("node.name".into(), format!("boundary-{index:03}")),
                        ("device.serial".into(), "x".into()),
                    ]),
                    alias: String::new(),
                    enabled: true,
                    hidden: false,
                    muted: false,
                })
                .collect(),
            ..Profile::default()
        };
        let target = MAX_PROFILE_BYTES - 1;
        let mut remaining = target - serde_json::to_vec(&profile).unwrap().len();
        for device in &mut profile.devices {
            let extra = remaining.min(511);
            device
                .identity
                .get_mut("device.serial")
                .unwrap()
                .push_str(&"x".repeat(extra));
            remaining -= extra;
        }
        assert_eq!(remaining, 0);
        assert!(profile.validate().is_ok());
        let bytes = crate::encode(&profile, MAX_PROFILE_BYTES).unwrap();
        assert_eq!(bytes.len(), MAX_PROFILE_BYTES);
        assert_eq!(Profile::decode(&bytes).unwrap(), profile);
        profile
            .devices
            .iter_mut()
            .find_map(|device| {
                device
                    .identity
                    .get_mut("device.serial")
                    .filter(|serial| serial.len() < 512)
            })
            .unwrap()
            .push('x');
        assert_eq!(
            serde_json::to_vec(&profile).unwrap().len(),
            MAX_PROFILE_BYTES
        );
        assert!(profile.validate().is_err());
    }

    #[test]
    fn invalid_parameters_and_native_injection_are_rejected() {
        assert!(
            Effect::Equalizer {
                frequency_hz: f32::NAN,
                gain_db: 0.0,
                q: 1.0
            }
            .validate()
            .is_err()
        );
        assert!(
            Profile::decode(br#"{"version":1,"buses":[],"devices":[],"routes":[],"node":{}}"#)
                .is_err()
        );
        assert!(identity(&BTreeMap::from([("object.id".into(), "42".into())])).is_err());
        assert!(Profile::default().validate().is_ok());
    }
}
