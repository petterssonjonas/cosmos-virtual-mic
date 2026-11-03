// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use cosmos_control::{BackendStatus, Layout, LayoutKind, Node, Phase, Status};
use pipewire as pw;
use pw::proxy::{Listener, ProxyT};
use pw::spa::param::{
    ParamInfoFlags, ParamType,
    audio::AudioInfoRaw,
    format::{MediaSubtype, MediaType},
    format_utils,
};
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

#[path = "devices.rs"]
mod devices;
use devices::DeviceRecord;

pub const MAX_NODES: usize = 256;
const MAX_DEVICES: usize = 128;
const MAX_PROPERTY_BYTES: usize = 512;
const MAX_PORTS: usize = 2048;
const MAX_LINKS: usize = 2048;
const KEYS: &[&str] = &[
    "node.name",
    "node.description",
    "node.nick",
    "media.class",
    "object.path",
    "object.serial",
    "device.id",
    "device.name",
    "device.description",
    "device.nick",
    "device.serial",
    "device.bus-id",
    "device.bus-path",
    "device.api",
    "api.alsa.path",
    "api.alsa.pcm.card",
    "api.alsa.pcm.device",
    "api.alsa.pcm.subdevice",
    "api.bluez5.address",
    "api.bluez5.profile",
    "audio.channels",
    "audio.position",
    "cosmos.owner",
    "cosmos.bus.id",
];
const IDENTITY_KEYS: &[&str] = &[
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
type Properties = BTreeMap<&'static str, String>;

#[derive(Default)]
struct Record {
    properties: Properties,
    current_layout: Option<Layout>,
    available_layout: Option<Layout>,
    format_readable: bool,
    enum_format_readable: bool,
}

impl Record {
    fn update_param_flags(&mut self, parameter_type: ParamType, flags: ParamInfoFlags) {
        let readable = flags.contains(ParamInfoFlags::READ);
        match parameter_type {
            ParamType::Format => {
                self.format_readable = readable;
                if !readable {
                    // Suspension need not emit a null parameter.
                    self.current_layout = None;
                }
            }
            ParamType::EnumFormat => {
                self.enum_format_readable = readable;
                if !readable {
                    self.available_layout = None;
                }
            }
            _ => {}
        }
    }

    fn readable_params(&self) -> &'static [ParamType] {
        match (self.format_readable, self.enum_format_readable) {
            (true, true) => &[ParamType::Format, ParamType::EnumFormat],
            (true, false) => &[ParamType::Format],
            (false, true) => &[ParamType::EnumFormat],
            (false, false) => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub id: u32,
    pub node_id: u32,
    pub output: bool,
    pub channel: String,
    pub monitor: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLink {
    pub output_port: u32,
    pub input_port: u32,
    pub owned: bool,
}

pub struct Observed {
    pub status: Status,
    nodes: BTreeMap<u32, Record>,
    devices: BTreeMap<u32, DeviceRecord>,
    device_operation: Option<devices::PendingProfile>,
    device_operation_sequence: u64,
    pub ports: BTreeMap<u32, Port>,
    pub links: BTreeMap<u32, GraphLink>,
    pub graph_revision: u64,
    pub policy_id: Option<u32>,
    pub policy_revision: u64,
    pub connection_error: Option<String>,
}

impl Observed {
    pub fn new() -> Self {
        Self {
            status: Status {
                pid: std::process::id(),
                phase: Phase::Disconnected,
                generation: 0,
                last_error: None,
                retry_ms: 0,
                discovery_error: None,
                last_control_error: None,
                backend: BackendStatus::default(),
                config_error: None,
                config_pending: false,
                device_profile_change: None,
            },
            nodes: BTreeMap::new(),
            devices: BTreeMap::new(),
            device_operation: None,
            device_operation_sequence: 0,
            ports: BTreeMap::new(),
            links: BTreeMap::new(),
            graph_revision: 0,
            policy_id: None,
            policy_revision: 0,
            connection_error: None,
        }
    }

    pub fn reset(&mut self) {
        self.fail_device_profile("PipeWire disconnected during native device profile change");
        self.nodes.clear();
        self.devices.clear();
        self.ports.clear();
        self.links.clear();
        self.policy_id = None;
        self.policy_revision = self.policy_revision.wrapping_add(1);
        self.graph_revision = self.graph_revision.wrapping_add(1);
        self.connection_error = None;
        self.status.discovery_error = None;
    }

    pub fn snapshot(&self) -> Vec<Node> {
        if self.status.phase != Phase::Connected {
            return Vec::new();
        }
        self.nodes
            .iter()
            .map(|(&id, record)| {
                let properties = &record.properties;
                let device = properties
                    .get("device.id")
                    .and_then(|id| id.parse::<u32>().ok())
                    .and_then(|id| self.devices.get(&id));
                let identity = IDENTITY_KEYS
                    .iter()
                    .filter_map(|&key| {
                        properties
                            .get(key)
                            .or_else(|| device.and_then(|device| device.properties.get(key)))
                            .map(|value| (key.to_owned(), value.clone()))
                    })
                    .collect();
                let property_layout = property_layout(properties);
                let layout = record
                    .current_layout
                    .as_ref()
                    .or_else(|| {
                        (property_layout.kind != LayoutKind::Unknown).then_some(&property_layout)
                    })
                    .or(record.available_layout.as_ref())
                    .unwrap_or(&property_layout)
                    .clone();
                Node {
                    id,
                    generation: self.status.generation,
                    name: properties.get("node.name").cloned().unwrap_or_default(),
                    description: properties
                        .get("node.description")
                        .or_else(|| properties.get("node.nick"))
                        .cloned()
                        .unwrap_or_default(),
                    media_class: properties.get("media.class").cloned().unwrap_or_default(),
                    identity,
                    layout,
                }
            })
            .collect()
    }

    pub fn is_owned(&self, id: u32) -> bool {
        self.nodes.get(&id).is_some_and(|record| {
            record
                .properties
                .get("cosmos.owner")
                .is_some_and(|owner| owner == "cosmos-audio-mixer")
        })
    }

    pub fn node_serial(&self, id: u32) -> Option<&str> {
        self.nodes
            .get(&id)?
            .properties
            .get("object.serial")
            .map(String::as_str)
    }

    pub fn owned_node_serials(&self) -> impl Iterator<Item = (u32, &str)> {
        self.nodes
            .iter()
            .filter(|(_, record)| {
                record.properties.get("cosmos.owner").map(String::as_str)
                    == Some("cosmos-audio-mixer")
            })
            .filter_map(|(&id, record)| {
                record
                    .properties
                    .get("object.serial")
                    .map(|serial| (id, serial.as_str()))
            })
    }

    fn core_error(&mut self, id: u32, result: i32, message: &str) {
        if result == -libc::ENOENT || result == -libc::ESTALE {
            // A request can race object removal, which can be reported on the core.
            // Reconcile without turning missing resources into a persistent error.
            self.graph_revision = self.graph_revision.wrapping_add(1);
            return;
        }
        let message: String = message.chars().take(512).collect();
        // Native filter-chain treats only core EPIPE as connection loss.
        if id == pw::core::PW_ID_CORE && result == -libc::EPIPE {
            self.connection_error =
                Some(format!("PipeWire connection failed ({result}): {message}"));
        } else {
            self.status.discovery_error =
                Some(format!("PipeWire object {id} failed ({result}): {message}"));
            self.graph_revision = self.graph_revision.wrapping_add(1);
        }
    }
}

fn update(properties: &mut Properties, dictionary: &DictRef) -> bool {
    let mut oversized = false;
    for &key in KEYS {
        if let Some(value) = dictionary.get(key) {
            if value.len() > MAX_PROPERTY_BYTES {
                properties.remove(key);
                oversized = true;
            } else {
                properties.insert(key, value.to_owned());
            }
        }
    }
    oversized
}

fn property_layout(properties: &Properties) -> Layout {
    let positions: Vec<String> = properties
        .get("audio.position")
        .map(|positions| {
            positions
                .split(|character: char| {
                    character.is_whitespace() || matches!(character, '[' | ']' | ',' | '"')
                })
                .filter(|position| !position.is_empty())
                .take(65)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let channels = properties
        .get("audio.channels")
        .and_then(|count| count.parse().ok())
        .or_else(|| (!positions.is_empty()).then_some(positions.len() as u32));
    Layout::new(channels, positions)
}

fn parameter_layout(parameter: &pw::spa::pod::Pod) -> Option<Layout> {
    let (media_type, subtype) = format_utils::parse_format(parameter).ok()?;
    if media_type != MediaType::Audio || subtype != MediaSubtype::Raw {
        return Some(Layout {
            kind: LayoutKind::Unsupported,
            channels: None,
            positions: vec![],
        });
    }
    let mut info = AudioInfoRaw::new();
    info.parse(parameter).ok()?;
    let channels = info.channels();
    if channels == 0 {
        return None;
    }
    let positions = if channels as usize <= pw::spa::param::audio::MAX_CHANNELS
        && !info
            .flags()
            .contains(pw::spa::param::audio::AudioInfoRawFlags::UNPOSITIONED)
    {
        info.position()[..channels as usize]
            .iter()
            .map(|&position| channel_name(position))
            .collect()
    } else {
        Vec::new()
    };
    Some(Layout::new(Some(channels), positions))
}

fn channel_name(channel: u32) -> String {
    let name = match channel {
        pw::spa::sys::SPA_AUDIO_CHANNEL_MONO => "MONO",
        pw::spa::sys::SPA_AUDIO_CHANNEL_FL => "FL",
        pw::spa::sys::SPA_AUDIO_CHANNEL_FR => "FR",
        pw::spa::sys::SPA_AUDIO_CHANNEL_FC => "FC",
        pw::spa::sys::SPA_AUDIO_CHANNEL_LFE => "LFE",
        pw::spa::sys::SPA_AUDIO_CHANNEL_RL => "RL",
        pw::spa::sys::SPA_AUDIO_CHANNEL_RR => "RR",
        pw::spa::sys::SPA_AUDIO_CHANNEL_SL => "SL",
        pw::spa::sys::SPA_AUDIO_CHANNEL_SR => "SR",
        _ => return format!("SPA:{channel}"),
    };
    name.to_owned()
}

struct Binding {
    // Listeners must unregister before the native proxy is destroyed.
    _listener: Box<dyn Listener>,
    object: BoundObject,
}

enum BoundObject {
    Node(pw::node::Node),
    Device(pw::device::Device),
    Other { _proxy: Box<dyn ProxyT> },
}

type Bindings = Rc<RefCell<BTreeMap<u32, Binding>>>;

pub struct Connection {
    // Drop callbacks first, then bound objects, registry, and finally the core.
    _registry_listener: pw::registry::Listener,
    _core_listener: pw::core::Listener,
    _bindings: Bindings,
    _registry: pw::registry::RegistryRc,
    _core: pw::core::CoreRc,
    context: pw::context::ContextRc,
}

impl Connection {
    pub fn core(&self) -> &pw::core::CoreRc {
        &self._core
    }

    pub fn context(&self) -> &pw::context::ContextRc {
        &self.context
    }

    pub fn set_node_param(&self, id: u32, parameter: &pw::spa::pod::Pod) -> bool {
        let bindings = self._bindings.borrow();
        if let Some(Binding {
            object: BoundObject::Node(node),
            ..
        }) = bindings.get(&id)
        {
            node.set_param(ParamType::Props, 0, parameter);
            true
        } else {
            false
        }
    }

    pub fn connect(
        main_loop: &pw::main_loop::MainLoopRc,
        remote: Option<&str>,
        observed: &Rc<RefCell<Observed>>,
    ) -> Result<Self, pw::Error> {
        let context = pw::context::ContextRc::new(main_loop, None)?;
        let mut properties = pw::properties::properties! {
            "application.name" => "Cosmos Audio Mixer engine",
            "application.id" => "cosmos-audio-mixer",
        };
        if let Some(remote) = remote {
            properties.insert("remote.name", remote);
        }
        let core = context.connect_rc(Some(properties))?;
        let generation = observed.borrow().status.generation;
        let state = Rc::downgrade(observed);
        let done_state = state.clone();
        let core_listener = core
            .add_listener_local()
            .done(move |id, sequence| {
                if id == pw::core::PW_ID_CORE
                    && let Some(state) = done_state.upgrade()
                {
                    let mut state = state.borrow_mut();
                    if state.status.generation != generation {
                        return;
                    }
                    state.device_barrier_done(sequence.seq());
                    if state.status.generation == generation
                        && state.status.phase == Phase::Connecting
                    {
                        state.status.phase = Phase::Connected;
                        state.status.last_error = None;
                    }
                }
            })
            .error(move |id, sequence, result, message| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    if state.status.generation != generation {
                        return;
                    }
                    state.device_error(
                        id,
                        sequence,
                        &format!("PipeWire device operation failed ({result}): {message}"),
                    );
                    state.core_error(id, result, message);
                }
            })
            .register();
        let registry = core.get_registry_rc()?;
        let registry_weak = registry.downgrade();
        let bindings: Bindings = Rc::new(RefCell::new(BTreeMap::new()));
        let binding_weak = Rc::downgrade(&bindings);
        let remove_bindings = binding_weak.clone();
        let state = Rc::downgrade(observed);
        let remove_state = state.clone();
        let registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                let (Some(registry), Some(bindings), Some(state)) = (
                    registry_weak.upgrade(),
                    binding_weak.upgrade(),
                    state.upgrade(),
                ) else {
                    return;
                };
                if state.borrow().status.generation != generation {
                    return;
                }
                let id = global.id;
                let binding = match global.type_ {
                    ObjectType::Node => {
                        let Some(properties) = global.props else {
                            return;
                        };
                        let class = properties.get("media.class").unwrap_or("");
                        if !class.starts_with("Audio/Source")
                            && !class.starts_with("Audio/Sink")
                            && class != "Stream/Output/Audio"
                            && class != "Stream/Input/Audio"
                            && class != crate::meter::MEDIA_CLASS
                        {
                            return;
                        }
                        {
                            let mut state = state.borrow_mut();
                            if state.nodes.len() >= MAX_NODES && !state.nodes.contains_key(&id) {
                                state.status.discovery_error =
                                    Some(format!("discovery limited to {MAX_NODES} audio nodes"));
                                return;
                            }
                            let mut record = Record::default();
                            if update(&mut record.properties, properties) {
                                state.status.discovery_error =
                                    Some("oversized PipeWire property ignored".into());
                            }
                            state.nodes.insert(id, record);
                            state.graph_revision = state.graph_revision.wrapping_add(1);
                        }
                        let node: pw::node::Node = match registry.bind(global) {
                            Ok(node) => node,
                            Err(error) => {
                                state.borrow_mut().status.discovery_error =
                                    Some(format!("cannot inspect node {id}: {error}"));
                                return;
                            }
                        };
                        let info_state = Rc::downgrade(&state);
                        let param_state = info_state.clone();
                        let info_bindings = Rc::downgrade(&bindings);
                        let listener = node
                            .add_listener_local()
                            .info(move |info| {
                                let Some(state) = info_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                                let Some(record) = state.nodes.get_mut(&id) else {
                                    return;
                                };
                                let previous = record.readable_params();
                                for parameter in info.params() {
                                    record.update_param_flags(parameter.id(), parameter.flags());
                                }
                                let readable = record.readable_params();
                                if let Some(properties) = info.props()
                                    && update(&mut record.properties, properties)
                                {
                                    state.status.discovery_error =
                                        Some("oversized PipeWire property ignored".into());
                                }
                                drop(state);
                                // Subscribing also enumerates immediately. An unconfigured
                                // node can return EIO for Format until READ is advertised.
                                if readable != previous
                                    && let Some(bindings) = info_bindings.upgrade()
                                {
                                    let bindings = bindings.borrow();
                                    if let Some(Binding {
                                        object: BoundObject::Node(node),
                                        ..
                                    }) = bindings.get(&id)
                                    {
                                        node.subscribe_params(readable);
                                    }
                                }
                            })
                            .param(move |_sequence, parameter_type, index, _next, parameter| {
                                let Some(state) = param_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                                let Some(record) = state.nodes.get_mut(&id) else {
                                    return;
                                };
                                if !record.readable_params().contains(&parameter_type) {
                                    // A reply queued before suspension must not restore stale layout.
                                    return;
                                }
                                let layout = parameter.and_then(parameter_layout);
                                if parameter_type == ParamType::Format {
                                    record.current_layout = layout;
                                } else if parameter_type == ParamType::EnumFormat {
                                    if index == 0 {
                                        record.available_layout = layout;
                                    } else if layout != record.available_layout {
                                        // Several advertised layouts are not an active mono/stereo configuration.
                                        record.available_layout = Some(Layout::new(None, vec![]));
                                    }
                                }
                            })
                            .register();
                        Binding {
                            _listener: Box::new(listener),
                            object: BoundObject::Node(node),
                        }
                    }
                    ObjectType::Device => {
                        {
                            let mut state = state.borrow_mut();
                            if state.devices.len() >= MAX_DEVICES
                                && !state.devices.contains_key(&id)
                            {
                                state.status.discovery_error =
                                    Some(format!("discovery limited to {MAX_DEVICES} devices"));
                                return;
                            }
                            let mut record = DeviceRecord {
                                permissions: Some(global.permissions),
                                ..DeviceRecord::default()
                            };
                            if let Some(dictionary) = global.props {
                                update(&mut record.properties, dictionary);
                            }
                            state.devices.insert(id, record);
                        }
                        let device: pw::device::Device = match registry.bind(global) {
                            Ok(device) => device,
                            Err(error) => {
                                let message = format!("cannot inspect device {id}: {error}");
                                let mut state = state.borrow_mut();
                                state.status.discovery_error = Some(message.clone());
                                if let Some(record) = state.devices.get_mut(&id) {
                                    record.error = Some(message);
                                }
                                return;
                            }
                        };
                        let info_state = Rc::downgrade(&state);
                        let param_state = info_state.clone();
                        let error_state = info_state.clone();
                        let info_bindings = Rc::downgrade(&bindings);
                        let proxy_id = device.upcast_ref().id();
                        let proxy_listener = device
                            .upcast_ref()
                            .add_listener_local()
                            .error(move |sequence, result, message| {
                                if let Some(state) = error_state.upgrade() {
                                    let mut state = state.borrow_mut();
                                    if state.status.generation != generation {
                                        return;
                                    }
                                    let error =
                                        format!("native device {id} failed ({result}): {message}");
                                    state.device_error(proxy_id, sequence, &error);
                                    if let Some(record) = state.devices.get_mut(&id) {
                                        record.error = Some(error.chars().take(512).collect());
                                    }
                                }
                            })
                            .register();
                        let listener = device
                            .add_listener_local()
                            .info(move |info| {
                                let Some(state) = info_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                                let Some(record) = state.devices.get_mut(&id) else {
                                    return;
                                };
                                let previous = record.readable_params();
                                for parameter in info.params() {
                                    record.update_flags(parameter.id(), parameter.flags());
                                }
                                let readable = record.readable_params();
                                if let Some(dictionary) = info.props()
                                    && update(&mut record.properties, dictionary)
                                {
                                    record.error =
                                        Some("oversized PipeWire device property ignored".into());
                                }
                                drop(state);
                                if previous != readable
                                    && let Some(bindings) = info_bindings.upgrade()
                                {
                                    let bindings = bindings.borrow();
                                    if let Some(Binding {
                                        object: BoundObject::Device(device),
                                        ..
                                    }) = bindings.get(&id)
                                    {
                                        device.subscribe_params(readable);
                                    }
                                }
                            })
                            .param(move |sequence, parameter_type, index, _next, parameter| {
                                let Some(state) = param_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                if let Some(record) = state.devices.get_mut(&id) {
                                    record.parameter(parameter_type, index, parameter);
                                }
                                if parameter_type == ParamType::Profile {
                                    state.device_profile_seen(id, sequence);
                                }
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                            })
                            .register();
                        Binding {
                            _listener: Box::new(devices::DeviceListeners {
                                _device: listener,
                                _proxy: proxy_listener,
                            }),
                            object: BoundObject::Device(device),
                        }
                    }
                    ObjectType::Port => {
                        let Some(properties) = global.props else {
                            return;
                        };
                        let Some(node_id) =
                            properties.get("node.id").and_then(|id| id.parse().ok())
                        else {
                            return;
                        };
                        {
                            let mut state = state.borrow_mut();
                            if state.ports.len() >= MAX_PORTS {
                                state.status.discovery_error =
                                    Some(format!("discovery limited to {MAX_PORTS} ports"));
                                return;
                            }
                            state.ports.insert(
                                id,
                                Port {
                                    id,
                                    node_id,
                                    output: properties.get("port.direction") == Some("out"),
                                    channel: properties
                                        .get("audio.channel")
                                        .unwrap_or("")
                                        .to_owned(),
                                    monitor: properties.get("port.monitor") == Some("true"),
                                },
                            );
                            state.graph_revision = state.graph_revision.wrapping_add(1);
                        }
                        let port: pw::port::Port = match registry.bind(global) {
                            Ok(port) => port,
                            Err(error) => {
                                state.borrow_mut().status.discovery_error =
                                    Some(format!("cannot inspect port {id}: {error}"));
                                return;
                            }
                        };
                        let info_state = Rc::downgrade(&state);
                        let listener = port
                            .add_listener_local()
                            .info(move |info| {
                                let Some(state) = info_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                let Some(port) = state.ports.get_mut(&id) else {
                                    return;
                                };
                                if let Some(properties) = info.props() {
                                    if let Some(channel) = properties.get("audio.channel") {
                                        port.channel = channel.chars().take(32).collect();
                                    }
                                    port.monitor = properties.get("port.monitor") == Some("true");
                                }
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                            })
                            .register();
                        Binding {
                            _listener: Box::new(listener),
                            object: BoundObject::Other {
                                _proxy: Box::new(port),
                            },
                        }
                    }
                    ObjectType::Link => {
                        if state.borrow().links.len() >= MAX_LINKS {
                            return;
                        }
                        let link: pw::link::Link = match registry.bind(global) {
                            Ok(link) => link,
                            Err(_) => return,
                        };
                        let info_state = Rc::downgrade(&state);
                        let listener = link
                            .add_listener_local()
                            .info(move |info| {
                                let Some(state) = info_state.upgrade() else {
                                    return;
                                };
                                let mut state = state.borrow_mut();
                                if state.status.generation != generation {
                                    return;
                                }
                                let owned = info.props().map_or_else(
                                    || state.links.get(&id).is_some_and(|link| link.owned),
                                    |props| props.get("cosmos.owner") == Some("cosmos-audio-mixer"),
                                );
                                state.links.insert(
                                    id,
                                    GraphLink {
                                        output_port: info.output_port_id(),
                                        input_port: info.input_port_id(),
                                        owned,
                                    },
                                );
                                state.graph_revision = state.graph_revision.wrapping_add(1);
                            })
                            .register();
                        Binding {
                            _listener: Box::new(listener),
                            object: BoundObject::Other {
                                _proxy: Box::new(link),
                            },
                        }
                    }
                    ObjectType::Metadata => {
                        if global.props.is_some_and(|props| {
                            props.get("metadata.name") == Some("default")
                                && props.get("client.id").is_some()
                        }) {
                            let mut state = state.borrow_mut();
                            state.policy_id = Some(id);
                            state.policy_revision = state.policy_revision.wrapping_add(1);
                            state.graph_revision = state.graph_revision.wrapping_add(1);
                        }
                        return;
                    }
                    _ => return,
                };
                bindings.borrow_mut().insert(id, binding);
            })
            .global_remove(move |id| {
                if let Some(bindings) = remove_bindings.upgrade() {
                    bindings.borrow_mut().remove(&id);
                }
                if let Some(state) = remove_state.upgrade() {
                    let mut state = state.borrow_mut();
                    if state.status.generation == generation {
                        if state.policy_id == Some(id) {
                            state.policy_id = None;
                            state.policy_revision = state.policy_revision.wrapping_add(1);
                        }
                        state.nodes.remove(&id);
                        state.device_removed(id);
                        state.devices.remove(&id);
                        state.ports.remove(&id);
                        state.links.remove(&id);
                        state.graph_revision = state.graph_revision.wrapping_add(1);
                    }
                }
            })
            .register();
        core.sync(0)?;
        Ok(Self {
            _registry_listener: registry_listener,
            _core_listener: core_listener,
            _bindings: bindings,
            _registry: registry,
            _core: core,
            context,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_clears_ids_and_device_properties() {
        let mut state = Observed::new();
        state.nodes.insert(17, Record::default());
        state.devices.insert(2, DeviceRecord::default());
        state.reset();
        state.status.phase = Phase::Connected;
        assert!(state.snapshot().is_empty());
        assert!(state.devices.is_empty());
    }

    #[test]
    fn unreadable_format_exposes_new_unsupported_layout_instead_of_stale_stereo() {
        let mut state = Observed::new();
        state.status.phase = Phase::Connected;
        let mut record = Record {
            current_layout: Some(Layout::new(Some(2), vec!["FL".into(), "FR".into()])),
            ..Record::default()
        };
        record.properties.insert("audio.channels", "6".into());
        record
            .properties
            .insert("audio.position", "[ FL FR FC LFE RL RR ]".into());
        record.update_param_flags(ParamType::EnumFormat, ParamInfoFlags::WRITE);
        record.update_param_flags(ParamType::Format, ParamInfoFlags::READ);
        state.nodes.insert(17, record);
        assert_eq!(state.snapshot()[0].layout.kind, LayoutKind::Stereo);

        state
            .nodes
            .get_mut(&17)
            .unwrap()
            .update_param_flags(ParamType::Format, ParamInfoFlags::WRITE);
        let nodes = state.snapshot();
        assert_eq!(nodes[0].layout.channels, Some(6));
        assert_eq!(nodes[0].layout.kind, LayoutKind::Unsupported);
    }

    #[test]
    fn duplicate_descriptions_keep_distinct_identity_candidates() {
        let mut state = Observed::new();
        state.status.phase = Phase::Connected;
        for (id, name) in [(1, "test.a"), (2, "test.b")] {
            let mut record = Record::default();
            record.properties.insert("node.name", name.into());
            record
                .properties
                .insert("node.description", "same description".into());
            state.nodes.insert(id, record);
        }
        let nodes = state.snapshot();
        assert_ne!(nodes[0].identity, nodes[1].identity);
    }
    #[test]
    fn resolved_aliases_cannot_close_a_bus_feedback_cycle() {
        use cosmos_control::{Bus, Channels, NodeRef, Profile, Route};
        let mut state = Observed::new();
        state.status.phase = Phase::Connected;
        let mut record = Record::default();
        record.properties.insert("node.name", "fixture".into());
        record.properties.insert("object.path", "/fixture".into());
        state.nodes.insert(7, record);
        let bus = NodeRef::Bus { id: "test".into() };
        let profile = Profile {
            buses: vec![Bus {
                id: "test".into(),
                name: "Test".into(),
                channels: Channels::Mono,
                enabled: true,
                hidden: false,
                muted: false,
                gain_db: 0.0,
                effects: vec![],
            }],
            routes: vec![
                Route {
                    id: "in".into(),
                    from: NodeRef::Hardware {
                        identity: BTreeMap::from([("node.name".into(), "fixture".into())]),
                    },
                    to: bus.clone(),
                    enabled: true,
                },
                Route {
                    id: "out".into(),
                    from: bus,
                    to: NodeRef::Hardware {
                        identity: BTreeMap::from([("object.path".into(), "/fixture".into())]),
                    },
                    enabled: true,
                },
            ],
            ..Profile::default()
        };
        assert!(
            crate::runtime::validate_observed_profile(&profile, &state)
                .unwrap_err()
                .contains("cycle")
        );
    }

    #[test]
    fn subscriptions_follow_format_readability_through_negotiation_and_suspend() {
        let mut record = Record::default();
        record.update_param_flags(ParamType::Format, ParamInfoFlags::WRITE);
        record.update_param_flags(ParamType::EnumFormat, ParamInfoFlags::READ);
        assert_eq!(record.readable_params(), &[ParamType::EnumFormat]);

        record.update_param_flags(
            ParamType::Format,
            ParamInfoFlags::READ | ParamInfoFlags::WRITE,
        );
        assert_eq!(
            record.readable_params(),
            &[ParamType::Format, ParamType::EnumFormat]
        );
        record.current_layout = Some(Layout::new(Some(2), vec!["FL".into(), "FR".into()]));
        record.available_layout = record.current_layout.clone();

        record.update_param_flags(ParamType::Format, ParamInfoFlags::WRITE);
        assert_eq!(record.readable_params(), &[ParamType::EnumFormat]);
        assert!(record.current_layout.is_none());
        record.update_param_flags(ParamType::EnumFormat, ParamInfoFlags::empty());
        assert!(record.readable_params().is_empty());
        assert!(record.available_layout.is_none());

        record.update_param_flags(ParamType::Format, ParamInfoFlags::READ);
        assert_eq!(record.readable_params(), &[ParamType::Format]);
    }

    #[test]
    fn missing_object_parameters_do_not_poison_discovery_or_clear_real_errors() {
        let mut state = Observed::new();
        state.status.phase = Phase::Connected;
        state.core_error(7, -libc::ENOENT, "format not negotiated");
        assert!(state.status.discovery_error.is_none());
        assert!(state.connection_error.is_none());
        assert_eq!(state.status.phase, Phase::Connected);

        for result in [-libc::EACCES, -libc::EIO] {
            state.core_error(7, result, "parameter query failed");
            let error = state.status.discovery_error.clone().unwrap();
            state.core_error(8, -libc::ENOENT, "resource removed");
            assert_eq!(
                state.status.discovery_error.as_deref(),
                Some(error.as_str())
            );
            assert!(state.connection_error.is_none());
        }
    }

    #[test]
    fn raced_resource_errors_preserve_the_live_generation_and_request_reconcile() {
        let mut state = Observed::new();
        state.status.phase = Phase::Connected;
        state.status.generation = 8;
        state.nodes.insert(7, Record::default());
        let revision = state.graph_revision;
        state.core_error(
            pw::core::PW_ID_CORE,
            -libc::ENOENT,
            "raced resource removal",
        );
        assert!(state.connection_error.is_none());
        assert_eq!(state.status.phase, Phase::Connected);
        assert_eq!(state.status.generation, 8);
        assert_eq!(state.snapshot().len(), 1);
        assert_ne!(state.graph_revision, revision);
        assert!(state.status.discovery_error.is_none());
        state.core_error(7, -libc::EPIPE, "individual resource failed");
        assert!(state.connection_error.is_none());
        state.core_error(pw::core::PW_ID_CORE, -libc::EPIPE, "transport closed");
        assert!(state.connection_error.is_some());
    }
}
