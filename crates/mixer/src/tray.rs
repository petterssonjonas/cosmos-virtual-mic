// SPDX-License-Identifier: MPL-2.0
use std::time::Duration;

use cosmic::iced::{
    self,
    futures::{SinkExt, StreamExt},
};
use ksni::TrayMethods;
use tokio::sync::{mpsc, watch};

pub const APP_ID: &str = "io.github.petterssonjonas.CosmosAudioMixer";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub running: bool,
    pub busy: bool,
    pub visible: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    Show,
    Hide,
    Start,
    Stop,
    Quit,
}

#[derive(Clone, Debug)]
pub enum Event {
    Ready(watch::Sender<State>),
    Host(Result<(), String>),
    Action(Action, Option<String>),
}

#[derive(Debug)]
struct Indicator {
    state: State,
    actions: mpsc::Sender<Event>,
}

impl Indicator {
    fn action(&self, action: Action) {
        // Click bursts cannot create unbounded GUI work. Full queues drop extra clicks.
        let _ = self
            .actions
            .try_send(Event::Action(action, ksni::current_xdg_activation_token()));
    }
}

impl ksni::Tray for Indicator {
    fn id(&self) -> String {
        APP_ID.into()
    }
    fn title(&self) -> String {
        "Cosmos Audio Mixer".into()
    }
    fn icon_name(&self) -> String {
        APP_ID.into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Supply pixels too: an uninstalled workstation bundle has no icon-theme entry.
        let mut data = vec![0; 24 * 24 * 4];
        for (x, knob) in [(6, 8), (12, 16), (18, 10)] {
            for y in 4..20 {
                let pixel = (y * 24 + x) * 4;
                data[pixel..pixel + 4].copy_from_slice(&[255, 103, 207, 198]);
            }
            for y in knob - 2..knob + 2 {
                for xx in x - 2..x + 3 {
                    let pixel = (y * 24 + xx) * 4;
                    data[pixel..pixel + 4].copy_from_slice(&[255, 103, 207, 198]);
                }
            }
        }
        vec![ksni::Icon {
            width: 24,
            height: 24,
            data,
        }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Cosmos Audio Mixer".into(),
            description: if self.state.running {
                "Audio engine running"
            } else {
                "Audio engine offline"
            }
            .into(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.action(Action::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open Mixer".into(),
                activate: Box::new(|tray: &mut Self| tray.action(Action::Show)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Hide Mixer".into(),
                enabled: self.state.visible && !self.state.busy,
                activate: Box::new(|tray: &mut Self| tray.action(Action::Hide)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.state.running {
                    "Stop engine…"
                } else {
                    "Start engine"
                }
                .into(),
                enabled: !self.state.busy,
                activate: Box::new(|tray: &mut Self| {
                    tray.action(if tray.state.running {
                        Action::Stop
                    } else {
                        Action::Start
                    })
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit Mixer (leave engine running)".into(),
                activate: Box::new(|tray: &mut Self| tray.action(Action::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        let _ = self
            .actions
            .try_send(Event::Host(Err("Panel indicator host disconnected".into())));
        false
    }
}

struct Running(ksni::Handle<Indicator>);
impl Drop for Running {
    fn drop(&mut self) {
        drop(self.0.shutdown());
    }
}

type Output = iced::futures::channel::mpsc::Sender<Event>;

pub fn subscription() -> iced::Subscription<Event> {
    iced::Subscription::run(|| iced::stream::channel(16, run))
}

async fn run(mut output: Output) {
    let (state_tx, mut state_rx) = watch::channel(State::default());
    if output.send(Event::Ready(state_tx)).await.is_err() {
        return;
    }
    loop {
        let result = session(&mut output, &mut state_rx).await;
        if output.send(Event::Host(Err(result))).await.is_err() {
            return;
        }
        // One reconnect worker, including when the bus/host is absent at login.
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn session(output: &mut Output, state: &mut watch::Receiver<State>) -> String {
    match tokio::time::timeout(CALL_TIMEOUT, connect()).await {
        Ok(Ok((connection, proxy))) => match serve(output, state, &connection, proxy).await {
            Ok(()) => "Panel indicator disconnected; reconnecting".into(),
            Err(error) => format!("Panel indicator unavailable: {error}"),
        },
        Ok(Err(error)) => format!("Panel indicator unavailable: {error}"),
        Err(_) => "Panel indicator connection timed out; reconnecting".into(),
    }
}

async fn connect() -> zbus::Result<(zbus::Connection, zbus::Proxy<'static>)> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::proxy::Builder::<zbus::Proxy<'static>>::new(&connection)
        .destination(WATCHER)?
        .path(WATCHER_PATH)?
        .interface(WATCHER)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    Ok((connection, proxy))
}

async fn host_registered(proxy: &zbus::Proxy<'_>) -> bool {
    matches!(
        tokio::time::timeout(
            CALL_TIMEOUT,
            proxy.get_property::<bool>("IsStatusNotifierHostRegistered")
        )
        .await,
        Ok(Ok(true))
    )
}

async fn serve(
    output: &mut Output,
    state: &mut watch::Receiver<State>,
    connection: &zbus::Connection,
    proxy: zbus::Proxy<'static>,
) -> Result<(), String> {
    let mut owners = proxy
        .receive_owner_changed()
        .await
        .map_err(|error| error.to_string())?;
    let mut signals = proxy
        .receive_all_signals()
        .await
        .map_err(|error| error.to_string())?;
    let properties = zbus::Proxy::new(
        connection,
        WATCHER,
        WATCHER_PATH,
        "org.freedesktop.DBus.Properties",
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut changes = properties
        .receive_signal("PropertiesChanged")
        .await
        .map_err(|error| error.to_string())?;
    let mut messages = zbus::MessageStream::from(connection);
    let (actions, mut clicks) = mpsc::channel(8);
    let tray = Indicator {
        state: *state.borrow_and_update(),
        actions,
    };
    let handle = tokio::time::timeout(CALL_TIMEOUT, tray.spawn())
        .await
        .map_err(|_| "registration timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    let running = Running(handle);
    if !host_registered(&proxy).await {
        return Err("No registered panel indicator host".into());
    }
    output
        .send(Event::Host(Ok(())))
        .await
        .map_err(|_| "Frontend closed".to_owned())?;
    loop {
        tokio::select! {
            event = clicks.recv() => match event {
                Some(Event::Host(Err(error))) => return Err(error),
                Some(event) => output.send(event).await.map_err(|_| "Frontend closed".to_owned())?,
                None => return Ok(()),
            },
            changed = state.changed() => {
                if changed.is_err() { return Ok(()); }
                let latest = *state.borrow_and_update();
                if !matches!(tokio::time::timeout(CALL_TIMEOUT, running.0.update(|tray| tray.state = latest)).await, Ok(Some(()))) {
                    return Err("Indicator stopped responding".into());
                }
            },
            owner = owners.next() => {
                if !matches!(owner, Some(Some(_))) { return Err("Panel indicator host disconnected".into()); }
                // ksni must register with the replacement watcher before hiding is safe.
                return Err("Panel indicator host changed; registering again".into());
            },
            signal = signals.next() => {
                if signal.is_none() || !host_registered(&proxy).await { return Err("No registered panel indicator host".into()); }
            },
            change = changes.next() => {
                if change.is_none() || !host_registered(&proxy).await { return Err("No registered panel indicator host".into()); }
            },
            message = messages.next() => {
                if !matches!(message, Some(Ok(_))) { return Err("Session bus disconnected".into()); }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_bursts_are_bounded_and_do_not_block() {
        let (actions, mut events) = mpsc::channel(1);
        let tray = Indicator {
            state: State::default(),
            actions,
        };
        tray.action(Action::Show);
        tray.action(Action::Quit);
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Action(Action::Show, _))
        ));
        assert!(events.try_recv().is_err());
    }
}
