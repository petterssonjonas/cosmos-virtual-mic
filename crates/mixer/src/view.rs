// SPDX-License-Identifier: MPL-2.0
use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, button, column, row, text};
use cosmos_control::{
    Bus, Channels, DeviceProfileRequest, DeviceProfileState, Effect, MeterSnapshot, NodeRef,
    ProfileAvailability,
};

use crate::app::{Flag, Message, Mixer, Page, Popup};
use crate::model::{self, Parameter};

pub fn view(app: &Mixer) -> Element<'_, Message> {
    let tabs = row::with_children([
        button::text("Mixer")
            .on_press(Message::Page(Page::Mixer))
            .into(),
        button::text("Routing")
            .on_press(Message::Page(Page::Routing))
            .into(),
        button::text("Devices")
            .on_press(Message::Page(Page::Devices))
            .into(),
        widget::space::horizontal().into(),
        widget::checkbox(app.show_hidden)
            .label("Show hidden")
            .on_toggle(Message::ShowHidden)
            .into(),
    ])
    .spacing(12)
    .align_y(Alignment::Center);
    let connection = app
        .control
        .status
        .as_ref()
        .map_or("Engine offline".to_owned(), |s| {
            format!(
                "PipeWire {:?} · engine {} · generation {}",
                s.phase, s.pid, s.generation
            )
        });
    let mut top = column::with_children([tabs.into(), text::caption(connection).into()]).spacing(8);
    if let Some(error) = &app.error {
        top = top.push(text::body(format!("Error: {error}")));
    }
    if let Some(error) = &app.control.error {
        top = top.push(text::body(format!("Audio changes: {error}")));
    }
    if let Some(error) = app.control.text_edits.error() {
        top = top.push(text::body(format!("Name or alias: {error}")));
    }
    if let Some(error) = &app.control.connection_error {
        top = top.push(text::body(format!("Connection: {error}")));
    }
    if let Some(error) = &app.control.device_error {
        top = top.push(text::body(format!("System device: {error}")));
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
            top = top.push(text::body(error));
        }
    }
    let content = match app.page {
        Page::Mixer => mixer(app),
        Page::Routing => routing(app),
        Page::Devices => devices(app),
    };
    let mut engine = button::standard(if app.control.status.is_some() {
        "Stop engine…"
    } else {
        "Start engine"
    });
    if !app.control.action_busy() {
        engine = engine.on_press(if app.control.status.is_some() {
            Message::Stop
        } else {
            Message::Start
        });
    }
    let mut actions = row::with_children([
        engine.into(),
        button::text("Reload…").on_press(Message::Reload).into(),
        button::text("Quit Mixer").on_press(Message::Quit).into(),
        widget::space::horizontal().into(),
        text::caption(if app.indicator_status.is_ok() {
            "Close hides to the panel; audio keeps running"
        } else {
            "Close exits the mixer; audio keeps running"
        })
        .into(),
    ])
    .spacing(12)
    .align_y(Alignment::Center);
    if app.control.retry_available() {
        actions = actions.push(button::standard("Retry pending changes").on_press(Message::Retry));
    }
    let footer = column::with_children([
        text::caption(if app.control.save_blocked() {
            "Changes are kept here. Resolve the error to resume live audio updates."
        } else if app.control.action_busy() {
            "Saving final changes before continuing…"
        } else if app.control.needs_save() && app.control.status.is_none() {
            "Changes are kept here until the engine is connected and running."
        } else if app.control.needs_save() {
            "Audio changes are live. Saving the complete profile…"
        } else if app.control.text_edits.pending() {
            "Name edits stay here until Enter or a final-save confirmation. Audio controls remain live."
        } else {
            &app.control.notice
        })
        .into(),
        actions.into(),
    ])
    .push(text::caption(match &app.indicator_status {
        Ok(()) => "Panel indicator connected. Open Mixer restores this window.".to_owned(),
        Err(reason) => format!("{reason}. Keep this window open or reopen the launcher. GNOME needs an AppIndicator extension."),
    }))
    .spacing(8);
    widget::container(column::with_children([top.into(), content, footer.into()]).spacing(16))
        .padding(18)
        .into()
}

