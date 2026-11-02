// SPDX-License-Identifier: MPL-2.0

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::Cursor;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cosmos_control::{
    BackendStatus, Bus, BusStatus, Channels, Effect, LayoutKind, Node, NodeRef, Phase, Profile,
    RouteStatus, RuntimeState,
};
use pipewire as pw;
use pw::proxy::ProxyT;
use pw::spa::pod::{Object, Pod, Property, Value};
use serde_json::{Value as Json, json};

use crate::discovery::{Connection, Observed};
use crate::meter::Meter;

const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);
const OWNER: &str = "cosmos-audio-mixer";

type Controls = Vec<(String, f32)>;
type PortPairs = Vec<(u32, u32)>;
type Failure = (RuntimeState, String);

struct ModuleState {
    module: Cell<*mut pw::sys::pw_impl_module>,
    hook: pw::spa::sys::spa_hook,
    dirty: Rc<Cell<bool>>,
}

unsafe extern "C" fn module_destroyed(data: *mut std::ffi::c_void) {
    // SAFETY: NativeModule keeps this box at a stable address until its hook is removed.
    // PipeWire invokes module events on the same main loop as reconciliation and Drop.
    let state = unsafe { &mut *data.cast::<ModuleState>() };
    state.module.set(std::ptr::null_mut());
    state.dirty.set(true);
    pw::spa::utils::hook::remove(state.hook);
}

static MODULE_EVENTS: pw::sys::pw_impl_module_events = pw::sys::pw_impl_module_events {
    version: 0,
    destroy: Some(module_destroyed),
    free: None,
    initialized: None,
    registered: None,
};

struct NativeModule {
    state: Box<ModuleState>,
    // The module is local to this engine-owned context, never the desktop daemon.
    _context: pw::context::ContextRc,
}

