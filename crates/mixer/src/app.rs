// SPDX-License-Identifier: MPL-2.0
use std::time::Duration;

use cosmic::Application;
use cosmic::app::Task;
use cosmic::iced;
use cosmic::prelude::*;
use cosmos_control::{
    Channels, DeviceProfileRequest, DeviceSettings, Effect, Node, NodeRef, ProfileAvailability,
    Route,
};

use crate::Options;
use crate::connection::{self, AfterSave, Control, Failure, Next, Operation, Reply};
use crate::model::{self, Parameter};
use crate::tray;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Mixer,
    Routing,
    Devices,
}
#[derive(Clone, Copy, Debug)]
pub enum Flag {
    Enabled,
    Hidden,
    Muted,
}
#[derive(Clone, Debug)]
pub enum Popup {
    Effects(usize, bool),
    Remove(usize),
    RemoveEffect(usize, usize),
    RemoveRoute(usize),
    ReplaceInput(String, Option<NodeRef>),
    SystemProfile {
        request: DeviceProfileRequest,
        device: String,
        profile: String,
        disabling: bool,
    },
    Reload,
    Stop,
    Close,
}
#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    Reply(Result<Reply, Failure>),
    Retry,
    Reload,
    DiscardAndReload,
    Start,
    Stop,
    Confirm,
    ClosePopup,
    CloseRequested,
    Quit,
    Tray(tray::Event),
    WindowOpened(Option<String>),
    Activate(Option<String>),
    Page(Page),
    ShowHidden(bool),
    Add(Channels),
    Popup(Popup),
    Name(usize, String),
    CommitName(usize),
    BusFlag(usize, Flag, bool),
    Parameter(usize, Option<usize>, Parameter, f32),
    EffectEnabled(usize, usize, bool),
    AddEffect(usize, Effect),
    Color(usize, f32, f32),
    Source(NodeRef),
    Destination(NodeRef),
    AddRoute,
    Input(String, Option<NodeRef>),
    SystemProfile(DeviceProfileRequest),
    RouteEnabled(usize, bool),
    Manage(Node),
    DeviceAlias(usize, String),
    DeviceFlag(usize, Flag, bool),
}