fn mixer(app: &Mixer) -> Element<'_, Message> {
    let mut add_mono = button::standard("+ Mono channel");
    let mut add_stereo = button::standard("+ Stereo channel");
    if app.control.baseline.is_some() {
        add_mono = add_mono.on_press(Message::Add(Channels::Mono));
        add_stereo = add_stereo.on_press(Message::Add(Channels::Stereo));
    }
    let controls = row::with_children([
        text::title2("Your mix").into(),
        widget::space::horizontal().into(),
        add_mono.into(),
        add_stereo.into(),
    ])
    .spacing(12)
    .align_y(Alignment::Center);
    let strips = row::with_children(
        app.control
            .profile
            .buses
            .iter()
            .enumerate()
            .filter(|(_, bus)| app.show_hidden || !bus.hidden)
            .map(|(index, bus)| strip(app, index, bus)),
    )
    .spacing(12);
    let mut body = column::with_children([controls.into()]).spacing(16);
    if app.control.profile.buses.is_empty() {
        body = body.push(text::title3("A clear start. No audio routes selected."))
            .push(text::body("Start the engine, add a mono voice channel or a stereo channel, then select its Input below the channel name. Use Routing for outputs and multi-source mixes. No system defaults will change."));
    }
    body = body.push(widget::scrollable(strips).direction(
        cosmic::iced::widget::scrollable::Direction::Horizontal(
            cosmic::iced::widget::scrollable::Scrollbar::new(),
        ),
    ));
    widget::scrollable(body).height(Length::Fill).into()
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
        widget::text_input("Channel name", app.control.text_edits.name(bus))
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
    content = content.push(
        row::with_children([
            widget::checkbox(bus.enabled)
                .label("Enabled")
                .on_toggle(move |v| Message::BusFlag(index, Flag::Enabled, v))
                .into(),
            widget::checkbox(bus.hidden)
                .label("Hidden")
                .on_toggle(move |v| Message::BusFlag(index, Flag::Hidden, v))
                .into(),
        ])
        .spacing(12),
    );
    content = content
        .push(parameter(
            index,
            None,
            Parameter::Gain,
            "Volume",
            bus.gain_db,
            -60.0..=12.0,
            0.5,
            "dB",
        ))
        .push(
            widget::checkbox(bus.muted)
                .label("Mute channel")
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
    content = content
        .push(meters)
        .push(button::text("Remove channel…").on_press(Message::Popup(Popup::Remove(index))));
    widget::container(content)
        .width(286)
        .padding(12)
        .class(cosmic::theme::Container::Card)
        .into()
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
        NodeRef::Bus { id } => app
            .control
            .profile
            .buses
            .iter()
            .find(|bus| &bus.id == id)
            .map_or_else(|| id.clone(), |bus| format!("{} ({id})", bus.name)),
        NodeRef::Hardware { identity } => app
            .control
            .profile
            .devices
            .iter()
            .find(|device| &device.identity == identity)
            .map(|d| d.alias.clone())
            .or_else(|| {
                app.control
                    .nodes
                    .iter()
                    .find(|node| &node.identity == identity)
                    .map(|n| n.description.clone())
            })
            .unwrap_or_else(|| identity.values().cloned().collect::<Vec<_>>().join(" · ")),
    }
}

fn node_role(media_class: &str) -> &'static str {
    match media_class {
        "Audio/Source" => "Input device",
        "Audio/Sink" => "Output device",
        "Stream/Output/Audio" => "Input · app playback",
        "Stream/Input/Audio" => "Output · app recording",
        _ => "Unknown direction",
    }
}