impl NativeModule {
    fn load(
        context: &pw::context::ContextRc,
        arguments: &Json,
        dirty: &Rc<Cell<bool>>,
    ) -> Result<Self, String> {
        let arguments = CString::new(arguments.to_string()).map_err(|error| error.to_string())?;
        // SAFETY: the reference-counted context outlives the module. PipeWire parses
        // arguments synchronously; ownership of the null properties pointer is empty.
        let module = unsafe {
            pw::sys::pw_context_load_module(
                context.as_raw_ptr(),
                c"libpipewire-module-filter-chain".as_ptr(),
                arguments.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        if module.is_null() {
            return Err(format!(
                "cannot load required native filter graph (LSP LV2 plugin/host unavailable): {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: spa_hook is an intrusive C hook initialized by add_listener. It
        // contains no Rust references. Its box and callback data do not move.
        let mut state = Box::new(ModuleState {
            module: Cell::new(module),
            hook: unsafe { std::mem::zeroed() },
            dirty: dirty.clone(),
        });
        unsafe {
            pw::sys::pw_impl_module_add_listener(
                module,
                &mut state.hook,
                &MODULE_EVENTS,
                (&mut *state as *mut ModuleState).cast(),
            );
        }
        Ok(Self {
            state,
            _context: context.clone(),
        })
    }

    fn alive(&self) -> bool {
        !self.state.module.get().is_null()
    }
}

impl Drop for NativeModule {
    fn drop(&mut self) {
        let module = self.state.module.replace(std::ptr::null_mut());
        if !module.is_null() {
            // Unregister our callback before destroying the native object. A module
            // that self-destructed already cleared its pointer and removed its hook.
            pw::spa::utils::hook::remove(self.state.hook);
            // SAFETY: this is the sole owning handle; context is still alive and
            // all access is on the PipeWire main loop, outside sample callbacks.
            unsafe {
                pw::sys::pw_impl_module_destroy(module);
            }
        }
    }
}

#[derive(Default)]
struct LinkFeedback {
    ready: bool,
    error: Option<String>,
}

struct OwnedLink {
    _listener: pw::link::LinkListener,
    _proxy_listener: pw::proxy::ProxyListener,
    _link: pw::link::Link,
    _core: pw::core::CoreRc,
    feedback: Rc<RefCell<LinkFeedback>>,
}

impl OwnedLink {
    fn new(
        connection: &Connection,
        output: u32,
        input: u32,
        dirty: &Rc<Cell<bool>>,
    ) -> Result<Self, String> {
        let properties = pw::properties::properties! {
            "link.output.port" => output.to_string(),
            "link.input.port" => input.to_string(),
            "object.linger" => "false",
            "cosmos.owner" => OWNER,
        };
        let link = connection
            .core()
            .create_object::<pw::link::Link>("link-factory", &properties)
            .map_err(|error| error.to_string())?;
        let feedback = Rc::new(RefCell::new(LinkFeedback::default()));
        let state = feedback.clone();
        let info_dirty = dirty.clone();
        let listener = link
            .add_listener_local()
            .info(move |info| {
                info_dirty.set(true);
                let mut state = state.borrow_mut();
                match info.state() {
                    pw::link::LinkState::Paused | pw::link::LinkState::Active => {
                        state.ready = true;
                        state.error = None;
                    }
                    pw::link::LinkState::Error(error) => {
                        state.ready = false;
                        state.error = Some(error.chars().take(512).collect());
                    }
                    pw::link::LinkState::Unlinked => {
                        state.ready = false;
                        state.error = Some("link was removed".into());
                    }
                    _ => {
                        state.ready = false;
                    }
                }
            })
            .register();
        let state = feedback.clone();
        let removed = feedback.clone();
        let error_dirty = dirty.clone();
        let remove_dirty = dirty.clone();
        let proxy_listener = link
            .upcast_ref()
            .add_listener_local()
            .error(move |_, result, error| {
                error_dirty.set(true);
                let mut state = state.borrow_mut();
                state.ready = false;
                state.error = Some(format!(
                    "link failed ({result}): {}",
                    error.chars().take(512).collect::<String>()
                ));
            })
            .removed(move || {
                remove_dirty.set(true);
                let mut state = removed.borrow_mut();
                state.ready = false;
                state.error = Some("link was removed".into());
            })
            .register();
        Ok(Self {
            _listener: listener,
            _proxy_listener: proxy_listener,
            _link: link,
            _core: connection.core().clone(),
            feedback,
        })
    }
}

struct OwnedRoute {
    pairs: PortPairs,
    links: Vec<OwnedLink>,
    created_at: Instant,
}

impl OwnedRoute {
    fn new(
        connection: &Connection,
        pairs: PortPairs,
        dirty: &Rc<Cell<bool>>,
    ) -> Result<Self, String> {
        let links = pairs
            .iter()
            .map(|&(output, input)| OwnedLink::new(connection, output, input, dirty))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            pairs,
            links,
            created_at: Instant::now(),
        })
    }

    fn state(&self) -> (RuntimeState, Option<String>) {
        for link in &self.links {
            let feedback = link.feedback.borrow();
            if let Some(error) = &feedback.error {
                return (RuntimeState::Error, Some(error.clone()));
            }
        }
        if self.links.iter().all(|link| link.feedback.borrow().ready) {
            (RuntimeState::Ready, None)
        } else if self.created_at.elapsed() >= Duration::from_secs(2) {
            (
                RuntimeState::Error,
                Some("native link negotiation timed out".into()),
            )
        } else {
            (RuntimeState::Pending, None)
        }
    }
}

struct MeterRoute {
    links: Option<OwnedRoute>,
    pairs: PortPairs,
    retry_at: Instant,
}

impl MeterRoute {
    fn new() -> Self {
        Self {
            links: None,
            pairs: Vec::new(),
            retry_at: Instant::now(),
        }
    }

    fn prepare(&mut self, pairs: PortPairs, now: Instant) -> bool {
        if self.pairs != pairs {
            self.links = None;
            self.pairs = pairs;
            self.retry_at = now;
        }
        if self
            .links
            .as_ref()
            .is_some_and(|route| route.state().0 == RuntimeState::Error)
        {
            self.links = None;
            self.retry_at = now + RETRY_MAX;
        }
        now >= self.retry_at
    }

    fn clear(&mut self) {
        self.links = None;
        self.pairs.clear();
    }
}

struct BusMeters {
    input_links: MeterRoute,
    output_links: MeterRoute,
    input: Meter,
    output: Meter,
}

struct HostedBus {
    meters: Option<BusMeters>,
    module: Option<NativeModule>,
    topology: Vec<String>,
    applied: Controls,
    control_node: Option<u32>,
    error: Option<String>,
    retry_at: Instant,
    retry_delay: Duration,
    started_at: Instant,
}

impl HostedBus {
    fn new(bus: &Bus) -> Self {
        Self {
            meters: None,
            module: None,
            topology: topology(bus),
            applied: Vec::new(),
            control_node: None,
            error: None,
            retry_at: Instant::now(),
            retry_delay: RETRY_MIN,
            started_at: Instant::now(),
        }
    }

    fn failed(&mut self, error: String) {
        self.error = Some(error);
        self.retry_at = Instant::now() + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    fn controls_applied(&mut self, node: u32, controls: Controls) {
        self.applied = controls;
        self.control_node = Some(node);
    }

    fn published(&mut self) {
        if self.control_node.is_some() {
            self.retry_delay = RETRY_MIN;
        }
    }
}

fn touches_retired(
    pairs: &[(u32, u32)],
    observed: &Observed,
    retired: &std::collections::BTreeSet<u32>,
) -> bool {
    pairs.iter().any(|(output, input)| {
        [output, input].iter().any(|id| {
            observed
                .ports
                .get(id)
                .is_some_and(|port| retired.contains(&port.node_id))
        })
    })
}

pub struct Runtime {
    routes: BTreeMap<String, OwnedRoute>,
    buses: BTreeMap<String, HostedBus>,
    route_retries: BTreeMap<String, Instant>,
    retired_nodes: std::collections::BTreeSet<u32>,
    remote: Option<String>,
    generation: u64,
    profile: Option<Profile>,
    revision: u64,
    refresh_at: Option<Instant>,
    dirty: Rc<Cell<bool>>,
    status: BackendStatus,
}

impl Runtime {
    pub fn new(_main_loop: &pw::main_loop::MainLoopRc, remote: Option<&str>) -> Self {
        Self {
            routes: BTreeMap::new(),
            buses: BTreeMap::new(),
            route_retries: BTreeMap::new(),
            retired_nodes: std::collections::BTreeSet::new(),
            remote: remote.map(str::to_owned),
            generation: 0,
            profile: None,
            revision: 0,
            refresh_at: None,
            dirty: Rc::new(Cell::new(true)),
            status: BackendStatus::default(),
        }
    }

    pub fn shutdown(&mut self) {
        self.routes.clear();
        self.buses.clear();
        self.route_retries.clear();
        self.retired_nodes.clear();
        self.profile = None;
        self.dirty.set(true);
    }

    pub fn reconcile(
        &mut self,
        profile: &Profile,
        connection: Option<&Connection>,
        observed: &Observed,
    ) -> BackendStatus {
        if observed.status.generation != self.generation
            || observed.status.phase != Phase::Connected
        {
            self.shutdown();
            self.generation = observed.status.generation;
        }
        let Some(connection) = connection.filter(|_| observed.status.phase == Phase::Connected)
        else {
            return BackendStatus {
                ready: false,
                error: Some("PipeWire is unavailable".into()),
                buses: profile
                    .buses
                    .iter()
                    .map(|bus| BusStatus {
                        id: bus.id.clone(),
                        state: if bus.enabled {
                            RuntimeState::Unavailable
                        } else {
                            RuntimeState::Disabled
                        },
                        error: bus.enabled.then(|| "PipeWire is unavailable".into()),
                        input_meter: None,
                        output_meter: None,
                    })
                    .collect(),
                routes: profile
                    .routes
                    .iter()
                    .map(|route| RouteStatus {
                        id: route.id.clone(),
                        state: if route.enabled {
                            RuntimeState::Unavailable
                        } else {
                            RuntimeState::Disabled
                        },
                        error: route.enabled.then(|| "PipeWire is unavailable".into()),
                    })
                    .collect(),
            };
        };
        let changed = self.profile.as_ref() != Some(profile);
        let dirty = self.dirty.replace(false);
        if !changed
            && !dirty
            && self.revision == observed.graph_revision
            && self
                .refresh_at
                .is_none_or(|deadline| Instant::now() < deadline)
        {
            self.update_meter_status();
            return self.status.clone();
        }
        if changed && let Err(error) = profile.validate() {
            self.shutdown();
            return BackendStatus {
                error: Some(error.to_string()),
                ..BackendStatus::default()
            };
        }
        if changed {
            self.profile = Some(profile.clone());
        }
        self.revision = observed.graph_revision;
        self.refresh_at = None;
        let mut nodes = observed.snapshot();
        self.retired_nodes
            .retain(|id| nodes.iter().any(|node| node.id == *id));
        let retiring = if changed {
            self.buses
                .iter()
                .filter_map(|(id, host)| {
                    profile
                        .buses
                        .iter()
                        .find(|bus| bus.id == *id && bus.enabled)
                        .is_none_or(|bus| host.topology != topology(bus))
                        .then_some(id.clone())
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for id in &retiring {
            for node in nodes.iter().filter(|node| observed.is_owned(node.id)) {
                if ["input", "output", "meter-input", "meter-output"]
                    .iter()
                    .any(|suffix| node.name == format!("cosmos.bus.{id}.{suffix}"))
                {
                    self.retired_nodes.insert(node.id);
                }
            }
        }
        self.routes
            .retain(|_, route| !touches_retired(&route.pairs, observed, &self.retired_nodes));
        if !retiring.is_empty() {
            for (id, host) in &mut self.buses {
                if let Some(meters) = &mut host.meters {
                    for route in [&mut meters.input_links, &mut meters.output_links] {
                        if retiring.contains(id)
                            || touches_retired(&route.pairs, observed, &self.retired_nodes)
                        {
                            route.clear();
                        }
                    }
                }
            }
            // Drop owned link proxies before destroying any endpoint they use.
            // Concurrent remote removal remains a recoverable native graph error.
            for id in retiring {
                self.buses.remove(&id);
            }
        }
        nodes.retain(|node| !self.retired_nodes.contains(&node.id));
        self.routes.retain(|id, _| {
            profile
                .routes
                .iter()
                .any(|route| route.enabled && route.id == *id)
        });
        self.route_retries.retain(|id, _| {
            profile
                .routes
                .iter()
                .any(|route| route.enabled && route.id == *id)
        });
        self.buses
            .retain(|id, _| profile.buses.iter().any(|bus| bus.enabled && bus.id == *id));
        for bus in profile.buses.iter().filter(|bus| bus.enabled) {
            let host = self
                .buses
                .entry(bus.id.clone())
                .or_insert_with(|| HostedBus::new(bus));
            if host.module.as_ref().is_some_and(|module| !module.alive()) {
                host.meters = None;
                host.module = None;
                host.applied.clear();
                host.control_node = None;
                host.failed("native filter graph disconnected; waiting to recover".into());
            }
            if host.module.is_some()
                && host.started_at.elapsed() >= Duration::from_secs(2)
                && (host.control_node.is_none() || !bus_ports_ready(bus, &nodes, observed))
            {
                host.meters = None;
                host.module = None;
                host.applied.clear();
                host.control_node = None;
                host.failed("native bus endpoint publication timed out".into());
            }
            if host.module.is_none() && (changed || Instant::now() >= host.retry_at) {
                let audible = !bus.muted
                    && !profile
                        .routes
                        .iter()
                        .any(|route| route.enabled && is_bus(&route.to, &bus.id));
                match NativeModule::load(
                    connection.context(),
                    &module_arguments(bus, self.remote.as_deref(), audible),
                    &self.dirty,
                ) {
                    Ok(module) => {
                        host.module = Some(module);
                        host.error = None;
                        host.applied.clear();
                        host.started_at = Instant::now();
                    }
                    Err(error) => host.failed(error),
                }
            }
        }
        let mut bus_status = profile
            .buses
            .iter()
            .map(|bus| {
                let (state, error) = if !bus.enabled {
                    (RuntimeState::Disabled, None)
                } else if let Some(error) =
                    self.buses.get(&bus.id).and_then(|host| host.error.clone())
                {
                    (RuntimeState::Error, Some(error))
                } else {
                    match (
                        bus_node(bus, false, &nodes, observed),
                        bus_node(bus, true, &nodes, observed),
                    ) {
                        (Ok(input), Ok(output)) => match (
                            node_ports(input, false, false, observed),
                            node_ports(output, true, false, observed),
                        ) {
                            (Ok((input, _)), Ok((output, _)))
                                if input == bus.channels && output == bus.channels =>
                            {
                                (RuntimeState::Ready, None)
                            }
                            (Err((state, error)), _) | (_, Err((state, error))) => {
                                (state, Some(error))
                            }
                            _ => (
                                RuntimeState::Error,
                                Some("native bus has an unsupported channel layout".into()),
                            ),
                        },
                        (Err((state, error)), _) | (_, Err((state, error))) => (state, Some(error)),
                    }
                };
                BusStatus {
                    id: bus.id.clone(),
                    state,
                    error,
                    input_meter: None,
                    output_meter: None,
                }
            })
            .collect::<Vec<_>>();
        let graph_errors = resolved_route_errors(profile, observed, &nodes);
        let mut route_status = Vec::with_capacity(profile.routes.len());
        for route in &profile.routes {
            let result = if !route.enabled {
                Err((RuntimeState::Disabled, "route is disabled".into()))
            } else if let Some(error) = graph_errors.get(&route.id) {
                Err((RuntimeState::Error, error.clone()))
            } else {
                resolve(&route.from, true, profile, &nodes, observed).and_then(|source| {
                    resolve(&route.to, false, profile, &nodes, observed)
                        .and_then(|target| pair_ports(source, target))
                })
            };
            let (state, error) = match result {
                Err((state, error)) => {
                    self.routes.remove(&route.id);
                    self.route_retries.remove(&route.id);
                    (state, Some(error))
                }
                Ok(pairs) => {
                    if self
                        .routes
                        .get(&route.id)
                        .is_some_and(|owned| owned.pairs != pairs)
                    {
                        self.routes.remove(&route.id);
                    }
                    let previous_error =
                        self.routes.get(&route.id).and_then(|owned| owned.state().1);
                    if previous_error.is_some() {
                        self.routes.remove(&route.id);
                        self.route_retries
                            .insert(route.id.clone(), Instant::now() + RETRY_MAX);
                    }
                    if !self.routes.contains_key(&route.id)
                        && self
                            .route_retries
                            .get(&route.id)
                            .is_none_or(|retry| changed || Instant::now() >= *retry)
                    {
                        match OwnedRoute::new(connection, pairs, &self.dirty) {
                            Ok(owned) => {
                                self.routes.insert(route.id.clone(), owned);
                                self.route_retries.remove(&route.id);
                            }
                            Err(error) => {
                                self.route_retries
                                    .insert(route.id.clone(), Instant::now() + RETRY_MAX);
                                route_status.push(RouteStatus {
                                    id: route.id.clone(),
                                    state: RuntimeState::Error,
                                    error: Some(error),
                                });
                                continue;
                            }
                        }
                    }
                    self.routes
                        .get(&route.id)
                        .map(OwnedRoute::state)
                        .unwrap_or_else(|| {
                            (
                                RuntimeState::Error,
                                previous_error
                                    .or_else(|| {
                                        self.status
                                            .routes
                                            .iter()
                                            .find(|status| status.id == route.id)
                                            .and_then(|status| status.error.clone())
                                    })
                                    .or_else(|| {
                                        Some("route is waiting for a bounded retry".into())
                                    }),
                            )
                        })
                }
            };
            route_status.push(RouteStatus {
                id: route.id.clone(),
                state,
                error,
            });
        }
        // Acyclic model routes reach a fixed point in at most one pass per bus.
        // Keep outputs closed until an explicit upstream route is actually usable.
        let mut usable = BTreeMap::new();
        for bus in &bus_status {
            let incoming = profile
                .routes
                .iter()
                .any(|route| route.enabled && is_bus(&route.to, &bus.id));
            usable.insert(
                bus.id.clone(),
                bus.state == RuntimeState::Ready && !incoming,
            );
        }
        for _ in 0..profile.buses.len() {
            for bus in &bus_status {
                if bus.state != RuntimeState::Ready {
                    continue;
                }
                if profile
                    .routes
                    .iter()
                    .zip(&route_status)
                    .any(|(route, status)| {
                        route.enabled
                            && is_bus(&route.to, &bus.id)
                            && status.state == RuntimeState::Ready
                            && match &route.from {
                                NodeRef::Bus { id } => usable.get(id) == Some(&true),
                                NodeRef::Hardware { .. } => true,
                            }
                    })
                {
                    usable.insert(bus.id.clone(), true);
                }
            }
        }
        for status in &mut route_status {
            if status.state == RuntimeState::Ready
                && let Some(route) = profile.routes.iter().find(|route| route.id == status.id)
                && let NodeRef::Bus { id } = &route.from
                && usable.get(id) != Some(&true)
            {
                status.state = RuntimeState::Unavailable;
                status.error = Some("source bus has no available input".into());
            }
        }
        for (bus, status) in profile.buses.iter().zip(&mut bus_status) {
            if !bus.enabled {
                continue;
            }
            let audible = usable.get(&bus.id) == Some(&true) && !bus.muted;
            if status.state == RuntimeState::Ready && usable.get(&bus.id) != Some(&true) {
                status.state = RuntimeState::Unavailable;
                status.error =
                    Some("no explicit input route is available; output is silenced".into());
            }
            let Some(host) = self.buses.get_mut(&bus.id) else {
                continue;
            };
            if host.module.is_none() {
                continue;
            }
            if let Ok(node) = bus_node(bus, false, &nodes, observed) {
                let controls = controls(bus, audible);
                if host.control_node != Some(node.id) || host.applied != controls {
                    match control_pod(&controls) {
                        Ok(bytes) => {
                            if let Some(pod) = Pod::from_bytes(&bytes)
                                && connection.set_node_param(node.id, pod)
                            {
                                host.controls_applied(node.id, controls);
                            }
                        }
                        Err(error) => {
                            status.state = RuntimeState::Error;
                            status.error = Some(error);
                        }
                    }
                }
            }
            if matches!(
                status.state,
                RuntimeState::Ready | RuntimeState::Unavailable
            ) {
                host.published();
            }
            reconcile_meters(host, bus, connection, &nodes, observed, &self.dirty);
        }
        let error = bus_status
            .iter()
            .find_map(|bus| {
                (bus.state == RuntimeState::Error)
                    .then(|| bus.error.clone())
                    .flatten()
            })
            .or_else(|| {
                route_status.iter().find_map(|route| {
                    (route.state == RuntimeState::Error)
                        .then(|| route.error.clone())
                        .flatten()
                })
            });
        let ready = bus_status
            .iter()
            .all(|bus| matches!(bus.state, RuntimeState::Ready | RuntimeState::Disabled))
            && route_status
                .iter()
                .all(|route| matches!(route.state, RuntimeState::Ready | RuntimeState::Disabled));
        self.status = BackendStatus {
            ready,
            error,
            buses: bus_status,
            routes: route_status,
        };
        for host in self.buses.values() {
            let deadline = if host.module.is_none() {
                Some(host.retry_at)
            } else if host.control_node.is_none() {
                Some(host.started_at + Duration::from_secs(2))
            } else if host.meters.is_none() {
                Some(host.retry_at)
            } else {
                None
            };
            if let Some(deadline) = deadline {
                schedule(&mut self.refresh_at, deadline);
            }
            if let Some(meters) = &host.meters {
                for route in [&meters.input_links, &meters.output_links] {
                    if let Some(links) = &route.links {
                        if links.state().0 == RuntimeState::Pending {
                            schedule(
                                &mut self.refresh_at,
                                links.created_at + Duration::from_secs(2),
                            );
                        }
                    } else if !route.pairs.is_empty() {
                        schedule(&mut self.refresh_at, route.retry_at);
                    }
                }
            }
        }
        if let Some(deadline) = next_route_retry(&self.route_retries, Instant::now()) {
            schedule(&mut self.refresh_at, deadline);
        }
        for route in self.routes.values() {
            if route.state().0 == RuntimeState::Pending {
                schedule(
                    &mut self.refresh_at,
                    route.created_at + Duration::from_secs(2),
                );
            }
        }
        self.update_meter_status();
        self.status.clone()
    }

    fn update_meter_status(&mut self) {
        for status in &mut self.status.buses {
            if let Some(meters) = self
                .buses
                .get(&status.id)
                .and_then(|host| host.meters.as_ref())
            {
                status.input_meter = Some(meters.input.snapshot());
                status.output_meter = Some(meters.output.snapshot());
                if status
                    .input_meter
                    .as_ref()
                    .and_then(|meter| meter.error.as_ref())
                    .is_some()
                    || status
                        .output_meter
                        .as_ref()
                        .and_then(|meter| meter.error.as_ref())
                        .is_some()
                {
                    schedule(&mut self.refresh_at, Instant::now() + RETRY_MAX);
                }
            }
        }
    }
}

fn schedule(deadline: &mut Option<Instant>, next: Instant) {
    *deadline = Some(deadline.map_or(next, |current| current.min(next)));
}

fn next_route_retry(retries: &BTreeMap<String, Instant>, now: Instant) -> Option<Instant> {
    retries
        .values()
        .copied()
        .filter(|deadline| *deadline > now)
        .min()
}

/// Validate aliases against the current generation before persisting or linking.
/// Unavailable/ambiguous selectors remain explicit runtime states, never guesses.
pub fn validate_observed_profile(profile: &Profile, observed: &Observed) -> Result<(), String> {
    resolved_route_errors(profile, observed, &observed.snapshot())
        .into_values()
        .next()
        .map_or(Ok(()), Err)
}

fn bus_ports_ready(bus: &Bus, nodes: &[Node], observed: &Observed) -> bool {
    [false, true].into_iter().all(|output| {
        bus_node(bus, output, nodes, observed)
            .and_then(|node| node_ports(node, output, false, observed))
            .is_ok_and(|(channels, _)| channels == bus.channels)
    })
}

fn resolved_route_errors(
    profile: &Profile,
    observed: &Observed,
    nodes: &[Node],
) -> BTreeMap<String, String> {
    let mut buses = BTreeMap::new();
    let mut graph: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (index, bus) in profile
        .buses
        .iter()
        .enumerate()
        .filter(|(_, bus)| bus.enabled)
    {
        let input = bus_node(bus, false, nodes, observed)
            .map_or(u64::MAX - (index as u64 * 2), |node| u64::from(node.id));
        let output = bus_node(bus, true, nodes, observed)
            .map_or(u64::MAX - (index as u64 * 2 + 1), |node| u64::from(node.id));
        buses.insert(bus.id.as_str(), (input, output));
        graph.entry(input).or_default().push(output);
    }
    for link in observed.links.values().filter(|link| !link.owned) {
        if let (Some(output), Some(input)) = (
            observed.ports.get(&link.output_port),
            observed.ports.get(&link.input_port),
        ) {
            graph
                .entry(u64::from(output.node_id))
                .or_default()
                .push(u64::from(input.node_id));
        }
    }
    let endpoint = |reference: &NodeRef, output: bool| -> Option<u64> {
        match reference {
            NodeRef::Bus { id } => buses
                .get(id.as_str())
                .map(|&(input, source)| if output { source } else { input }),
            NodeRef::Hardware { identity } => {
                let mut matches = nodes
                    .iter()
                    .filter(|node| !observed.is_owned(node.id) && matches_identity(node, identity));
                let node = matches.next()?;
                matches.next().is_none().then_some(u64::from(node.id))
            }
        }
    };
    let mut desired = Vec::new();
    for route in profile.routes.iter().filter(|route| route.enabled) {
        if let (Some(from), Some(to)) = (endpoint(&route.from, true), endpoint(&route.to, false)) {
            desired.push((route.id.clone(), from, to));
            graph.entry(from).or_default().push(to);
        }
    }
    route_graph_errors(&desired, &graph)
}

fn route_graph_errors(
    desired: &[(String, u64, u64)],
    graph: &BTreeMap<u64, Vec<u64>>,
) -> BTreeMap<String, String> {
    let mut errors = BTreeMap::new();
    let mut connections: BTreeMap<(u64, u64), &String> = BTreeMap::new();
    for (id, from, to) in desired {
        if let Some(other) = connections.insert((*from, *to), id) {
            errors.insert(
                other.clone(),
                "different route selectors resolve to the same native connection".into(),
            );
            errors.insert(
                id.clone(),
                "different route selectors resolve to the same native connection".into(),
            );
        }
    }
    for (id, from, to) in desired {
        let mut pending = vec![*to];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(node) = pending.pop() {
            if node == *from {
                errors.entry(id.clone()).or_insert_with(|| {
                    "resolved routing graph contains an audio feedback cycle".into()
                });
                break;
            }
            if visited.insert(node)
                && let Some(next) = graph.get(&node)
            {
                pending.extend(next);
            }
        }
    }
    errors
}

fn is_bus(endpoint: &NodeRef, bus: &str) -> bool {
    matches!(endpoint, NodeRef::Bus { id } if id == bus)
}

fn bus_name(bus: &Bus, output: bool) -> String {
    format!(
        "cosmos.bus.{}.{}",
        bus.id,
        if output { "output" } else { "input" }
    )
}

fn bus_node<'a>(
    bus: &Bus,
    output: bool,
    nodes: &'a [Node],
    observed: &Observed,
) -> Result<&'a Node, Failure> {
    let name = bus_name(bus, output);
    let mut matches = nodes
        .iter()
        .filter(|node| observed.is_owned(node.id) && node.name == name);
    let node = matches.next().ok_or_else(|| {
        (
            RuntimeState::Pending,
            "waiting for native bus endpoint".into(),
        )
    })?;
    if matches.next().is_some() {
        return Err((
            RuntimeState::Error,
            "ambiguous app-owned bus endpoint".into(),
        ));
    }
    Ok(node)
}

fn matches_identity(node: &Node, identity: &BTreeMap<String, String>) -> bool {
    identity
        .iter()
        .all(|(key, value)| node.identity.get(key) == Some(value))
}

fn resolve(
    endpoint: &NodeRef,
    output: bool,
    profile: &Profile,
    nodes: &[Node],
    observed: &Observed,
) -> Result<(Channels, Vec<u32>), Failure> {
    let node = match endpoint {
        NodeRef::Bus { id } => {
            let bus = profile
                .buses
                .iter()
                .find(|bus| bus.id == *id)
                .ok_or_else(|| (RuntimeState::Error, "unknown bus".into()))?;
            if !bus.enabled {
                return Err((RuntimeState::Disabled, "bus is disabled".into()));
            }
            bus_node(bus, output, nodes, observed)?
        }
        NodeRef::Hardware { identity } => {
            let mut matches = nodes
                .iter()
                .filter(|node| !observed.is_owned(node.id) && matches_identity(node, identity));
            let node = matches.next().ok_or_else(|| {
                (
                    RuntimeState::Unavailable,
                    "selected device/application is unavailable; no replacement selected".into(),
                )
            })?;
            if matches.next().is_some() {
                return Err((
                    RuntimeState::Error,
                    "selected identity is ambiguous; use more durable identity properties".into(),
                ));
            }
            if let Some(settings) = profile
                .devices
                .iter()
                .find(|settings| matches_identity(node, &settings.identity))
                && (!settings.enabled || settings.muted)
            {
                return Err((
                    RuntimeState::Disabled,
                    if settings.muted {
                        "device is muted locally"
                    } else {
                        "device participation is disabled locally"
                    }
                    .into(),
                ));
            }
            node
        }
    };
    node_ports(
        node,
        output,
        output && node.media_class.starts_with("Audio/Sink"),
        observed,
    )
}

fn node_ports(
    node: &Node,
    output: bool,
    monitor: bool,
    observed: &Observed,
) -> Result<(Channels, Vec<u32>), Failure> {
    if node.layout.kind == LayoutKind::Unsupported {
        return Err((
            RuntimeState::Error,
            "selected node has an unsupported channel layout".into(),
        ));
    }
    let ports = observed
        .ports
        .values()
        .filter(|port| port.node_id == node.id && port.output == output && port.monitor == monitor)
        .collect::<Vec<_>>();
    if ports.is_empty() {
        return Err((
            RuntimeState::Pending,
            "waiting for native audio ports in the selected direction".into(),
        ));
    }
    if ports.len() == 1
        && (matches!(ports[0].channel.as_str(), "MONO" | "FC")
            || (ports[0].channel.is_empty() && node.layout.kind == LayoutKind::Mono))
    {
        return Ok((Channels::Mono, vec![ports[0].id]));
    }
    if ports.len() == 2 {
        let left = ports.iter().find(|port| port.channel == "FL");
        let right = ports.iter().find(|port| port.channel == "FR");
        if let (Some(left), Some(right)) = (left, right) {
            return Ok((Channels::Stereo, vec![left.id, right.id]));
        }
    }
    Err((
        RuntimeState::Error,
        "native ports do not have supported mono or FL/FR stereo semantics".into(),
    ))
}

fn pair_ports(
    source: (Channels, Vec<u32>),
    target: (Channels, Vec<u32>),
) -> Result<PortPairs, Failure> {
    if source.0 != target.0 {
        return Err((
            RuntimeState::Error,
            "route channel layouts differ; implicit upmix/downmix is disabled".into(),
        ));
    }
    Ok(source.1.into_iter().zip(target.1).collect())
}

fn topology(bus: &Bus) -> Vec<String> {
    let mut result = vec![format!("{:?}:{}", bus.channels, bus.name)];
    result.extend(
        bus.effects
            .iter()
            .map(|effect| format!("{}:{}", effect.id, plugin_name(&effect.effect))),
    );
    result
}

fn plugin_name(effect: &Effect) -> &'static str {
    match effect {
        Effect::Gate { .. } => "gate",
        Effect::Compressor { .. } => "compressor",
        Effect::Equalizer { .. } => "para_equalizer_x8",
        Effect::Limiter { .. } => "limiter",
    }
}

fn linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn effect_controls(effect: &cosmos_control::EffectInstance) -> Controls {
    let mut controls = vec![
        ("enabled".into(), if effect.enabled { 1.0 } else { 0.0 }),
        ("g_in".into(), 1.0),
        ("g_out".into(), 1.0),
    ];
    let specific: &[(&str, f32)] = match effect.effect {
        Effect::Gate {
            threshold_db,
            attack_ms,
            hold_ms,
            release_ms,
        } => &[
            ("sci", 0.0),
            ("scm", 1.0),
            ("scr", 5.0),
            ("gt", linear(threshold_db)),
            ("gz", 0.1),
            ("gh", 0.0),
            ("at", attack_ms),
            ("rt", release_ms),
            ("hold", hold_ms),
            ("gr", 0.0002511886),
            ("mk", 1.0),
            ("cdr", 0.0),
            ("cwt", 1.0),
            ("cdw", 100.0),
        ],
        Effect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => &[
            ("sct", 0.0),
            ("scm", 1.0),
            ("scr", 5.0),
            ("cm", 0.0),
            ("al", linear(threshold_db)),
            ("at", attack_ms),
            ("rt", release_ms),
            ("cr", ratio),
            ("kn", 1.0),
            ("mk", linear(makeup_db)),
            ("cdr", 0.0),
            ("cwt", 1.0),
            ("cdw", 100.0),
        ],
        Effect::Equalizer {
            frequency_hz,
            gain_db,
            q,
        } => &[
            ("mode", 0.0),
            ("ft_0", 1.0),
            ("fm_0", 0.0),
            ("s_0", 0.0),
            ("f_0", frequency_hz),
            ("g_0", linear(gain_db)),
            ("q_0", q),
            ("ft_1", 0.0),
            ("ft_2", 0.0),
            ("ft_3", 0.0),
            ("ft_4", 0.0),
            ("ft_5", 0.0),
            ("ft_6", 0.0),
            ("ft_7", 0.0),
        ],
        Effect::Limiter {
            ceiling_db,
            release_ms,
        } => &[
            ("alr", 0.0),
            ("th", linear(ceiling_db)),
            ("boost", 0.0),
            ("lk", 5.0),
            ("at", 1.0),
            ("rt", release_ms),
            ("ovs", 0.0),
            ("dith", 0.0),
            ("extsc", 0.0),
        ],
    };
    controls.extend(specific.iter().map(|&(name, value)| (name.into(), value)));
    controls
}

fn controls(bus: &Bus, audible: bool) -> Controls {
    let mut values = Vec::new();
    for effect in &bus.effects {
        values.extend(
            effect_controls(effect)
                .into_iter()
                .map(|(name, value)| (format!("fx_{}:{name}", effect.id), value)),
        );
    }
    for index in 0..channel_count(bus.channels) {
        values.push((format!("fader_{index}:Gain 1"), linear(bus.gain_db)));
        values.push((
            format!("volume_{index}:Gain 1"),
            if audible { 1.0 } else { 0.0 },
        ));
    }
    values
}

fn control_pod(controls: &Controls) -> Result<Vec<u8>, String> {
    let values = controls
        .iter()
        .flat_map(|(name, value)| [Value::String(name.clone()), Value::Float(*value)])
        .collect();
    let object = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pw::spa::sys::SPA_PARAM_Props,
        properties: vec![Property::new(
            pw::spa::sys::SPA_PROP_params,
            Value::Struct(values),
        )],
    });
    pw::spa::pod::serialize::PodSerializer::serialize(Cursor::new(Vec::new()), &object)
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("cannot encode native controls: {error:?}"))
}

fn channel_count(channels: Channels) -> u32 {
    match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
    }
}

