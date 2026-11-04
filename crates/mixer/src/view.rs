// SPDX-License-Identifier: MPL-2.0
use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, button, column, row, text};
use cosmos_control::{Bus, Effect, MeterSnapshot, NodeRef};

use crate::app::{Flag, Message, Mixer, Popup};
use crate::model::{self, Parameter};

pub fn view(app: &Mixer) -> Element<'_, Message> {
    let running = app.control.status.is_some();
    let mut engine = button::standard(if running {
        "Stop engine…"
    } else {
        "Start engine"
    });
    if !app.control.action_busy() {
        engine = engine.on_press(if running {
            Message::Stop
        } else {
            Message::Start
        });
    }
    let header = row::with_children([
        engine.into(),
        widget::mouse_area(widget::space::horizontal().height(28))
            .on_press(Message::DragWindow)
            .interaction(cosmic::iced::mouse::Interaction::Grab)
            .into(),
        button::standard("Close")
            .on_press(Message::CloseRequested)
            .into(),
    ])
    .spacing(8)
    .align_y(Alignment::Center);
    let mut body = column::with_capacity(8).spacing(6);
    for (label, error) in [
        ("Error", app.error.as_deref()),
        ("Audio changes", app.control.error.as_deref()),
        ("Name", app.control.text_edits.error()),
        ("Connection", app.control.connection_error.as_deref()),
    ] {
        if let Some(error) = error {
            body = body.push(text::body(format!("{label}: {error}")));
        }
    }
    if let Some(status) = &app.control.status {
        for error in [
            &status.config_error,
            &status.backend.error,
            &status.last_error,
            &status.discovery_error,
        ]
        .into_iter()
        .flatten()
        {
            body = body.push(text::body(error));
        }
    }
    let content = if app.control.baseline.is_none() {
        text::body("Start the engine to load microphone settings.").into()
    } else {
        app.control
            .profile
            .buses
            .iter()
            .enumerate()
            .find(|(_, bus)| bus.id == "channel-1")
            .map_or_else(
                || text::body("Microphone profile unavailable. Reload to recover.").into(),
                |(index, bus)| strip(app, index, bus),
            )
    };
    body = body.push(content);
    if app.control.needs_save() || app.control.text_edits.pending() {
        body = body.push(text::caption(if app.control.save_blocked() {
            "Changes could not be saved. Retry or reload."
        } else if app.control.save_unavailable() {
            "Start the engine to save pending changes."
        } else if app.control.text_edits.pending() {
            "Press Enter to save the name."
        } else {
            "Saving changes…"
        }));
    }
    if app.error.is_some()
        || app.control.error.is_some()
        || app.control.save_blocked()
        || (app.control.needs_save() && app.control.save_unavailable())
    {
        let mut recovery =
            column::with_children([button::text("Reload…").on_press(Message::Reload).into()])
                .spacing(6);
        if app.control.retry_available() {
            recovery =
                recovery.push(button::standard("Retry pending changes").on_press(Message::Retry));
        }
        body = body.push(recovery);
    }
    widget::container(
        column::with_children([
            header.into(),
            widget::scrollable(body).height(Length::Fill).into(),
        ])
        .spacing(6),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(12)
    .class(cosmic::theme::Container::Card)
    .into()
}