pub struct Mixer {
    pub core: cosmic::Core,
    pub options: Options,
    pub control: Control,
    pub error: Option<String>,
    pub page: Page,
    pub popup: Option<Popup>,
    pub show_hidden: bool,
    pub source: Option<NodeRef>,
    pub destination: Option<NodeRef>,
    diagnostics: cosmos_control::diagnostics::Change,
    text_diagnostics: cosmos_control::diagnostics::Change,
    pub indicator_status: Result<(), String>,
    indicator_state: Option<tokio::sync::watch::Sender<tray::State>>,
    close_intent: Option<CloseIntent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseIntent {
    Window,
    Quit,
}

fn hide_after_save(intent: CloseIntent, host_available: bool) -> bool {
    intent == CloseIntent::Window && host_available
}

impl Mixer {
    fn schedule(&mut self) -> Task<Message> {
        match self.control.next() {
            Some(Next::Close) => {
                let intent = self.close_intent.take().unwrap_or(CloseIntent::Window);
                if hide_after_save(intent, self.indicator_status.is_ok())
                    && let Some(id) = self.core.set_main_window_id(None)
                {
                    return iced::window::close(id);
                }
                iced::exit()
            }
            Some(Next::Request(operation)) => {
                let applying = matches!(
                    operation,
                    Operation::Apply { .. } | Operation::DeviceProfile(_)
                );
                let options = self.options.clone();
                iced::Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || connection::execute(options, operation))
                            .await
                            .map_err(|error| {
                                let error = format!("Control worker failed: {error}");
                                if applying {
                                    Failure::Uncertain(error)
                                } else {
                                    Failure::Transport(error)
                                }
                            })?
                    },
                    |reply| cosmic::Action::App(Message::Reply(reply)),
                )
            }
            None => Task::none(),
        }
    }

    fn edit(&mut self, result: Result<(), String>) {
        self.error = result.err();
    }

    fn save_then(&mut self, action: AfterSave) {
        match self.control.after_save(action) {
            Ok(()) => {
                self.error = None;
                self.popup = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn begin_close(&mut self, intent: CloseIntent) {
        if intent == CloseIntent::Window && self.core.main_window_id().is_none() {
            return;
        }
        self.close_intent = Some(intent);
        self.save_then(AfterSave::Close);
        if self.error.is_some() {
            self.popup = Some(Popup::Close);
        }
    }

    fn show(&mut self, token: Option<String>) -> Task<Message> {
        if self.close_intent.take().is_some() {
            self.control.cancel_action();
            if matches!(self.popup, Some(Popup::Close)) {
                self.popup = None;
            }
        }
        if self.core.main_window_id().is_none() {
            let mut settings = iced::window::Settings {
                size: iced::Size::new(1180.0, 760.0),
                min_size: Some(iced::Size::new(860.0, 560.0)),
                exit_on_close_request: false,
                decorations: false,
                transparent: true,
                ..Default::default()
            };
            settings.platform_specific.application_id = Self::APP_ID.into();
            let (id, open) = iced::window::open(settings);
            self.core.set_main_window_id(Some(id));
            return open.map(move |_| cosmic::Action::App(Message::WindowOpened(token.clone())));
        }
        self.activate(token)
    }

    fn activate(&self, token: Option<String>) -> Task<Message> {
        let Some(id) = self.core.main_window_id() else {
            return Task::none();
        };
        if let Some(token) = token {
            Task::done(cosmic::Action::Cosmic(cosmic::app::Action::Activate(token)))
        } else {
            iced::platform_specific::shell::commands::activation::request_token(
                Some(Self::APP_ID.into()),
                Some(id),
            )
            .map(|token| cosmic::Action::App(Message::Activate(token)))
        }
    }

    fn update_indicator(&self) {
        if let Some(sender) = &self.indicator_state {
            let latest = tray::State {
                running: self.control.status.is_some(),
                busy: self.control.action_busy(),
                visible: self.core.main_window_id().is_some(),
            };
            sender.send_if_modified(|state| {
                if *state == latest {
                    false
                } else {
                    *state = latest;
                    true
                }
            });
        }
    }
}

impl cosmic::Application for Mixer {
    type Executor = cosmic::executor::Default;
    type Flags = Options;
    type Message = Message;
    const APP_ID: &'static str = "io.github.petterssonjonas.CosmosAudioMixer";
    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }
    fn init(core: cosmic::Core, options: Options) -> (Self, Task<Message>) {
        let mut app = Self {
            core,
            options,
            control: Control::default(),
            error: None,
            page: Page::Mixer,
            popup: None,
            show_hidden: false,
            source: None,
            destination: None,
            diagnostics: Default::default(),
            text_diagnostics: Default::default(),
            indicator_status: Err("Panel indicator: checking for a host…".into()),
            indicator_state: None,
            close_intent: None,
        };
        app.set_header_title("Cosmos Audio Mixer".into());
        let title = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title("Cosmos Audio Mixer".into(), id))
            .unwrap_or_else(Task::none);
        let connect = app.schedule();
        (app, Task::batch([title, connect]))
    }
    fn subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::batch([
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            iced::window::close_requests().map(|_| Message::CloseRequested),
            tray::subscription().map(Message::Tray),
        ])
    }
    fn dbus_activation(&mut self, message: cosmic::dbus_activation::Message) -> Task<Message> {
        self.show(message.activation_token.or(message.desktop_startup_id))
    }
    fn on_app_exit(&mut self) -> Option<Message> {
        Some(Message::CloseRequested)
    }
    fn on_escape(&mut self) -> Task<Message> {
        self.popup = None;
        self.control.cancel_action();
        self.close_intent = None;
        Task::none()
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloseRequested => self.begin_close(CloseIntent::Window),
            Message::Quit => {
                let show = self.show(None);
                self.begin_close(CloseIntent::Quit);
                return show.chain(self.schedule());
            }
            Message::Tray(tray::Event::Ready(sender)) => {
                self.indicator_state = Some(sender);
                self.update_indicator();
            }
            Message::Tray(tray::Event::Host(status)) => {
                self.indicator_status = status;
                if self.indicator_status.is_err() && self.core.main_window_id().is_none() {
                    return self.show(None);
                }
            }
            Message::Tray(tray::Event::Action(action, token)) => {
                if matches!(action, tray::Action::Hide) {
                    if self.core.main_window_id().is_some() {
                        self.begin_close(CloseIntent::Window);
                    }
                    return self.schedule();
                }
                let show = self.show(token);
                match action {
                    tray::Action::Show => return show,
                    tray::Action::Start if !self.control.action_busy() => {
                        self.control.start();
                    }
                    tray::Action::Stop if !self.control.action_busy() => {
                        self.popup = Some(Popup::Stop);
                    }
                    tray::Action::Quit => self.begin_close(CloseIntent::Quit),
                    _ => {}
                }
                self.update_indicator();
                return show.chain(self.schedule());
            }
            Message::WindowOpened(token) => {
                let title = self
                    .core
                    .main_window_id()
                    .map(|id| self.set_window_title("Cosmos Audio Mixer".into(), id))
                    .unwrap_or_else(Task::none);
                return title.chain(self.activate(token));
            }
            Message::Activate(token) => {
                if let Some(token) = token {
                    return self.activate(Some(token));
                }
                return self
                    .core
                    .main_window_id()
                    .map(|id| {
                        iced::window::request_user_attention(
                            id,
                            Some(iced::window::UserAttention::Informational),
                        )
                    })
                    .unwrap_or_else(Task::none);
            }
            Message::Tick => self.control.tick(),
            Message::Reply(result) => {
                if self.control.finish(result)
                    && matches!(
                        self.popup,
                        Some(
                            Popup::Effects(..)
                                | Popup::Remove(_)
                                | Popup::RemoveEffect(..)
                                | Popup::RemoveRoute(_)
                                | Popup::ReplaceInput(..)
                        )
                    )
                {
                    self.popup = None;
                }
            }
            Message::Retry => {
                self.error = None;
                self.control.retry();
            }
            Message::Start => {
                self.error = None;
                self.popup = None;
                self.control.start();
            }
            Message::Stop => self.popup = Some(Popup::Stop),
            Message::Reload => self.popup = Some(Popup::Reload),
            Message::DiscardAndReload => {
                if matches!(self.popup, Some(Popup::Reload)) && self.control.discard_and_reload() {
                    self.error = None;
                    self.popup = None;
                }
            }
            Message::Confirm => match self.popup.clone() {
                Some(Popup::Stop) => self.save_then(AfterSave::Stop),
                Some(Popup::Reload) => self.save_then(AfterSave::Reload),
                Some(Popup::Close) => self.save_then(AfterSave::Close),
                Some(Popup::ReplaceInput(bus, source)) => {
                    let result =
                        model::set_channel_input(&mut self.control.profile, &bus, source, true);
                    self.edit(result);
                    self.popup = None;
                }
                Some(Popup::SystemProfile { request, .. }) => {
                    match self.control.set_device_profile(request) {
                        Ok(()) => {
                            self.popup = None;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Some(Popup::Remove(index)) if index < self.control.profile.buses.len() => {
                    let removed = self.control.profile.buses.remove(index);
                    self.control.profile.routes.retain(|route| {
                        !matches!(&route.from, NodeRef::Bus { id } if id == &removed.id)
                            && !matches!(&route.to, NodeRef::Bus { id } if id == &removed.id)
                    });
                    self.control.text_edits.prune(&self.control.profile);
                    self.popup = None;
                }
                Some(Popup::RemoveEffect(index, effect)) => {
                    if let Some(bus) = self.control.profile.buses.get_mut(index)
                        && effect < bus.effects.len()
                    {
                        bus.effects.remove(effect);
                    }
                    self.popup = None;
                }
                Some(Popup::RemoveRoute(index)) => {
                    if index < self.control.profile.routes.len() {
                        self.control.profile.routes.remove(index);
                    }
                    self.popup = None;
                }
                _ => {}
            },
            Message::ClosePopup => {
                self.popup = None;
                self.control.cancel_action();
                self.close_intent = None;
            }
            Message::Page(page) => self.page = page,
            Message::ShowHidden(show) => self.show_hidden = show,
            Message::Popup(popup) => self.popup = Some(popup),
            Message::Add(channels) if self.control.baseline.is_some() => {
                match model::new_bus(&self.control.profile, channels) {
                    Ok(bus) => {
                        self.control.profile.buses.push(bus);
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Message::Add(_) => {}
            Message::Name(index, name) => {
                self.control
                    .text_edits
                    .rename(&mut self.control.profile, index, name);
                self.error = None;
            }
            Message::CommitName(index) => {
                let result = self
                    .control
                    .text_edits
                    .commit_name(&mut self.control.profile, index);
                self.edit(result);
            }
            Message::BusFlag(index, flag, value) => {
                if let Some(bus) = self.control.profile.buses.get_mut(index) {
                    match flag {
                        Flag::Enabled => bus.enabled = value,
                        Flag::Hidden => bus.hidden = value,
                        Flag::Muted => bus.muted = value,
                    }
                }
            }
            Message::Parameter(index, effect, parameter, value) => {
                if let Some(bus) = self.control.profile.buses.get_mut(index) {
                    let result = model::set_parameter(bus, effect, parameter, value);
                    self.edit(result);
                }
            }
            Message::EffectEnabled(index, effect, value) => {
                if let Some(instance) = self
                    .control
                    .profile
                    .buses
                    .get_mut(index)
                    .and_then(|bus| bus.effects.get_mut(effect))
                {
                    instance.enabled = value;
                }
            }
            Message::AddEffect(index, effect) => {
                if let Some(bus) = self.control.profile.buses.get_mut(index) {
                    let result = model::add_effect(bus, effect);
                    self.edit(result);
                }
            }
            Message::Color(index, warmth, brightness) => {
                if let Some(bus) = self.control.profile.buses.get_mut(index) {
                    let result = model::set_color(bus, warmth, brightness);
                    self.edit(result);
                }
            }
            Message::Input(bus, source) => {
                if model::channel_inputs(&self.control.profile, &bus).count() > 1 {
                    self.popup = Some(Popup::ReplaceInput(bus, source));
                } else {
                    let result =
                        model::set_channel_input(&mut self.control.profile, &bus, source, false);
                    self.edit(result);
                }
            }
            Message::SystemProfile(request) => {
                if let Some(device) = self.control.devices.iter().find(|device| {
                    device.id == request.id
                        && device.generation == request.generation
                        && device.serial == Some(request.serial)
                }) && let Some(profile) = device
                    .profiles
                    .iter()
                    .find(|profile| profile.index == request.profile_index)
                {
                    if !device.writable || profile.availability == ProfileAvailability::No {
                        self.error = Some(
                            "This system profile is unavailable or the device is read-only.".into(),
                        );
                    } else if device.current_profile != Some(profile.index) {
                        self.popup = Some(Popup::SystemProfile {
                            request,
                            device: device.description.clone(),
                            profile: profile.description.clone(),
                            disabling: profile.name == "off",
                        });
                        self.error = None;
                    }
                } else {
                    self.error = Some(
                        "The device or profile changed. Select it again from the refreshed list."
                            .into(),
                    );
                }
            }
            Message::Source(source) => self.source = Some(source),
            Message::Destination(destination) => self.destination = Some(destination),
            Message::AddRoute => {
                if let (Some(from), Some(to)) = (&self.source, &self.destination) {
                    let id = (1..=cosmos_control::model::MAX_ROUTES + 1)
                        .map(|n| format!("route-{n}"))
                        .find(|id| {
                            !self
                                .control
                                .profile
                                .routes
                                .iter()
                                .any(|route| &route.id == id)
                        });
                    if let Some(id) = id {
                        self.control.profile.routes.push(Route {
                            id,
                            from: from.clone(),
                            to: to.clone(),
                            enabled: true,
                        });
                        if let Err(error) = self.control.profile.validate() {
                            self.control.profile.routes.pop();
                            self.error = Some(error.to_string());
                        } else {
                            self.error = None;
                        }
                    }
                }
            }
            Message::RouteEnabled(index, enabled) => {
                let result = model::set_route_enabled(&mut self.control.profile, index, enabled);
                self.edit(result);
            }
            Message::Manage(node) => {
                if !self
                    .control
                    .profile
                    .devices
                    .iter()
                    .any(|device| device.identity == node.identity)
                {
                    self.control.profile.devices.push(DeviceSettings {
                        identity: node.identity,
                        alias: node.description,
                        enabled: true,
                        hidden: false,
                        muted: false,
                    });
                    if let Err(error) = self.control.profile.validate() {
                        self.control.profile.devices.pop();
                        self.error = Some(error.to_string());
                    } else {
                        self.error = None;
                    }
                }
            }
            Message::DeviceAlias(index, alias) => {
                self.control
                    .text_edits
                    .rename_device(&mut self.control.profile, index, alias);
                self.error = None;
            }
            Message::DeviceFlag(index, flag, value) => {
                if let Some(device) = self.control.profile.devices.get_mut(index) {
                    match flag {
                        Flag::Enabled => device.enabled = value,
                        Flag::Hidden => device.hidden = value,
                        Flag::Muted => device.muted = value,
                    }
                }
            }
        }
        self.diagnostics.error("Mixer", self.error.as_deref());
        self.text_diagnostics
            .error("Name or alias", self.control.text_edits.error());
        self.control.log_changes();
        self.update_indicator();
        self.schedule()
    }
    fn view(&self) -> Element<'_, Message> {
        crate::view::view(self)
    }
    fn view_window(&self, _id: iced::window::Id) -> Element<'_, Message> {
        // A closing old native window can request one last frame after a new main ID is set.
        crate::view::view(self)
    }
    fn dialog(&self) -> Option<Element<'_, Message>> {
        crate::view::dialog(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseIntent, hide_after_save};

    #[test]
    fn window_close_hides_only_with_a_host_after_final_save() {
        assert!(hide_after_save(CloseIntent::Window, true));
        assert!(!hide_after_save(CloseIntent::Window, false));
    }

    #[test]
    fn explicit_quit_never_hides_the_frontend() {
        assert!(!hide_after_save(CloseIntent::Quit, true));
        assert!(!hide_after_save(CloseIntent::Quit, false));
    }
}
