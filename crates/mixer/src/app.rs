// SPDX-License-Identifier: MPL-2.0
use std::time::Duration;

use cosmic::Application;
use cosmic::app::Task;
use cosmic::iced;
use cosmic::prelude::*;
use cosmos_control::{Channels, Effect, LayoutKind, NodeRef};

use crate::Options;
use crate::connection::{self, AfterSave, Control, Failure, Next, Operation, Reply};
use crate::model::{self, Parameter};
#[derive(Clone, Copy, Debug)]
pub enum Flag {
    Muted,
}
#[derive(Clone, Debug)]
pub enum Popup {
    Effects(usize, bool),
    RemoveEffect(usize, usize),
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
    DragWindow,
    Activate(Option<String>),
    Popup(Popup),
    Name(usize, String),
    CommitName(usize),
    BusFlag(usize, Flag, bool),
    Parameter(usize, Option<usize>, Parameter, f32),
    EffectEnabled(usize, usize, bool),
    AddEffect(usize, Effect),
    Color(usize, f32, f32),
    Input(String, Option<NodeRef>),
}

pub struct Mixer {
    pub core: cosmic::Core,
    pub options: Options,
    pub control: Control,
    pub error: Option<String>,
    pub popup: Option<Popup>,
    diagnostics: cosmos_control::diagnostics::Change,
    text_diagnostics: cosmos_control::diagnostics::Change,
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

    fn begin_close(&mut self) {
        self.save_then(AfterSave::Close);
        if self.error.is_some() {
            self.popup = Some(Popup::Close);
        }
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
}

impl cosmic::Application for Mixer {
    type Executor = cosmic::executor::Default;
    type Flags = Options;
    type Message = Message;
    const APP_ID: &'static str = "io.github.petterssonjonas.CosmosVirtualMic";
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
            popup: None,
            diagnostics: Default::default(),
            text_diagnostics: Default::default(),
        };
        app.core.window.use_template = false;
        app.core.window.show_headerbar = false;
        app.core.window.content_container = false;
        app.core.set_auto_blur(Default::default());
        let title = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title("Cosmos Virtual Mic".into(), id))
            .unwrap_or_else(Task::none);
        // Settings::resizable(None) leaves iced's default enabled in this toolkit revision.
        let fixed_size = app
            .core
            .main_window_id()
            .map(|id| iced::window::set_resizable(id, false))
            .unwrap_or_else(Task::none);
        let connect = app.schedule();
        (app, Task::batch([title, fixed_size, connect]))
    }
    fn subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::batch([
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            iced::window::close_requests().map(|_| Message::CloseRequested),
        ])
    }
    fn dbus_activation(&mut self, message: cosmic::dbus_activation::Message) -> Task<Message> {
        self.activate(message.activation_token.or(message.desktop_startup_id))
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
            Message::CloseRequested => self.begin_close(),
            Message::DragWindow => {
                return self
                    .core
                    .main_window_id()
                    .map(iced::window::drag)
                    .unwrap_or_else(Task::none);
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
                        Some(Popup::Effects(..) | Popup::RemoveEffect(..))
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
                Some(Popup::RemoveEffect(index, effect)) => {
                    if let Some(bus) = self.control.profile.buses.get_mut(index)
                        && effect < bus.effects.len()
                    {
                        bus.effects.remove(effect);
                    }
                    self.popup = None;
                }
                _ => {}
            },
            Message::ClosePopup => {
                self.popup = None;
                self.control.cancel_action();
            }
            Message::Popup(popup) => self.popup = Some(popup),
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
                let layout = match &source {
                    Some(NodeRef::Hardware { identity }) => {
                        let node = self
                            .control
                            .nodes
                            .iter()
                            .find(|node| &node.identity == identity && model::microphone(node));
                        let Some(node) = node else {
                            self.error = Some("The selected microphone is no longer available. Select it again; no replacement was chosen.".into());
                            return Task::none();
                        };
                        match node.layout.kind {
                            LayoutKind::Mono => Some(Channels::Mono),
                            LayoutKind::Stereo => Some(Channels::Stereo),
                            LayoutKind::Unknown => {
                                self.error = Some("The microphone format is not available yet. Wait for discovery, then select it again.".into());
                                return Task::none();
                            }
                            LayoutKind::Unsupported => {
                                self.error = Some("This microphone layout is not supported. Select a mono or stereo microphone; no channels were remapped.".into());
                                return Task::none();
                            }
                        }
                    }
                    Some(NodeRef::Bus { .. }) => {
                        self.error = Some("Select a hardware microphone.".into());
                        return Task::none();
                    }
                    None => None,
                };
                let result =
                    model::set_channel_input(&mut self.control.profile, &bus, source, layout);
                self.edit(result);
            }
        }
        self.diagnostics.error("Mixer", self.error.as_deref());
        self.text_diagnostics
            .error("Name or alias", self.control.text_edits.error());
        self.control.log_changes();
        self.schedule()
    }
    fn view(&self) -> Element<'_, Message> {
        // The template normally owns this modal; retain it without its shell.
        let mut view = cosmic::widget::popover(crate::view::view(self)).modal(true);
        if let Some(dialog) = self.dialog() {
            view = view.popup(cosmic::widget::id_container(
                dialog,
                cosmic::widget::Id::new("COSMIC_dialog"),
            ));
        }
        view.into()
    }
    fn dialog(&self) -> Option<Element<'_, Message>> {
        crate::view::dialog(self)
    }
    fn style(&self) -> Option<iced::theme::Style> {
        let theme = cosmic::theme::active();
        let card = &theme.cosmic().background(false).component;
        Some(iced::theme::Style {
            background_color: card.base.into(),
            icon_color: card.on.into(),
            text_color: card.on.into(),
        })
    }
}