fn channel_input<'a>(app: &'a Mixer, bus: &'a Bus) -> Element<'a, Message> {
    let mut inputs = model::channel_inputs(&app.control.profile, &bus.id);
    let first = inputs.next();
    let count = usize::from(first.is_some()) + inputs.count();
    let mut sources = choices(app, true);
    sources.retain(|(_, source)| !matches!(source, NodeRef::Bus { id } if id == &bus.id));
    let selected = if count == 0 {
        Some(0)
    } else if let Some(route) = first.filter(|route| count == 1 && route.enabled) {
        let index = sources
            .iter()
            .position(|(_, source)| source == &route.from)
            .unwrap_or_else(|| {
                sources.push((
                    format!("Saved input: {}", endpoint_name(app, &route.from)),
                    route.from.clone(),
                ));
                sources.len() - 1
            });
        Some(index + 1)
    } else {
        None
    };
    let labels: Vec<_> = std::iter::once("No routed input".to_owned())
        .chain(sources.iter().map(|(label, _)| label.clone()))
        .collect();
    let bus_id = bus.id.clone();
    let placeholder = if count > 1 {
        format!("{count} input routes · see Routing")
    } else {
        format!(
            "Disabled: {}",
            first.map_or_else(String::new, |route| endpoint_name(app, &route.from))
        )
    };
    column::with_children([
        text::caption("Input").into(),
        widget::dropdown(labels, selected, move |index| {
            Message::Input(
                bus_id.clone(),
                index.checked_sub(1).map(|index| sources[index].1.clone()),
            )
        })
        .placeholder(placeholder)
        .width(Length::Fill)
        .into(),
    ])
    .spacing(4)
    .into()
}

fn choices(app: &Mixer, source: bool) -> Vec<(String, NodeRef)> {
    let mut choices: Vec<_> = app
        .control
        .profile
        .buses
        .iter()
        .filter(|bus| app.show_hidden || !bus.hidden)
        .map(|bus| {
            (
                format!("Channel: {} ({})", bus.name, bus.id),
                NodeRef::Bus { id: bus.id.clone() },
            )
        })
        .collect();
    choices.extend(
        app.control
            .nodes
            .iter()
            .filter(|node| !node.name.starts_with("cosmos.") && !node.identity.is_empty())
            .filter(|node| {
                if source {
                    node.media_class.contains("Source") || node.media_class == "Stream/Output/Audio"
                } else {
                    node.media_class.contains("Sink") || node.media_class == "Stream/Input/Audio"
                }
            })
            .filter(|node| {
                app.show_hidden
                    || !app
                        .control
                        .profile
                        .devices
                        .iter()
                        .any(|d| d.hidden && d.identity == node.identity)
            })
            .map(|node| {
                let endpoint = NodeRef::Hardware {
                    identity: node.identity.clone(),
                };
                (
                    format!(
                        "{}: {} · {} · {:?}",
                        node_role(&node.media_class),
                        endpoint_name(app, &endpoint),
                        node.name,
                        node.layout.kind
                    ),
                    endpoint,
                )
            }),
    );
    choices
}