fn strip<'a>(app: &'a Mixer, index: usize, bus: &'a Bus) -> Element<'a, Message> {
    let status = app
        .control
        .status
        .as_ref()
        .and_then(|s| s.backend.buses.iter().find(|s| s.id == bus.id));
    let state = status.map_or("Not applied / engine offline".to_owned(), |s| {
        format!("{:?}", s.state)
    });
    let mut content = column::with_children([
        text::caption(format!("{} · {:?}", bus.id, bus.channels)).into(),
        widget::text_input("Virtual microphone name", app.control.text_edits.name(bus))
            .on_input(move |name| Message::Name(index, name))
            .on_submit(move |_| Message::CommitName(index))
            .into(),
        text::caption("Press Enter to rename. Renaming can interrupt this channel.").into(),
        channel_input(app, bus),
        text::body(state).into(),
    ])
    .spacing(6);
    if let Some(error) = status.and_then(|s| s.error.as_ref()) {
        content = content.push(text::caption(error));
    }
    for route in model::channel_inputs(&app.control.profile, &bus.id) {
        if let Some(route_status) = app.control.status.as_ref().and_then(|status| {
            status
                .backend
                .routes
                .iter()
                .find(|status| status.id == route.id)
        }) {
            content = content.push(text::caption(format!(
                "Microphone input: {:?}",
                route_status.state
            )));
            if let Some(error) = &route_status.error {
                content = content.push(text::body(error));
            }
        }
    }
    content = content
        .push(parameter(
            index,
            None,
            Parameter::Gain,
            "Gain",
            bus.gain_db,
            -60.0..=12.0,
            0.5,
            "dB",
        ))
        .push(
            widget::checkbox(bus.muted)
                .label("Mute microphone")
                .on_toggle(move |value| Message::BusFlag(index, Flag::Muted, value)),
        );
    for (effect_index, instance) in bus.effects.iter().enumerate() {
        let (label, threshold) = match instance.effect {
            Effect::Gate { threshold_db, .. } => ("Noise gate", threshold_db),
            Effect::Compressor { threshold_db, .. } => ("Compressor", threshold_db),
            _ => continue,
        };
        content = content
            .push(
                widget::checkbox(instance.enabled)
                    .label(label)
                    .on_toggle(move |value| Message::EffectEnabled(index, effect_index, value)),
            )
            .push(parameter(
                index,
                Some(effect_index),
                Parameter::Threshold,
                "Threshold",
                threshold,
                -60.0..=0.0,
                0.5,
                "dB",
            ));
    }
    content = content.push(
        row::with_children([
            button::standard("Equalizer…")
                .on_press(Message::Popup(Popup::Effects(index, true)))
                .into(),
            button::text("Dynamics…")
                .on_press(Message::Popup(Popup::Effects(index, false)))
                .into(),
        ])
        .spacing(8),
    );
    let (warmth, brightness) = model::color(bus);
    content = content
        .push(text::caption("TONE COLOR · warmth / brightness"))
        .push(
            widget::canvas(crate::color::ColorPad {
                index,
                warmth,
                brightness,
            })
            .width(Length::Fill)
            .height(80),
        )
        .push(
            row::with_children([
                button::text("Color controls")
                    .on_press(Message::Popup(Popup::Effects(index, true)))
                    .into(),
                button::text("Reset tone")
                    .on_press(Message::Color(index, 0.0, 0.0))
                    .into(),
            ])
            .spacing(8),
        );
    let meters = row::with_children([
        meter("IN", status.and_then(|s| s.input_meter.as_ref())),
        meter("OUT", status.and_then(|s| s.output_meter.as_ref())),
    ])
    .spacing(12)
    .width(Length::Fill);
    content = content.push(meters);
    content.into()
}

fn meter<'a>(label: &'static str, meter: Option<&'a MeterSnapshot>) -> Element<'a, Message> {
    let mut content = column::with_children([text::caption(label).into()]).spacing(4);
    if let Some(meter) = meter.filter(|m| m.fresh && m.error.is_none()) {
        for channel in 0..meter.channels.min(2) as usize {
            let peak = db(meter.peak[channel]);
            let rms = db(meter.rms[channel]);
            content = content
                .push(text::caption(format!(
                    "{}  {peak:.1} dBFS",
                    if meter.channels == 1 {
                        "M"
                    } else if channel == 0 {
                        "L"
                    } else {
                        "R"
                    }
                )))
                .push(
                    widget::progress_bar::linear::Linear::new()
                        .progress(((peak + 60.0) / 60.0).clamp(0.0, 1.0))
                        .girth(8.0)
                        .width(Length::Fill),
                )
                .push(text::caption(format!("RMS {rms:.1}")));
        }
    } else {
        content = content.push(text::caption("No fresh level"));
    }
    content.into()
}

fn db(linear: f32) -> f32 {
    if linear > 0.0 {
        (20.0 * linear.log10()).max(-90.0)
    } else {
        -90.0
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "A slider needs its target, display unit, and validated numeric range"
)]
fn parameter<'a>(
    bus: usize,
    effect: Option<usize>,
    parameter: Parameter,
    label: &'static str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    unit: &'static str,
) -> Element<'a, Message> {
    column::with_children([
        text::caption(format!("{label}  {value:.1} {unit}")).into(),
        widget::slider(range, value, move |v| {
            Message::Parameter(bus, effect, parameter, model::snap_control(v, step))
        })
        .description(format!("{label}, {unit}, channel {}", bus + 1))
        .step(step)
        .into(),
    ])
    .spacing(4)
    .into()
}