fn module_arguments(bus: &Bus, remote: Option<&str>, audible: bool) -> Json {
    let stereo = bus.channels == Channels::Stereo;
    let inputs: &[&str] = if stereo { &["in_l", "in_r"] } else { &["in"] };
    let outputs: &[&str] = if stereo {
        &["out_l", "out_r"]
    } else {
        &["out"]
    };
    let position: &[&str] = if stereo { &["FL", "FR"] } else { &["MONO"] };
    let mut nodes = Vec::new();
    let mut links = Vec::new();
    for effect in &bus.effects {
        let control = effect_controls(effect)
            .into_iter()
            .map(|(name, value)| (name, json!(value)))
            .collect::<serde_json::Map<_, _>>();
        nodes.push(json!({"type":"lv2","name":format!("fx_{}",effect.id),"plugin":format!("http://lsp-plug.in/plugins/lv2/{}_{}",plugin_name(&effect.effect),if stereo {"stereo"} else {"mono"}),"control":control}));
    }
    // Keep the fader before a trailing limiter even while it is bypassed, so
    // enabling it and changing gain remain native control-only updates.
    let fader_position = bus.effects.len()
        - usize::from(
            bus.effects
                .last()
                .is_some_and(|effect| matches!(&effect.effect, Effect::Limiter { .. })),
        );
    let mut graph_inputs = Vec::new();
    for (index, (input, output)) in inputs.iter().zip(outputs).enumerate() {
        nodes.push(json!({"type":"builtin","name":format!("fader_{index}"),"label":"mixer","control":{"Gain 1":linear(bus.gain_db)}}));
        // Safety mute is always last, including after plugins with release tails.
        nodes.push(json!({"type":"builtin","name":format!("volume_{index}"),"label":"mixer","control":{"Gain 1":if audible {1.0} else {0.0}}}));
        let mut chain = bus
            .effects
            .iter()
            .map(|effect| {
                (
                    format!("fx_{}:{input}", effect.id),
                    format!("fx_{}:{output}", effect.id),
                )
            })
            .collect::<Vec<_>>();
        chain.insert(
            fader_position,
            (format!("fader_{index}:In 1"), format!("fader_{index}:Out")),
        );
        chain.push((
            format!("volume_{index}:In 1"),
            format!("volume_{index}:Out"),
        ));
        graph_inputs.push(chain[0].0.clone());
        for pair in chain.windows(2) {
            links.push(json!({"output":pair[0].1,"input":pair[1].0}));
        }
    }
    let graph_outputs = (0..position.len())
        .map(|index| format!("volume_{index}:Out"))
        .collect::<Vec<_>>();
    let endpoint = |output: bool| {
        json!({
            "node.name":bus_name(bus,output), "node.description":bus.name,
            "media.class":if output {"Audio/Source"} else {"Audio/Sink"},
            "cosmos.owner":OWNER, "cosmos.bus.id":bus.id,
            "node.autoconnect":false, "node.dont-reconnect":true,
            "node.always-process":true,"node.pause-on-idle":false,
            "audio.channels":channel_count(bus.channels),"audio.position":position,
            "adapter.auto-port-config":{"mode":"dsp","monitor":false},
        })
    };
    let mut result = json!({
        "node.description":bus.name,"audio.rate":48000,
        "audio.channels":channel_count(bus.channels),"audio.position":position,
        "filter.graph":{"nodes":nodes,"links":links,"inputs":graph_inputs,"outputs":graph_outputs},
        "capture.props":endpoint(false),"playback.props":endpoint(true),
    });
    if let Some(remote) = remote {
        result["remote.name"] = json!(remote);
    }
    result
}