fn routing(app: &Mixer) -> Element<'_, Message> {
    let sources = choices(app, true);
    let destinations = choices(app, false);
    let source_labels: Vec<_> = sources.iter().map(|(label, _)| label.clone()).collect();
    let destination_labels: Vec<_> = destinations
        .iter()
        .map(|(label, _)| label.clone())
        .collect();
    let selected_source = sources
        .iter()
        .position(|(_, value)| Some(value) == app.source.as_ref());
    let selected_destination = destinations
        .iter()
        .position(|(_, value)| Some(value) == app.destination.as_ref());
    let source = widget::dropdown(source_labels, selected_source, move |index| {
        Message::Source(sources[index].1.clone())
    })
    .placeholder("Select source")
    .width(Length::Fill);
    let destination = widget::dropdown(destination_labels, selected_destination, move |index| {
        Message::Destination(destinations[index].1.clone())
    })
    .placeholder("Select destination")
    .width(Length::Fill);
    let mut add = button::suggested("Add route");
    if app.control.baseline.is_some() && app.source.is_some() && app.destination.is_some() {
        add = add.on_press(Message::AddRoute);
    }
    let mut content = column::with_children([
        text::title2("Explicit connections").into(),
        text::body("Source → channel → destination. Applications can also select each channel’s virtual input or output directly. Monitoring is opt-in. Feedback routes are rejected.").into(),
        row::with_children([source.into(), text::body("→").into(), destination.into(), add.into()]).spacing(12).align_y(Alignment::Center).into(),
    ]).spacing(16);
    for (index, route) in app.control.profile.routes.iter().enumerate() {
        let status = app
            .control
            .status
            .as_ref()
            .and_then(|s| s.backend.routes.iter().find(|s| s.id == route.id));
        let label = format!(
            "{} → {}",
            endpoint_name(app, &route.from),
            endpoint_name(app, &route.to)
        );
        let state = status.map_or("Not applied".into(), |s| {
            format!(
                "{:?}{}",
                s.state,
                s.error.as_ref().map_or(String::new(), |e| format!(": {e}"))
            )
        });
        content = content.push(
            widget::container(
                column::with_children([
                    row::with_children([
                        widget::checkbox(route.enabled)
                            .label(label)
                            .on_toggle(move |v| Message::RouteEnabled(index, v))
                            .into(),
                        widget::space::horizontal().into(),
                        button::text("Remove")
                            .on_press(Message::Popup(Popup::RemoveRoute(index)))
                            .into(),
                    ])
                    .spacing(12)
                    .into(),
                    text::caption(state).into(),
                ])
                .spacing(8),
            )
            .padding(12)
            .class(cosmic::theme::Container::Card),
        );
    }
    for bus in &app.control.profile.buses {
        content = content.push(text::caption(format!(
            "{}: playback cosmos.bus.{}.input · recording source cosmos.bus.{}.output",
            bus.name, bus.id, bus.id
        )));
    }
    widget::scrollable(content).height(Length::Fill).into()
}

fn devices(app: &Mixer) -> Element<'_, Message> {
    let mut content = column::with_children([
        system_devices(app),
        text::title2("Mixer-local devices & applications").into(),
        text::body("Input sends audio into the mixer; Output receives audio from it. These local controls do not disable devices for other apps. Hidden is not muted.").into(),
    ]).spacing(16);
    for (index, device) in app
        .control
        .profile
        .devices
        .iter()
        .enumerate()
        .filter(|(_, d)| app.show_hidden || !d.hidden)
    {
        let direction = app
            .control
            .nodes
            .iter()
            .filter(|node| {
                device
                    .identity
                    .iter()
                    .all(|(key, value)| node.identity.get(key) == Some(value))
            })
            .fold(0, |direction, node| {
                direction
                    | match node.media_class.as_str() {
                        "Audio/Source" | "Stream/Output/Audio" => 1,
                        "Audio/Sink" | "Stream/Input/Audio" => 2,
                        _ => 0,
                    }
            });
        let identity = device
            .identity
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" · ");
        content = content.push(
            widget::container(
                column::with_children([
                    widget::text_input("Local alias", app.control.text_edits.alias(device))
                        .on_input(move |v| Message::DeviceAlias(index, v))
                        .into(),
                    text::caption(format!(
                        "{} · {identity}",
                        match direction {
                            1 => "Input · present",
                            2 => "Output · present",
                            3 => "Input and output · present",
                            _ => "Unavailable · direction unknown",
                        }
                    ))
                    .into(),
                    row::with_children([
                        widget::checkbox(device.enabled)
                            .label("Enabled locally")
                            .on_toggle(move |v| Message::DeviceFlag(index, Flag::Enabled, v))
                            .into(),
                        widget::checkbox(device.hidden)
                            .label("Hidden")
                            .on_toggle(move |v| Message::DeviceFlag(index, Flag::Hidden, v))
                            .into(),
                        widget::checkbox(device.muted)
                            .label("Muted locally")
                            .on_toggle(move |v| Message::DeviceFlag(index, Flag::Muted, v))
                            .into(),
                    ])
                    .spacing(16)
                    .into(),
                ])
                .spacing(10),
            )
            .padding(14)
            .class(cosmic::theme::Container::Card),
        );
    }
    for node in app.control.nodes.iter().filter(|node| {
        !node.name.starts_with("cosmos.")
            && !node.identity.is_empty()
            && !app
                .control
                .profile
                .devices
                .iter()
                .any(|device| device.identity == node.identity)
    }) {
        content = content.push(
            row::with_children([
                column::with_children([
                    text::body(&node.description).into(),
                    text::caption(format!(
                        "{} · {} · {:?}",
                        node.name,
                        node_role(&node.media_class),
                        node.layout.kind
                    ))
                    .into(),
                ])
                .width(Length::Fill)
                .into(),
                button::standard("Manage locally")
                    .on_press(Message::Manage(node.clone()))
                    .into(),
            ])
            .spacing(12),
        );
    }
    widget::scrollable(content).height(Length::Fill).into()
}