fn endpoint_name(app: &Mixer, endpoint: &NodeRef) -> String {
    match endpoint {
        NodeRef::Hardware { identity } => app
            .control
            .nodes
            .iter()
            .find(|node| &node.identity == identity && model::microphone(node))
            .map_or_else(
                || identity.values().cloned().collect::<Vec<_>>().join(" · "),
                |node| node.description.clone(),
            ),
        NodeRef::Bus { id } => id.clone(),
    }
}

fn channel_input<'a>(app: &'a Mixer, bus: &'a Bus) -> Element<'a, Message> {
    let mut sources: Vec<_> = app
        .control
        .nodes
        .iter()
        .filter(|node| model::microphone(node))
        .map(|node| {
            (
                format!("{} · {:?}", node.description, node.layout.kind),
                NodeRef::Hardware {
                    identity: node.identity.clone(),
                },
            )
        })
        .collect();
    let route = model::channel_inputs(&app.control.profile, &bus.id).next();
    let mut missing = false;
    let selected = route.map_or(0, |route| {
        let index = sources
            .iter()
            .position(|(_, source)| source == &route.from)
            .unwrap_or_else(|| {
                missing = app
                    .control
                    .status
                    .as_ref()
                    .is_some_and(|status| status.phase == cosmos_control::Phase::Connected);
                sources.push((
                    format!(
                        "{} microphone: {}",
                        if missing { "Missing" } else { "Saved" },
                        endpoint_name(app, &route.from)
                    ),
                    route.from.clone(),
                ));
                sources.len() - 1
            });
        index + 1
    });
    let labels: Vec<_> = std::iter::once("No microphone selected".to_owned())
        .chain(sources.iter().map(|(label, _)| label.clone()))
        .collect();
    let bus_id = bus.id.clone();
    let mut content = column::with_children([
        text::caption("Microphone input").into(),
        widget::dropdown(labels, Some(selected), move |index| {
            Message::Input(
                bus_id.clone(),
                index.checked_sub(1).map(|index| sources[index].1.clone()),
            )
        })
        .width(Length::Fill)
        .into(),
    ])
    .spacing(4);
    if missing {
        content = content.push(text::body(
            "The saved microphone is unavailable. No replacement was selected.",
        ));
    }
    content.into()
}

pub fn dialog(app: &Mixer) -> Option<Element<'_, Message>> {
    let popup = app.popup.as_ref()?;
    let (title, body) = match popup {
        Popup::Effects(index, equalizer) => {
            return app
                .control
                .profile
                .buses
                .get(*index)
                .map(|bus| effects(app, *index, bus, *equalizer));
        }
        Popup::RemoveEffect(..) => (
            "Remove effect?",
            "This immediately removes the effect and can interrupt this channel.",
        ),
        Popup::Reload => (
            "Save and reload engine settings?",
            "Final changes are saved first. If saving fails, your changes stay here. To recover from a conflict or invalid text, you can explicitly discard pending changes instead.",
        ),
        Popup::Stop => (
            "Save and stop the audio engine?",
            "Final changes are saved first. Then the virtual microphone and input route stop. Other applications may lose their selected source. If saving fails, the engine is not stopped.",
        ),
        Popup::Close => (
            "Save before closing?",
            "Final changes are saved first, then this window exits. If saving fails or a name is invalid, the window stays open. The engine keeps running.",
        ),
    };
    let mut controls = column::with_capacity(4).spacing(8);
    for error in [
        app.error.as_deref(),
        app.control.error.as_deref(),
        app.control.connection_error.as_deref(),
        app.control.text_edits.error(),
    ]
    .into_iter()
    .flatten()
    {
        controls = controls.push(text::body(error));
    }
    if app.control.save_unavailable() {
        controls = controls
            .push(button::standard("Start engine to save changes").on_press(Message::Start));
    }
    if matches!(popup, Popup::Reload)
        && (app.control.save_blocked()
            || app.control.text_edits.pending()
            || (app.control.needs_save() && app.control.save_unavailable()))
    {
        let mut discard = button::destructive("Discard pending changes and reload");
        if app.control.can_discard() {
            discard = discard.on_press(Message::DiscardAndReload);
        }
        controls = controls.push(discard);
    }
    Some(
        widget::dialog()
            .title(title)
            .body(body)
            .control(widget::container(widget::scrollable(controls)).max_height(240))
            .primary_action(button::suggested("Confirm").on_press(Message::Confirm))
            .secondary_action(button::standard("Cancel").on_press(Message::ClosePopup))
            .width(Length::Fill)
            .max_height(760)
            .into(),
    )
}

