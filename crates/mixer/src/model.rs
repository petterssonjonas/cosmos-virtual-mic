// SPDX-License-Identifier: MPL-2.0
//! Toolkit-independent edits to the existing profile, with validation before mutation.

use std::collections::BTreeMap;

use cosmos_control::model::MAX_EFFECTS;
use cosmos_control::{Bus, Channels, Effect, EffectInstance, Node, NodeRef, Profile, Route};

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

    pub fn error(&self) -> Option<&str> {
        self.names.values().find_map(|draft| draft.error.as_deref())
    }

    pub fn pending(&self) -> bool {
        !self.names.is_empty()
    }

    pub fn clear(&mut self) {
        self.names.clear();
    }

    pub fn prune(&mut self, profile: &Profile) {
        self.names
            .retain(|id, _| profile.buses.iter().any(|bus| &bus.id == id));
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
}

pub fn channel_inputs<'a>(profile: &'a Profile, bus: &'a str) -> impl Iterator<Item = &'a Route> {
    profile
        .routes
        .iter()
        .filter(move |route| matches!(&route.to, NodeRef::Bus { id } if id == bus))
}

/// Only discovered capture devices can be selected; application endpoints are excluded.
pub fn microphone(node: &Node) -> bool {
    node.media_class == "Audio/Source"
        && !node.name.starts_with("cosmos.")
        && !node.identity.is_empty()
}

/// Replace the sole microphone route atomically, including a known channel layout.
pub fn set_channel_input(
    profile: &mut Profile,
    bus: &str,
    source: Option<NodeRef>,
    channels: Option<Channels>,
) -> Result<(), String> {
    if bus != "channel-1" || profile.buses.len() != 1 || profile.buses[0].id != bus {
        return Err("The virtual microphone channel is unavailable.".into());
    }
    if matches!(source, Some(NodeRef::Bus { .. })) {
        return Err("Select a hardware microphone.".into());
    }
    let previous_channels = profile.buses[0].channels;
    if let Some(channels) = channels {
        profile.buses[0].channels = channels;
    }
    let routes = source
        .map(|from| Route {
            id: "microphone".into(),
            from,
            to: NodeRef::Bus { id: bus.to_owned() },
            enabled: true,
        })
        .into_iter()
        .collect();
    let previous_routes = std::mem::replace(&mut profile.routes, routes);
    if let Err(error) = profile.validate() {
        profile.routes = previous_routes;
        profile.buses[0].channels = previous_channels;
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Bus {
        Profile::default().buses.remove(0)
    }

    #[test]
    fn microphone_choices_exclude_streams_sinks_and_owned_endpoints() {
        let mut node = Node {
            id: 1,
            generation: 1,
            name: "test-microphone".into(),
            description: "Test microphone".into(),
            media_class: "Audio/Source".into(),
            identity: BTreeMap::from([("node.name".into(), "test-microphone".into())]),
            layout: cosmos_control::Layout::new(Some(1), vec!["MONO".into()]),
        };
        assert!(microphone(&node));
        for media_class in ["Audio/Sink", "Stream/Output/Audio", "Stream/Input/Audio"] {
            node.media_class = media_class.into();
            assert!(!microphone(&node));
        }
        node.media_class = "Audio/Source".into();
        node.name = "cosmos.virtual-mic.channel-1.output".into();
        assert!(!microphone(&node));
        node.name = "test-microphone".into();
        node.identity.clear();
        assert!(!microphone(&node));
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
    fn microphone_selection_replaces_and_clears_without_substitution() {
        let mut profile = Profile::default();
        let source = |name: &str| NodeRef::Hardware {
            identity: BTreeMap::from([("node.name".into(), name.into())]),
        };
        set_channel_input(
            &mut profile,
            "channel-1",
            Some(source("first")),
            Some(Channels::Stereo),
        )
        .unwrap();
        assert_eq!(profile.buses[0].channels, Channels::Stereo);
        set_channel_input(
            &mut profile,
            "channel-1",
            Some(source("second")),
            Some(Channels::Mono),
        )
        .unwrap();
        assert_eq!(profile.routes.len(), 1);
        assert_eq!(profile.routes[0].from, source("second"));
        assert_eq!(profile.buses[0].channels, Channels::Mono);
        let previous = profile.clone();
        assert!(
            set_channel_input(
                &mut profile,
                "channel-1",
                Some(NodeRef::Bus {
                    id: "channel-1".into()
                }),
                None
            )
            .is_err()
        );
        assert_eq!(profile, previous);
        set_channel_input(&mut profile, "channel-1", None, None).unwrap();
        assert!(profile.routes.is_empty());
    }

    #[test]
    fn invalid_name_draft_does_not_replace_saved_name_or_block_audio_controls() {
        let mut profile = Profile::default();
        let previous = profile.buses[0].name.clone();
        let mut edits = TextEdits::default();
        edits.rename(&mut profile, 0, "invalid\nname".into());
        assert_eq!(edits.name(&profile.buses[0]), "invalid\nname");
        assert_eq!(profile.buses[0].name, previous);
        assert!(edits.commit_names(&mut profile).is_err());
        set_parameter(&mut profile.buses[0], None, Parameter::Gain, -6.0).unwrap();
        assert!(profile.validate().is_ok());
        edits.rename(&mut profile, 0, "My microphone".into());
        edits.commit_names(&mut profile).unwrap();
        assert_eq!(profile.buses[0].name, "My microphone");
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