fn system_devices(app: &Mixer) -> Element<'_, Message> {
    let mut content = column::with_children([
        text::title2("System audio devices").into(),
        text::body("These profiles affect every app. Off disables the entire sound card, which can include both microphones and speakers. Select an available active profile to enable it again.").into(),
    ]).spacing(12);
    if app.control.devices.is_empty() {
        content = content.push(text::caption(
            "No system device profiles available. Start the engine and connect to PipeWire.",
        ));
    }
    if let Some(change) = app
        .control
        .status
        .as_ref()
        .and_then(|status| status.device_profile_change.as_ref())
    {
        content = content.push(text::body(match change.state {
            DeviceProfileState::Pending => {
                "Waiting for PipeWire to confirm the system profile…".to_owned()
            }
            DeviceProfileState::Confirmed => "System profile confirmed by PipeWire.".to_owned(),
            DeviceProfileState::Failed => format!(
                "System profile change failed: {}",
                change.error.as_deref().unwrap_or("Unknown error")
            ),
        }));
    }
    for device in &app.control.devices {
        let mut card = column::with_children([
            text::title3(&device.description).into(),
            text::caption(format!(
                "{} · {}",
                device.name,
                if device.disabled {
                    "Disabled for all apps"
                } else {
                    "System sound card"
                }
            ))
            .into(),
        ])
        .spacing(6);
        if let Some(error) = &device.error {
            card = card.push(text::body(error));
        }
        let selected = device
            .profiles
            .iter()
            .position(|profile| Some(profile.index) == device.current_profile);
        if device.readable
            && device.writable
            && let Some(serial) = device.serial
            && let Some(status) = &app.control.status
            && !device.profiles.is_empty()
            && !app.control.device_busy()
            && !app.control.save_unavailable()
        {
            let labels: Vec<_> = device
                .profiles
                .iter()
                .map(|profile| {
                    format!(
                        "{}{}",
                        if profile.name == "off" {
                            "Off — disable for all apps"
                        } else {
                            &profile.description
                        },
                        if profile.availability == ProfileAvailability::No {
                            " (unavailable)"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            let engine_pid = status.pid;
            let generation = device.generation;
            let id = device.id;
            let profiles: Vec<_> = device
                .profiles
                .iter()
                .map(|profile| profile.index)
                .collect();
            card = card.push(text::caption("System profile")).push(
                widget::dropdown(labels, selected, move |index| {
                    Message::SystemProfile(DeviceProfileRequest {
                        engine_pid,
                        generation,
                        id,
                        serial,
                        profile_index: profiles[index],
                    })
                })
                .placeholder("Select system profile")
                .width(Length::Fill),
            );
        } else {
            card = card.push(text::caption(
                selected.map_or("Profile unavailable".to_owned(), |index| {
                    format!("Profile: {}", device.profiles[index].description)
                }),
            ));
            if !device.writable || device.serial.is_none() {
                card = card.push(text::caption(
                    "System profile changes are not available for this device.",
                ));
            }
        }
        content = content.push(
            widget::container(card)
                .padding(14)
                .class(cosmic::theme::Container::Card),
        );
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
        Popup::SystemProfile {
            device,
            profile,
            disabling,
            ..
        } => {
            let mut confirm = if *disabling {
                button::destructive("Disable for all apps")
            } else {
                button::suggested("Change system profile")
            };
            if !app.control.device_busy() && !app.control.save_unavailable() {
                confirm = confirm.on_press(Message::Confirm);
            }
            return Some(widget::dialog()
                .title(if *disabling { "Disable this system sound card?" } else { "Change this system sound card?" })
                .body(format!("{device}\nSelected profile: {profile}\n\nThis can interrupt every app using this card and remove both its inputs and outputs. The OS may remember this choice. To restore audio, select an active profile here. Mixer-local settings and system defaults are not changed directly."))
                .control(text::body(app.error.as_deref().unwrap_or("")))
                .primary_action(confirm)
                .secondary_action(button::standard("Cancel").on_press(Message::ClosePopup)).into());
        }
        Popup::ReplaceInput(..) => (
            "Replace this channel's input routes?",
            "This channel has multiple incoming routes, including any disabled routes. The selected input will replace all of them. Output routes and other channels are kept. Use Routing if you want to keep a multi-source mix.",
        ),
        Popup::Remove(_) => (
            "Remove channel?",
            "This immediately removes the channel, its routes, and its audio endpoints.",
        ),
        Popup::RemoveEffect(..) => (
            "Remove effect?",
            "This immediately removes the effect and can interrupt this channel.",
        ),
        Popup::RemoveRoute(_) => (
            "Remove route?",
            "This immediately disconnects this audio route.",
        ),
        Popup::Reload => (
            "Save and reload engine settings?",
            "Final changes are saved first. If saving fails, your changes stay here. To recover from a conflict or invalid text, you can explicitly discard pending changes instead.",
        ),
        Popup::Stop => (
            "Save and stop the audio engine?",
            "Final changes are saved first. Then all mixer endpoints and routes stop. Other applications may lose their selected source. If saving fails, the engine is not stopped.",
        ),
        Popup::Close => (
            "Save before closing the mixer?",
            "Final changes are saved first. Close hides to a connected panel indicator; Quit exits only the frontend. Without a panel host, Close exits the frontend. If saving fails or a name is invalid, the window stays open. The engine keeps running.",
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
            .control(controls)
            .primary_action(button::suggested("Confirm").on_press(Message::Confirm))
            .secondary_action(button::standard("Cancel").on_press(Message::ClosePopup))
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
        let mut effect = column::with_children([row::with_children([
            widget::checkbox(instance.enabled)
                .label(format!("{name} · {}", instance.id))
                .on_toggle(move |v| Message::EffectEnabled(index, effect_index, v))
                .into(),
            widget::space::horizontal().into(),
            button::text("Remove")
                .on_press(Message::Popup(Popup::RemoveEffect(index, effect_index)))
                .into(),
        ])
        .spacing(12)
        .into()])
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
        row::with_children([button::standard("+ EQ band")
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
        row::with_children([
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
    widget::dialog().title(format!("{} · {}", bus.name, if equalizer { "Equalizer & tone" } else { "Dynamics" }))
        .body("Controls change audio and save immediately, including while dragging. Adding or removing an effect can interrupt this channel.")
        .control(widget::scrollable(controls).height(420)).width(680)
        .primary_action(button::standard("Close").on_press(Message::ClosePopup)).into()
}
