// SPDX-License-Identifier: MPL-2.0
//! Toolkit-independent edits to the existing profile, with validation before mutation.

use std::collections::BTreeMap;

use cosmos_control::model::{MAX_BUSES, MAX_EFFECTS};
use cosmos_control::{
    Bus, Channels, DeviceSettings, Effect, EffectInstance, NodeRef, Profile, Route,
};

#[derive(Clone, Copy, Debug)]
pub enum Parameter {
    Gain,
    Threshold,
    Attack,
    Hold,
    Release,
    Ratio,
    Makeup,
    Frequency,
    EqGain,
    Q,
    Ceiling,
}

/// Snap GUI input, not imported profiles or DSP samples.
pub fn snap_control(value: f32, step: f32) -> f32 {
    // Divide by the reciprocal instead of multiplying by an inexact 0.1.
    let scale = step.recip();
    let rounded = (value * scale).round() / scale;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// Create one channel without modifying or replacing any existing profile data.
pub fn new_bus(profile: &Profile, channels: Channels) -> Result<Bus, String> {
    if profile.buses.len() >= MAX_BUSES {
        return Err(format!("A profile supports at most {MAX_BUSES} channels"));
    }
    let number = (1..=MAX_BUSES + 1)
        .find(|number| {
            let id = format!("channel-{number}");
            !profile.buses.iter().any(|bus| bus.id == id)
        })
        .ok_or_else(|| "No unused channel ID available".to_owned())?;
    let effects = vec![
        EffectInstance {
            id: "gate".into(),
            enabled: false,
            effect: Effect::Gate {
                threshold_db: -40.0,
                attack_ms: 5.0,
                hold_ms: 100.0,
                release_ms: 150.0,
            },
        },
        EffectInstance {
            id: "compressor".into(),
            enabled: false,
            effect: Effect::Compressor {
                threshold_db: -18.0,
                ratio: 3.0,
                attack_ms: 10.0,
                release_ms: 100.0,
                makeup_db: 0.0,
            },
        },
        equalizer("bass", 120.0, 0.0),
        equalizer("mid", 1000.0, 0.0),
        equalizer("treble", 6000.0, 0.0),
        equalizer("color-warmth", 160.0, 0.0),
        equalizer("color-brightness", 4500.0, 0.0),
        EffectInstance {
            id: "limiter".into(),
            enabled: true,
            effect: Effect::Limiter {
                ceiling_db: -1.0,
                release_ms: 5.0,
            },
        },
    ];
    if effects.len() > MAX_EFFECTS {
        return Err(format!("A channel supports at most {MAX_EFFECTS} effects"));
    }
    Ok(Bus {
        id: format!("channel-{number}"),
        name: format!("Channel {number}"),
        channels,
        gain_db: 0.0,
        enabled: true,
        hidden: false,
        muted: false,
        effects,
    })
}

/// Edit only a matching parameter, leaving the bus unchanged on any error.
pub fn set_parameter(
    bus: &mut Bus,
    effect_index: Option<usize>,
    parameter: Parameter,
    value: f32,
) -> Result<(), String> {
    let Some(index) = effect_index else {
        if !matches!(parameter, Parameter::Gain) {
            return Err(format!("{parameter:?} requires an effect index"));
        }
        if !value.is_finite() || !(-60.0..=12.0).contains(&value) {
            return Err("Channel gain must be finite and in -60..=12 dB".into());
        }
        bus.gain_db = value;
        return Ok(());
    };
    let instance = bus
        .effects
        .get_mut(index)
        .ok_or_else(|| format!("Effect index {index} is out of range"))?;
    let mut edited = instance.effect.clone();
    let member = match (&mut edited, parameter) {
        (
            Effect::Gate { threshold_db, .. } | Effect::Compressor { threshold_db, .. },
            Parameter::Threshold,
        ) => threshold_db,
        (
            Effect::Gate { attack_ms, .. } | Effect::Compressor { attack_ms, .. },
            Parameter::Attack,
        ) => attack_ms,
        (Effect::Gate { hold_ms, .. }, Parameter::Hold) => hold_ms,
        (
            Effect::Gate { release_ms, .. }
            | Effect::Compressor { release_ms, .. }
            | Effect::Limiter { release_ms, .. },
            Parameter::Release,
        ) => release_ms,
        (Effect::Compressor { ratio, .. }, Parameter::Ratio) => ratio,
        (Effect::Compressor { makeup_db, .. }, Parameter::Makeup) => makeup_db,
        (Effect::Equalizer { frequency_hz, .. }, Parameter::Frequency) => frequency_hz,
        (Effect::Equalizer { gain_db, .. }, Parameter::EqGain) => gain_db,
        (Effect::Equalizer { q, .. }, Parameter::Q) => q,
        (Effect::Limiter { ceiling_db, .. }, Parameter::Ceiling) => ceiling_db,
        _ => {
            return Err(format!(
                "{parameter:?} is not a parameter of effect {}",
                instance.id
            ));
        }
    };
    *member = value;
    edited.validate().map_err(|error| error.to_string())?;
    instance.effect = edited;
    Ok(())
}

/// Add an enabled effect without changing existing IDs or a final limiter's position.
pub fn add_effect(bus: &mut Bus, effect: Effect) -> Result<(), String> {
    if bus.effects.len() >= MAX_EFFECTS {
        return Err(format!("A channel supports at most {MAX_EFFECTS} effects"));
    }
    effect.validate().map_err(|error| error.to_string())?;
    let id = (1..=MAX_EFFECTS + 1)
        .map(|number| format!("fx-{number}"))
        .find(|id| !bus.effects.iter().any(|instance| instance.id == *id))
        .ok_or_else(|| "No unused effect ID available".to_owned())?;
    let index = insertion_index(bus);
    bus.effects.insert(
        index,
        EffectInstance {
            id,
            enabled: true,
            effect,
        },
    );
    Ok(())
}

/// Apply two ordinary tonal EQ bands, not a proprietary color algorithm.
/// Existing reserved bands retain their IDs and enabled state; edits use 160/4500 Hz and Q 0.7.
/// Missing bands are added together only when at least one requested gain is nonzero.
pub fn set_color(bus: &mut Bus, warmth: f32, brightness: f32) -> Result<(), String> {
    let bands = [
        ("color-warmth", 160.0, warmth),
        ("color-brightness", 4500.0, brightness),
    ];
    let activate = warmth != 0.0 || brightness != 0.0;
    let mut missing = 0;
    for (id, _, gain) in bands {
        if !gain.is_finite() || !(-12.0..=12.0).contains(&gain) {
            return Err(format!("{id} gain must be finite and in -12..=12 dB"));
        }
        let mut found = false;
        for instance in bus.effects.iter().filter(|instance| instance.id == id) {
            if found {
                return Err(format!("Duplicate reserved color ID: {id}"));
            }
            if !matches!(instance.effect, Effect::Equalizer { .. }) {
                return Err(format!("Reserved color ID {id} must be an equalizer"));
            }
            found = true;
        }
        if !found && activate {
            missing += 1;
        }
    }
    if bus.effects.len() > MAX_EFFECTS || missing > MAX_EFFECTS - bus.effects.len() {
        return Err(format!(
            "Color requires room for {missing} more effects (limit {MAX_EFFECTS})"
        ));
    }
    for (id, frequency_hz, gain_db) in bands {
        if let Some(instance) = bus.effects.iter_mut().find(|instance| instance.id == id) {
            instance.effect = Effect::Equalizer {
                frequency_hz,
                gain_db,
                q: 0.7,
            };
        } else if activate {
            let index = insertion_index(bus);
            bus.effects
                .insert(index, equalizer(id, frequency_hz, gain_db));
        }
    }
    Ok(())
}

/// Read bounded tonal EQ gains without mutating imported data; absent or non-EQ IDs are neutral.
pub fn color(bus: &Bus) -> (f32, f32) {
    let gain = |id| {
        bus.effects
            .iter()
            .find_map(|instance| {
                if instance.id == id
                    && let Effect::Equalizer { gain_db, .. } = instance.effect
                {
                    return Some(gain_db.clamp(-12.0, 12.0));
                }
                None
            })
            .unwrap_or(0.0)
    };
    (gain("color-warmth"), gain("color-brightness"))
}

/// Whether an effect ID is reserved for the two tonal color controls.
pub fn is_color(id: &str) -> bool {
    matches!(id, "color-warmth" | "color-brightness")
}

fn equalizer(id: &str, frequency_hz: f32, gain_db: f32) -> EffectInstance {
    EffectInstance {
        id: id.into(),
        enabled: true,
        effect: Effect::Equalizer {
            frequency_hz,
            gain_db,
            q: 0.7,
        },
    }
}

fn insertion_index(bus: &Bus) -> usize {
    let count = bus.effects.len();
    if bus
        .effects
        .last()
        .is_some_and(|instance| matches!(instance.effect, Effect::Limiter { .. }))
    {
        count - 1
    } else {
        count
    }
}

#[derive(Default)]
pub struct TextEdits {
    names: BTreeMap<String, TextDraft>,
    aliases: BTreeMap<BTreeMap<String, String>, TextDraft>,
}

struct TextDraft {
    value: String,
    error: Option<String>,
}

impl TextEdits {
    pub fn name<'a>(&'a self, bus: &'a Bus) -> &'a str {
        self.names
            .get(&bus.id)
            .map_or(&bus.name, |draft| &draft.value)
    }

    pub fn alias<'a>(&'a self, device: &'a DeviceSettings) -> &'a str {
        self.aliases
            .get(&device.identity)
            .map_or(&device.alias, |draft| &draft.value)
    }

    pub fn error(&self) -> Option<&str> {
        self.names
            .values()
            .chain(self.aliases.values())
            .find_map(|draft| draft.error.as_deref())
    }

    pub fn pending(&self) -> bool {
        !self.names.is_empty() || !self.aliases.is_empty()
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.aliases.clear();
    }

    pub fn prune(&mut self, profile: &Profile) {
        self.names
            .retain(|id, _| profile.buses.iter().any(|bus| &bus.id == id));
        self.aliases.retain(|identity, _| {
            profile
                .devices
                .iter()
                .any(|device| &device.identity == identity)
        });
    }

    /// Channel names change endpoint topology. Keep all typing separate until Enter
    /// or an explicit final-save confirmation; audio parameters remain live.
    pub fn rename(&mut self, profile: &mut Profile, index: usize, value: String) {
        let Some(bus) = profile.buses.get_mut(index) else {
            return;
        };
        let previous = std::mem::replace(&mut bus.name, value);
        let error = profile.validate().err().map(|error| error.to_string());
        let bus = &mut profile.buses[index];
        let value = std::mem::replace(&mut bus.name, previous);
        if value == bus.name {
            self.names.remove(&bus.id);
        } else {
            self.names
                .insert(bus.id.clone(), TextDraft { value, error });
        }
    }

    pub fn commit_name(&mut self, profile: &mut Profile, index: usize) -> Result<(), String> {
        let Some(bus) = profile.buses.get_mut(index) else {
            return Ok(());
        };
        if let Some(error) = self
            .names
            .get(&bus.id)
            .and_then(|draft| draft.error.as_ref())
        {
            return Err(error.clone());
        }
        if let Some(draft) = self.names.remove(&bus.id) {
            bus.name = draft.value;
        }
        Ok(())
    }

    pub fn commit_names(&mut self, profile: &mut Profile) -> Result<(), String> {
        if let Some(error) = self.error() {
            return Err(error.to_owned());
        }
        for bus in &mut profile.buses {
            if let Some(draft) = self.names.remove(&bus.id) {
                bus.name = draft.value;
            }
        }
        Ok(())
    }

    pub fn rename_device(&mut self, profile: &mut Profile, index: usize, value: String) {
        let Some(device) = profile.devices.get_mut(index) else {
            return;
        };
        let previous = std::mem::replace(&mut device.alias, value);
        match profile.validate() {
            Ok(()) => {
                self.aliases.remove(&profile.devices[index].identity);
            }
            Err(error) => {
                let device = &mut profile.devices[index];
                let value = std::mem::replace(&mut device.alias, previous);
                self.aliases.insert(
                    device.identity.clone(),
                    TextDraft {
                        value,
                        error: Some(error.to_string()),
                    },
                );
            }
        }
    }
}

/// Re-enabling a route can introduce feedback, so validate it before keeping the edit.
pub fn set_route_enabled(profile: &mut Profile, index: usize, enabled: bool) -> Result<(), String> {
    let Some(route) = profile.routes.get_mut(index) else {
        return Ok(());
    };
    let previous = std::mem::replace(&mut route.enabled, enabled);
    if let Err(error) = profile.validate() {
        profile.routes[index].enabled = previous;
        return Err(error.to_string());
    }
    Ok(())
}

pub fn channel_inputs<'a>(profile: &'a Profile, bus: &'a str) -> impl Iterator<Item = &'a Route> {
    profile
        .routes
        .iter()
        .filter(move |route| matches!(&route.to, NodeRef::Bus { id } if id == bus))
}

/// Replace only this channel's incoming routes. Keep outgoing routes and roll back
/// invalid edits. Replacing a multi-source mix requires explicit confirmation.
pub fn set_channel_input(
    profile: &mut Profile,
    bus: &str,
    source: Option<NodeRef>,
    replace_multiple: bool,
) -> Result<(), String> {
    if !profile.buses.iter().any(|candidate| candidate.id == bus) {
        return Err("This channel no longer exists.".into());
    }
    if !replace_multiple && channel_inputs(profile, bus).count() > 1 {
        return Err("Confirm replacement of this channel's multiple input routes.".into());
    }
    let incoming = |route: &Route| matches!(&route.to, NodeRef::Bus { id } if id == bus);
    let Some(source) = source else {
        profile.routes.retain(|route| !incoming(route));
        return Ok(());
    };
    if let Some(first) = profile.routes.iter().position(incoming) {
        // Disable the extra inputs during validation, without copying route identities.
        let disabled: Vec<_> = profile
            .routes
            .iter_mut()
            .enumerate()
            .skip(first + 1)
            .filter(|(_, route)| incoming(route) && route.enabled)
            .map(|(index, route)| {
                route.enabled = false;
                index
            })
            .collect();
        let previous = std::mem::replace(&mut profile.routes[first].from, source);
        let enabled = std::mem::replace(&mut profile.routes[first].enabled, true);
        if let Err(error) = profile.validate() {
            profile.routes[first].from = previous;
            profile.routes[first].enabled = enabled;
            for index in disabled {
                profile.routes[index].enabled = true;
            }
            return Err(error.to_string());
        }
        let mut index = 0;
        profile.routes.retain(|route| {
            let keep = index == first || !incoming(route);
            index += 1;
            keep
        });
    } else {
        let id = (1..=cosmos_control::model::MAX_ROUTES + 1)
            .map(|number| format!("route-{number}"))
            .find(|id| !profile.routes.iter().any(|route| &route.id == id))
            .ok_or_else(|| "No unused route ID available.".to_owned())?;
        profile.routes.push(Route {
            id,
            from: source,
            to: NodeRef::Bus { id: bus.to_owned() },
            enabled: true,
        });
        if let Err(error) = profile.validate() {
            profile.routes.pop();
            return Err(error.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Bus {
        new_bus(&Profile::default(), Channels::Stereo).unwrap()
    }

    #[test]
    fn control_steps_remove_fraction_noise_and_negative_zero() {
        assert_eq!(snap_control(-6.26, 0.5), -6.5);
        assert_eq!(snap_control(1234.5678, 1.0), 1235.0);
        let decimal = snap_control(0.1 + 12.0 * 0.1, 0.1);
        assert_eq!(decimal, 1.3);
        let neutral = snap_control(-0.01, 0.5);
        assert_eq!(neutral.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn channel_input_replacement_preserves_outputs_and_requires_multi_source_consent() {
        let mut profile = Profile::default();
        profile.buses.push(channel());
        profile
            .buses
            .push(new_bus(&profile, Channels::Stereo).unwrap());
        let bus = profile.buses[0].id.clone();
        let endpoint = NodeRef::Bus { id: bus.clone() };
        let source = |name: &str| NodeRef::Hardware {
            identity: BTreeMap::from([("node.name".into(), name.into())]),
        };
        let output = Route {
            id: "output".into(),
            from: endpoint.clone(),
            to: NodeRef::Bus {
                id: profile.buses[1].id.clone(),
            },
            enabled: true,
        };
        profile.routes = vec![
            Route {
                id: "mic".into(),
                from: source("old-mic"),
                to: endpoint.clone(),
                enabled: true,
            },
            output.clone(),
            Route {
                id: "app".into(),
                from: source("player"),
                to: endpoint,
                enabled: false,
            },
        ];
        let original = profile.clone();
        assert!(set_channel_input(&mut profile, &bus, Some(source("new-mic")), false).is_err());
        assert_eq!(profile, original);
        assert!(set_channel_input(&mut profile, &bus, None, false).is_err());
        assert_eq!(profile, original);
        set_channel_input(&mut profile, &bus, Some(source("new-mic")), true).unwrap();
        assert_eq!(
            profile.routes,
            vec![
                Route {
                    id: "mic".into(),
                    from: source("new-mic"),
                    to: NodeRef::Bus { id: bus.clone() },
                    enabled: true
                },
                output.clone(),
            ]
        );
        set_channel_input(&mut profile, &bus, None, false).unwrap();
        assert_eq!(profile.routes, vec![output]);
    }

    #[test]
    fn channel_input_feedback_rejection_restores_all_routes_and_flags() {
        let mut profile = Profile::default();
        profile.buses.push(channel());
        let bus = profile.buses[0].id.clone();
        let endpoint = NodeRef::Bus { id: bus.clone() };
        profile.routes = ["a", "b"]
            .into_iter()
            .map(|name| Route {
                id: name.into(),
                from: NodeRef::Hardware {
                    identity: BTreeMap::from([("node.name".into(), name.into())]),
                },
                to: endpoint.clone(),
                enabled: name == "b",
            })
            .collect();
        let original = profile.clone();
        assert!(set_channel_input(&mut profile, &bus, Some(endpoint.clone()), true).is_err());
        assert_eq!(profile, original);
        profile.routes.clear();
        assert!(set_channel_input(&mut profile, &bus, Some(endpoint), false).is_err());
        assert!(profile.routes.is_empty());
    }

    #[test]
    fn invalid_alias_stays_visible_while_other_valid_edits_and_empty_alias_are_accepted() {
        let mut profile = Profile::default();
        profile.buses.push(channel());
        profile.devices.push(DeviceSettings {
            identity: BTreeMap::from([("node.name".into(), "test-source".into())]),
            alias: "Source".into(),
            enabled: true,
            hidden: false,
            muted: false,
        });
        let mut text = TextEdits::default();
        text.rename_device(&mut profile, 0, "invalid\nalias".into());
        assert_eq!(text.alias(&profile.devices[0]), "invalid\nalias");
        assert_eq!(profile.devices[0].alias, "Source");
        assert!(text.error().is_some());
        set_color(&mut profile.buses[0], 3.0, -2.0).unwrap();
        assert!(profile.validate().is_ok());
        text.rename_device(&mut profile, 0, String::new());
        assert_eq!(text.alias(&profile.devices[0]), "");
        assert!(!text.pending());
    }

    #[test]
    fn enabling_feedback_route_is_rejected_without_changing_any_control() {
        use cosmos_control::{NodeRef, Route};
        let mut profile = Profile::default();
        profile.buses.push(channel());
        profile
            .buses
            .push(new_bus(&profile, Channels::Mono).unwrap());
        let first = NodeRef::Bus {
            id: profile.buses[0].id.clone(),
        };
        let second = NodeRef::Bus {
            id: profile.buses[1].id.clone(),
        };
        profile.routes = vec![
            Route {
                id: "forward".into(),
                from: first.clone(),
                to: second.clone(),
                enabled: true,
            },
            Route {
                id: "feedback".into(),
                from: second,
                to: first,
                enabled: false,
            },
        ];
        profile.validate().unwrap();
        let original = profile.clone();
        assert!(set_route_enabled(&mut profile, 1, true).is_err());
        assert_eq!(profile, original);
        set_parameter(&mut profile.buses[0], None, Parameter::Gain, -6.0).unwrap();
        profile.validate().unwrap();
    }

    #[test]
    fn new_channel_keeps_existing_profile_and_uses_an_unused_id() {
        let mut profile = Profile::default();
        profile.buses.push(channel());
        let original = profile.clone();
        let added = new_bus(&profile, Channels::Mono).unwrap();
        assert_eq!(profile, original);
        assert_eq!(added.id, "channel-2");
    }

    #[test]
    fn invalid_parameter_edits_preserve_all_data() {
        let mut bus = channel();
        let original = bus.clone();
        let gate = bus
            .effects
            .iter()
            .position(|instance| matches!(instance.effect, Effect::Gate { .. }))
            .unwrap();
        let eq = bus
            .effects
            .iter()
            .position(|instance| matches!(instance.effect, Effect::Equalizer { .. }))
            .unwrap();
        for (index, parameter, value) in [
            (None, Parameter::Gain, f32::NAN),
            (None, Parameter::Gain, 13.0),
            (Some(gate), Parameter::Threshold, 1.0),
            (Some(gate), Parameter::Attack, f32::INFINITY),
            (Some(eq), Parameter::Frequency, 0.0),
            (Some(100), Parameter::Release, 5.0),
        ] {
            assert!(set_parameter(&mut bus, index, parameter, value).is_err());
            assert_eq!(bus, original);
        }
    }

    #[test]
    fn valid_parameter_edits_preserve_other_effects_and_ids() {
        let mut bus = channel();
        let mut expected = bus.clone();
        let compressor = bus
            .effects
            .iter()
            .position(|instance| matches!(instance.effect, Effect::Compressor { .. }))
            .unwrap();
        expected.gain_db = -6.0;
        if let Effect::Compressor { ratio, .. } = &mut expected.effects[compressor].effect {
            *ratio = 4.0;
        }
        set_parameter(&mut bus, None, Parameter::Gain, -6.0).unwrap();
        set_parameter(&mut bus, Some(compressor), Parameter::Ratio, 4.0).unwrap();
        assert_eq!(bus, expected);
    }

    #[test]
    fn adding_effect_preserves_existing_instances_and_final_limiter() {
        let mut bus = channel();
        bus.effects[0].id = "fx-1".into();
        let original = bus.effects.clone();
        add_effect(
            &mut bus,
            Effect::Equalizer {
                frequency_hz: 800.0,
                gain_db: 2.0,
                q: 1.0,
            },
        )
        .unwrap();
        let added = bus
            .effects
            .iter()
            .position(|instance| instance.id == "fx-2")
            .unwrap();
        assert_eq!(added + 1, bus.effects.len() - 1);
        bus.effects.remove(added);
        assert_eq!(bus.effects, original);
        let original = bus.clone();
        assert!(
            add_effect(
                &mut bus,
                Effect::Limiter {
                    ceiling_db: 2.0,
                    release_ms: 5.0
                }
            )
            .is_err()
        );
        assert_eq!(bus, original);
    }

    #[test]
    fn color_capacity_failure_is_atomic_even_with_one_existing_band() {
        let mut bus = channel();
        bus.effects.retain(|instance| !is_color(&instance.id));
        while bus.effects.len() < MAX_EFFECTS - 1 {
            add_effect(
                &mut bus,
                Effect::Equalizer {
                    frequency_hz: 500.0,
                    gain_db: 0.0,
                    q: 0.7,
                },
            )
            .unwrap();
        }
        let original = bus.clone();
        assert!(set_color(&mut bus, 4.0, 2.0).is_err());
        assert_eq!(bus, original);
        bus.effects.insert(0, equalizer("color-warmth", 160.0, 1.0));
        let original = bus.clone();
        assert!(set_color(&mut bus, 4.0, 2.0).is_err());
        assert_eq!(bus, original);
    }

    #[test]
    fn color_preserves_existing_effects_and_reuses_reserved_ids() {
        let mut bus = channel();
        let warmth = bus
            .effects
            .iter_mut()
            .find(|instance| instance.id == "color-warmth")
            .unwrap();
        warmth.enabled = false;
        let mut expected = bus.clone();
        for instance in &mut expected.effects {
            match instance.id.as_str() {
                "color-warmth" => {
                    instance.effect = Effect::Equalizer {
                        frequency_hz: 160.0,
                        gain_db: -3.0,
                        q: 0.7,
                    }
                }
                "color-brightness" => {
                    instance.effect = Effect::Equalizer {
                        frequency_hz: 4500.0,
                        gain_db: 6.0,
                        q: 0.7,
                    }
                }
                _ => {}
            }
        }
        set_color(&mut bus, -3.0, 6.0).unwrap();
        assert_eq!(bus, expected);
    }

    #[test]
    fn invalid_color_or_reserved_id_preserves_data() {
        let mut bus = channel();
        bus.effects.retain(|instance| !is_color(&instance.id));
        let original = bus.clone();
        assert!(set_color(&mut bus, 4.0, f32::NAN).is_err());
        assert_eq!(bus, original);
        assert!(set_color(&mut bus, 13.0, 0.0).is_err());
        assert_eq!(bus, original);
        bus.effects
            .iter_mut()
            .find(|instance| matches!(instance.effect, Effect::Compressor { .. }))
            .unwrap()
            .id = "color-brightness".into();
        let original = bus.clone();
        assert!(set_color(&mut bus, 4.0, 2.0).is_err());
        assert_eq!(bus, original);
    }

    #[test]
    fn neutral_color_does_not_add_missing_bands_even_at_capacity() {
        for retained in [None, Some("color-warmth"), Some("color-brightness")] {
            let mut bus = channel();
            bus.effects.retain(|instance| {
                !is_color(&instance.id) || Some(instance.id.as_str()) == retained
            });
            while bus.effects.len() < MAX_EFFECTS {
                add_effect(
                    &mut bus,
                    Effect::Equalizer {
                        frequency_hz: 500.0,
                        gain_db: 0.0,
                        q: 0.7,
                    },
                )
                .unwrap();
            }
            let original = bus.clone();
            set_color(&mut bus, 0.0, 0.0).unwrap();
            assert_eq!(bus, original);
        }
    }

    #[test]
    fn color_activation_adds_missing_bands_with_exact_capacity() {
        for retained in [None, Some("color-warmth"), Some("color-brightness")] {
            let mut bus = channel();
            bus.effects.retain(|instance| {
                !is_color(&instance.id) || Some(instance.id.as_str()) == retained
            });
            let missing = if retained.is_some() { 1 } else { 2 };
            while bus.effects.len() < MAX_EFFECTS - missing {
                add_effect(
                    &mut bus,
                    Effect::Equalizer {
                        frequency_hz: 500.0,
                        gain_db: 0.0,
                        q: 0.7,
                    },
                )
                .unwrap();
            }
            let original: Vec<_> = bus
                .effects
                .iter()
                .filter(|instance| !is_color(&instance.id))
                .cloned()
                .collect();
            set_color(&mut bus, 4.0, 0.0).unwrap();
            assert_eq!(color(&bus), (4.0, 0.0));
            assert_eq!(bus.effects.len(), MAX_EFFECTS);
            for id in ["color-warmth", "color-brightness"] {
                assert_eq!(
                    bus.effects
                        .iter()
                        .filter(|instance| instance.id == id)
                        .count(),
                    1
                );
            }
            let preserved: Vec<_> = bus
                .effects
                .into_iter()
                .filter(|instance| !is_color(&instance.id))
                .collect();
            assert_eq!(preserved, original);
        }
    }

    #[test]
    fn imported_color_is_bounded_without_mutating_profile_data() {
        let mut bus = channel();
        for instance in &mut bus.effects {
            match instance.id.as_str() {
                "color-warmth" => {
                    instance.effect = Effect::Equalizer {
                        frequency_hz: 200.0,
                        gain_db: 18.0,
                        q: 1.5,
                    }
                }
                "color-brightness" => {
                    instance.effect = Effect::Equalizer {
                        frequency_hz: 5000.0,
                        gain_db: -18.0,
                        q: 1.2,
                    }
                }
                _ => {}
            }
        }
        let original = bus.clone();
        assert_eq!(color(&bus), (12.0, -12.0));
        assert_eq!(bus, original);
        set_color(&mut bus, 12.0, -12.0).unwrap();
        for instance in bus.effects.iter().filter(|instance| is_color(&instance.id)) {
            let expected = if instance.id == "color-warmth" {
                Effect::Equalizer {
                    frequency_hz: 160.0,
                    gain_db: 12.0,
                    q: 0.7,
                }
            } else {
                Effect::Equalizer {
                    frequency_hz: 4500.0,
                    gain_db: -12.0,
                    q: 0.7,
                }
            };
            assert_eq!(instance.effect, expected);
        }
    }
}
