// SPDX-License-Identifier: MPL-2.0
use std::time::Duration;

use cosmic::app::Task;
use cosmic::iced;
use cosmic::prelude::*;
use cosmos_control::{Channels, DeviceSettings, Effect, Node, NodeRef, Route};

use crate::Options;
use crate::connection::{self, AfterSave, Control, Failure, Next, Operation, Reply};
use crate::model::{self, Parameter};

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
}

impl Mixer {
    fn schedule(&mut self) -> Task<Message> {
        match self.control.next() {
            Some(Next::Close) => iced::exit(),
            Some(Next::Request(operation)) => {
                let applying = matches!(operation, Operation::Apply { .. });
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
        ])
    }
    fn on_app_exit(&mut self) -> Option<Message> {
        Some(Message::CloseRequested)
    }
    fn on_escape(&mut self) -> Task<Message> {
        self.popup = None;
        self.control.cancel_action();
        Task::none()
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloseRequested => {
                if self.control.pending()
                    || self.control.text_edits.pending()
                    || self.control.action_busy()
                {
                    self.popup = Some(Popup::Close);
                } else {
                    return iced::exit();
                }
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
        self.schedule()
    }
    fn view(&self) -> Element<'_, Message> {
        crate::view::view(self)
    }
    fn dialog(&self) -> Option<Element<'_, Message>> {
        crate::view::dialog(self)
    }
}