fn input_meter_pairs(
    input: &[u32],
    meter: &[u32],
    observed: &Observed,
) -> Result<PortPairs, Failure> {
    let mut pairs = std::collections::BTreeSet::new();
    for (&input, &meter) in input.iter().zip(meter) {
        for link in observed
            .links
            .values()
            .filter(|link| link.input_port == input)
        {
            if observed
                .ports
                .get(&link.output_port)
                .is_some_and(|port| port.output)
                && !pairs.insert((link.output_port, meter))
            {
                return Err((
                    RuntimeState::Error,
                    "duplicate native source links prevent an exact input meter tap".into(),
                ));
            }
        }
    }
    if pairs.is_empty() {
        return Err((
            RuntimeState::Pending,
            "input meter is waiting for an actual source link".into(),
        ));
    }
    Ok(pairs.into_iter().collect())
}

fn reconcile_meters(
    host: &mut HostedBus,
    bus: &Bus,
    connection: &Connection,
    nodes: &[Node],
    observed: &Observed,
    dirty: &Rc<Cell<bool>>,
) {
    if host.meters.as_ref().is_some_and(|meters| {
        [&meters.input, &meters.output]
            .iter()
            .any(|meter| meter.failed())
    }) {
        host.meters = None;
        host.retry_at = Instant::now() + RETRY_MAX;
    }
    if host.meters.is_none() && Instant::now() >= host.retry_at {
        let input = Meter::new(
            connection.core().clone(),
            &format!("cosmos.bus.{}.meter-input", bus.id),
            channel_count(bus.channels),
        );
        let output = Meter::new(
            connection.core().clone(),
            &format!("cosmos.bus.{}.meter-output", bus.id),
            channel_count(bus.channels),
        );
        match (input, output) {
            (Ok(input), Ok(output)) => {
                host.meters = Some(BusMeters {
                    input_links: MeterRoute::new(),
                    output_links: MeterRoute::new(),
                    input,
                    output,
                });
            }
            _ => {
                host.retry_at = Instant::now() + RETRY_MAX;
            }
        }
    }
    let Some(meters) = host.meters.as_mut() else {
        return;
    };
    for output in [false, true] {
        let route = if output {
            &mut meters.output_links
        } else {
            &mut meters.input_links
        };
        let meter = if output {
            &meters.output
        } else {
            &meters.input
        };
        let result = bus_node(bus, output, nodes, observed).and_then(|bus_node| {
            let node = nodes
                .iter()
                .find(|node| {
                    node.id == meter.node_id()
                        && node.name == meter.node_name()
                        && observed.is_owned(node.id)
                })
                .ok_or_else(|| (RuntimeState::Pending, "waiting for meter ports".into()))?;
            let target = node_ports(node, false, false, observed)?;
            if target.1.len() as u32 != meter.channels() {
                return Err((RuntimeState::Error, "meter channel layout differs".into()));
            }
            if output {
                pair_ports(node_ports(bus_node, true, false, observed)?, target)
            } else {
                let input = node_ports(bus_node, false, false, observed)?;
                if input.0 != target.0 {
                    return Err((
                        RuntimeState::Error,
                        "input meter channel layout differs".into(),
                    ));
                }
                // Native sink monitor chunks can carry stale EMPTY flags. Tap
                // the actual incoming source ports instead; PipeWire mixes the
                // same per-channel signals in the passive meter's input ports.
                input_meter_pairs(&input.1, &target.1, observed)
            }
        });
        match result {
            Ok(pairs) => {
                if !route.prepare(pairs, Instant::now()) {
                    continue;
                }
                if route.links.is_none() {
                    match OwnedRoute::new(connection, route.pairs.clone(), dirty) {
                        Ok(links) => {
                            route.links = Some(links);
                        }
                        Err(_) => {
                            route.retry_at = Instant::now() + RETRY_MAX;
                        }
                    }
                }
            }
            Err(_) => {
                route.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmos_control::EffectInstance;

    fn bus(channels: Channels) -> Bus {
        Bus {
            id: "test".into(),
            name: "Test".into(),
            channels,
            enabled: true,
            hidden: false,
            muted: false,
            gain_db: 0.0,
            effects: vec![EffectInstance {
                id: "gate".into(),
                enabled: true,
                effect: Effect::Gate {
                    threshold_db: -20.0,
                    attack_ms: 5.0,
                    hold_ms: 0.0,
                    release_ms: 20.0,
                },
            }],
        }
    }

    #[test]
    fn input_meter_fans_out_actual_sources_per_channel_including_external_playback() {
        use crate::discovery::{GraphLink, Port};
        let mut observed = Observed::new();
        for id in [10, 11, 12] {
            observed.ports.insert(
                id,
                Port {
                    id,
                    node_id: id,
                    output: true,
                    channel: "MONO".into(),
                    monitor: false,
                },
            );
        }
        for (id, output_port, input_port, owned) in
            [(1, 10, 20, true), (2, 11, 20, false), (3, 12, 21, false)]
        {
            observed.links.insert(
                id,
                GraphLink {
                    output_port,
                    input_port,
                    owned,
                },
            );
        }
        assert_eq!(
            input_meter_pairs(&[20, 21], &[30, 31], &observed).unwrap(),
            vec![(10, 30), (11, 30), (12, 31)]
        );
        observed.links.remove(&2);
        assert_eq!(
            input_meter_pairs(&[20, 21], &[30, 31], &observed).unwrap(),
            vec![(10, 30), (12, 31)]
        );
    }
    #[test]
    fn resolved_graph_errors_leave_unrelated_routes_usable() {
        let desired = vec![
            ("cycle-in".into(), 1, 2),
            ("cycle-out".into(), 3, 1),
            ("duplicate-a".into(), 7, 8),
            ("duplicate-b".into(), 7, 8),
            ("sibling".into(), 5, 6),
        ];
        let graph = BTreeMap::from([
            (1, vec![2]),
            (2, vec![3]),
            (3, vec![1]),
            (7, vec![8]),
            (5, vec![6]),
        ]);
        let errors = route_graph_errors(&desired, &graph);
        assert_eq!(errors.len(), 4);
        assert!(errors["cycle-in"].contains("cycle"));
        assert!(errors["cycle-out"].contains("cycle"));
        assert!(errors["duplicate-a"].contains("same native connection"));
        assert!(errors["duplicate-b"].contains("same native connection"));
        assert!(!errors.contains_key("sibling"));
    }

    #[test]
    fn meter_topology_changes_bypass_old_backoff_without_delaying_other_tap() {
        let now = Instant::now();
        let mut input = MeterRoute::new();
        let mut output = MeterRoute::new();
        assert!(input.prepare(vec![(10, 30), (11, 30)], now));
        assert!(output.prepare(vec![(20, 40)], now));
        input.retry_at = now + RETRY_MAX;
        assert!(!input.prepare(vec![(10, 30), (11, 30)], now));
        assert!(input.prepare(vec![(11, 30)], now));
        assert_eq!(input.pairs, vec![(11, 30)]);
        assert!(output.prepare(vec![(20, 40)], now));
        input.retry_at = now + RETRY_MAX;
        input.clear();
        assert!(input.prepare(vec![(12, 30)], now));
    }

    #[test]
    fn repeated_publication_failures_back_off_until_ports_and_controls_are_ready() {
        let mut host = HostedBus::new(&bus(Channels::Mono));
        host.failed("publication failed".into());
        host.failed("publication failed again".into());
        assert_eq!(host.retry_delay, RETRY_MIN * 4);
        host.controls_applied(42, vec![("volume_0:Gain 1".into(), 0.0)]);
        assert_eq!(host.control_node, Some(42));
        assert_eq!(host.retry_delay, RETRY_MIN * 4);
        host.published();
        assert_eq!(host.retry_delay, RETRY_MIN);
        host.failed("failed after established publication".into());
        assert_eq!(host.retry_delay, RETRY_MIN * 2);
    }

    #[test]
    fn expired_route_failures_do_not_keep_the_control_loop_awake() {
        let now = Instant::now();
        let mut retries = BTreeMap::from([
            ("unplugged-source".into(), now - RETRY_MAX),
            ("waiting-for-retry".into(), now + RETRY_MIN),
        ]);
        assert_eq!(next_route_retry(&retries, now), Some(now + RETRY_MIN));
        retries.remove("waiting-for-retry");
        assert_eq!(next_route_retry(&retries, now), None);
        assert_eq!(next_route_retry(&retries, now + RETRY_MAX), None);
    }

    #[test]
    fn retiring_a_bus_identifies_routes_and_cross_bus_meter_taps_but_not_siblings() {
        use crate::discovery::Port;
        let mut observed = Observed::new();
        for (id, node_id) in [(10, 1), (20, 2), (30, 3)] {
            observed.ports.insert(
                id,
                Port {
                    id,
                    node_id,
                    output: true,
                    channel: "MONO".into(),
                    monitor: false,
                },
            );
        }
        let retired = std::collections::BTreeSet::from([2]);
        assert!(touches_retired(&[(10, 20)], &observed, &retired));
        assert!(touches_retired(&[(20, 30)], &observed, &retired));
        assert!(!touches_retired(&[(10, 30)], &observed, &retired));
    }
}