fn effects<'a>(
    app: &'a Mixer,
    index: usize,
    bus: &'a Bus,
    equalizer: bool,
) -> Element<'a, Message> {
    let mut controls = column::with_capacity(20).spacing(18);
    if equalizer {
        let (warmth, brightness) = model::color(bus);
        controls = controls.push(text::body("Tone color uses two ordinary EQ bands: 160 Hz warmth and 4.5 kHz brightness. It is not a Voicemeeter algorithm."))
            .push(text::caption(format!("Warmth {warmth:+.1} dB")))
            .push(widget::slider(-12.0..=12.0, warmth, move |v| Message::Color(index, v, brightness)).description("Tone warmth in decibels").step(0.5_f32))
            .push(text::caption(format!("Brightness {brightness:+.1} dB")))
            .push(widget::slider(-12.0..=12.0, brightness, move |v| Message::Color(index, warmth, v)).description("Tone brightness in decibels").step(0.5_f32));
        for (effect_index, instance) in bus
            .effects
            .iter()
            .enumerate()
            .filter(|(_, effect)| model::is_color(&effect.id))
        {
            controls = controls.push(
                widget::checkbox(instance.enabled)
                    .label(format!("Enable {}", instance.id))
                    .on_toggle(move |enabled| Message::EffectEnabled(index, effect_index, enabled)),
            );
        }
        if !bus.effects.iter().any(|effect| model::is_color(&effect.id)) {
            controls = controls.push(text::caption("This imported channel has no color bands. Its first non-neutral color edit adds effects and can interrupt audio."));
        }
    } else {
        controls = controls.push(text::body("Input/output meters show real signal levels, not an exact gate-open or gain-reduction signal."));
    }
    for (effect_index, instance) in bus.effects.iter().enumerate() {
        if matches!(instance.effect, Effect::Equalizer { .. }) != equalizer
            || model::is_color(&instance.id)
        {
            continue;
        }
        let name = match instance.effect {
            Effect::Gate { .. } => "Noise gate",
            Effect::Compressor { .. } => "Compressor",
            Effect::Equalizer { .. } => "EQ band",
            Effect::Limiter { .. } => "Limiter",
        };
        let mut effect = column::with_children([
            widget::checkbox(instance.enabled)
                .label(format!("{name} · {}", instance.id))
                .on_toggle(move |v| Message::EffectEnabled(index, effect_index, v))
                .into(),
            button::text("Remove")
                .on_press(Message::Popup(Popup::RemoveEffect(index, effect_index)))
                .into(),
        ])
        .spacing(10);
        let p = |kind, label, value, range, step, unit| {
            parameter(
                index,
                Some(effect_index),
                kind,
                label,
                value,
                range,
                step,
                unit,
            )
        };
        match instance.effect {
            Effect::Gate {
                threshold_db,
                attack_ms,
                hold_ms,
                release_ms,
            } => {
                effect = effect
                    .push(p(
                        Parameter::Threshold,
                        "Threshold",
                        threshold_db,
                        -60.0..=0.0,
                        0.5,
                        "dB",
                    ))
                    .push(p(
                        Parameter::Attack,
                        "Attack",
                        attack_ms,
                        0.1..=2000.0,
                        0.1,
                        "ms",
                    ))
                    .push(p(Parameter::Hold, "Hold", hold_ms, 0.0..=1000.0, 1.0, "ms"))
                    .push(p(
                        Parameter::Release,
                        "Release",
                        release_ms,
                        1.0..=5000.0,
                        1.0,
                        "ms",
                    ));
            }
            Effect::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                makeup_db,
            } => {
                effect = effect
                    .push(p(
                        Parameter::Threshold,
                        "Threshold",
                        threshold_db,
                        -60.0..=0.0,
                        0.5,
                        "dB",
                    ))
                    .push(p(Parameter::Ratio, "Ratio", ratio, 1.0..=100.0, 0.1, ":1"))
                    .push(p(
                        Parameter::Attack,
                        "Attack",
                        attack_ms,
                        0.1..=2000.0,
                        0.1,
                        "ms",
                    ))
                    .push(p(
                        Parameter::Release,
                        "Release",
                        release_ms,
                        1.0..=5000.0,
                        1.0,
                        "ms",
                    ))
                    .push(p(
                        Parameter::Makeup,
                        "Makeup",
                        makeup_db,
                        -24.0..=24.0,
                        0.5,
                        "dB",
                    ));
            }
            Effect::Equalizer {
                frequency_hz,
                gain_db,
                q,
            } => {
                effect = effect
                    .push(text::caption(format!("Frequency {frequency_hz:.0} Hz")))
                    .push(
                        widget::slider(
                            20_f32.log10()..=20000_f32.log10(),
                            frequency_hz.log10(),
                            move |v| {
                                Message::Parameter(
                                    index,
                                    Some(effect_index),
                                    Parameter::Frequency,
                                    model::snap_control(10_f32.powf(v).clamp(20.0, 20000.0), 1.0),
                                )
                            },
                        )
                        .description("EQ frequency, logarithmic Hertz")
                        .step(0.005_f32),
                    )
                    .push(p(
                        Parameter::EqGain,
                        "Gain",
                        gain_db,
                        -24.0..=24.0,
                        0.5,
                        "dB",
                    ))
                    .push(p(Parameter::Q, "Q", q, 0.1..=20.0, 0.1, ""));
            }
            Effect::Limiter {
                ceiling_db,
                release_ms,
            } => {
                effect = effect
                    .push(p(
                        Parameter::Ceiling,
                        "Ceiling",
                        ceiling_db,
                        -48.0..=0.0,
                        0.5,
                        "dB",
                    ))
                    .push(p(
                        Parameter::Release,
                        "Release",
                        release_ms,
                        0.25..=20.0,
                        0.25,
                        "ms",
                    ));
            }
        }
        controls = controls.push(
            widget::container(effect)
                .padding(12)
                .class(cosmic::theme::Container::Card),
        );
    }
    let add = if equalizer {
        column::with_children([button::standard("+ EQ band")
            .on_press(Message::AddEffect(
                index,
                Effect::Equalizer {
                    frequency_hz: 1000.0,
                    gain_db: 0.0,
                    q: 0.7,
                },
            ))
            .into()])
    } else {
        column::with_children([
            button::standard("+ Gate")
                .on_press(Message::AddEffect(
                    index,
                    Effect::Gate {
                        threshold_db: -40.0,
                        attack_ms: 5.0,
                        hold_ms: 100.0,
                        release_ms: 150.0,
                    },
                ))
                .into(),
            button::standard("+ Compressor")
                .on_press(Message::AddEffect(
                    index,
                    Effect::Compressor {
                        threshold_db: -18.0,
                        ratio: 3.0,
                        attack_ms: 10.0,
                        release_ms: 100.0,
                        makeup_db: 0.0,
                    },
                ))
                .into(),
            button::standard("+ Limiter")
                .on_press(Message::AddEffect(
                    index,
                    Effect::Limiter {
                        ceiling_db: -1.0,
                        release_ms: 5.0,
                    },
                ))
                .into(),
        ])
    };
    controls = controls.push(add.spacing(8));
    if let Some(error) = &app.error {
        controls = controls.push(text::body(error));
    }
    if let Some(error) = &app.control.error {
        controls = controls.push(text::body(error));
    }
    if let Some(error) = &app.control.connection_error {
        controls = controls.push(text::body(error));
    }
    if app.control.retry_available() {
        controls =
            controls.push(button::standard("Retry pending changes").on_press(Message::Retry));
    }
    widget::dialog()
        .title(format!(
            "{} · {}",
            bus.name,
            if equalizer {
                "Equalizer & tone"
            } else {
                "Dynamics"
            }
        ))
        .body("Audio updates live. Adding or removing effects can interrupt audio.")
        .control(widget::scrollable(controls).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .max_height(760)
        .primary_action(button::standard("Close").on_press(Message::ClosePopup))
        .into()
}
