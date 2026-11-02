// SPDX-License-Identifier: MPL-2.0
//! Native OS device profiles. These are deliberately unrelated to persisted mixer profiles.

use std::io::Cursor;
use std::time::{Duration, Instant};

use cosmos_control::{
    AudioDevice, DeviceProfile, DeviceProfileChange, DeviceProfileRequest, DeviceProfileState,
    Phase, ProfileAvailability,
};
use pipewire as pw;
use pw::permissions::PermissionFlags;
use pw::proxy::ProxyT;
use pw::spa::param::{ParamInfoFlags, ParamType};
use pw::spa::pod::{Object, Pod, Property, Value, serialize::PodSerializer};

use super::{BoundObject, Connection, Observed, Properties};

const MAX_PROFILES: usize = 128;
const MAX_PROFILE_POD_BYTES: usize = 64 * 1024;
const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) struct DeviceListeners {
    pub _device: pw::device::DeviceListener,
    pub _proxy: pw::proxy::ProxyListener,
}

impl pw::proxy::Listener for DeviceListeners {}

#[derive(Default)]
pub(super) struct DeviceRecord {
    pub properties: Properties,
    pub permissions: Option<PermissionFlags>,
    pub profiles: std::collections::BTreeMap<i32, DeviceProfile>,
    pub current: Option<i32>,
    pub enum_readable: bool,
    pub profile_readable: bool,
    pub profile_writable: bool,
    pub error: Option<String>,
}

impl DeviceRecord {
    pub fn update_flags(&mut self, id: ParamType, flags: ParamInfoFlags) {
        match id {
            ParamType::EnumProfile => {
                self.enum_readable = flags.contains(ParamInfoFlags::READ);
                if !self.enum_readable {
                    self.profiles.clear();
                }
            }
            ParamType::Profile => {
                self.profile_readable = flags.contains(ParamInfoFlags::READ);
                self.profile_writable = flags.contains(ParamInfoFlags::WRITE);
                if !self.profile_readable {
                    self.current = None;
                }
            }
            _ => {}
        }
    }

    pub fn readable_params(&self) -> &'static [ParamType] {
        if !self
            .permissions
            .is_some_and(|flags| flags.contains(PermissionFlags::R | PermissionFlags::X))
        {
            return &[];
        }
        match (self.enum_readable, self.profile_readable) {
            (true, true) => &[ParamType::EnumProfile, ParamType::Profile],
            (true, false) => &[ParamType::EnumProfile],
            (false, true) => &[ParamType::Profile],
            (false, false) => &[],
        }
    }

    fn readable(&self) -> bool {
        self.readable_params().len() == 2
    }

    fn writable(&self) -> bool {
        self.readable()
            && self.profile_writable
            && self
                .permissions
                .is_some_and(|flags| flags.contains(PermissionFlags::W))
            && self.serial().is_some()
    }

    fn serial(&self) -> Option<u64> {
        self.properties
            .get("object.serial")
            .and_then(|value| value.parse().ok())
    }

    pub fn parameter(&mut self, id: ParamType, index: u32, pod: Option<&Pod>) {
        if !self.readable_params().contains(&id) {
            return;
        }
        if id == ParamType::EnumProfile && index == 0 {
            self.profiles.clear();
        }
        let Some(pod) = pod else {
            if id == ParamType::Profile {
                self.current = None;
            }
            return;
        };
        match parse_profile(pod) {
            Ok(profile) if id == ParamType::Profile => self.current = Some(profile.index),
            Ok(profile) => {
                if self.profiles.len() < MAX_PROFILES || self.profiles.contains_key(&profile.index)
                {
                    self.profiles.insert(profile.index, profile);
                } else {
                    self.error = Some(format!(
                        "device profile discovery limited to {MAX_PROFILES} choices"
                    ));
                }
            }
            Err(error) => {
                if id == ParamType::Profile {
                    self.current = None;
                }
                self.error = Some(error);
            }
        }
    }

    fn snapshot(&self, id: u32, generation: u64) -> AudioDevice {
        AudioDevice {
            id,
            generation,
            serial: self.serial(),
            name: self
                .properties
                .get("device.name")
                .cloned()
                .unwrap_or_default(),
            description: self
                .properties
                .get("device.description")
                .or_else(|| self.properties.get("device.nick"))
                .cloned()
                .unwrap_or_default(),
            current_profile: self.current,
            profiles: self.profiles.values().cloned().collect(),
            disabled: self
                .current
                .and_then(|index| self.profiles.get(&index))
                .is_some_and(|profile| profile.name == "off"),
            readable: self.readable(),
            writable: self.writable(),
            error: self.error.clone(),
        }
    }
}

fn parse_profile(pod: &Pod) -> Result<DeviceProfile, String> {
    if pod.as_bytes().len() > MAX_PROFILE_POD_BYTES {
        return Err("oversized native device profile ignored".into());
    }
    let object = pod
        .as_object()
        .map_err(|_| "native device profile is not an object".to_owned())?;
    if object.type_().as_raw() != pw::spa::sys::SPA_TYPE_OBJECT_ParamProfile {
        return Err("native device profile has the wrong object type".into());
    }
    let mut index = None;
    let mut name = String::new();
    let mut description = String::new();
    let mut availability = ProfileAvailability::Unknown;
    for property in object.props() {
        let value = property.value();
        match property.key().0 {
            pw::spa::sys::SPA_PARAM_PROFILE_index => {
                index = value.get_int().ok().filter(|index| *index >= 0)
            }
            pw::spa::sys::SPA_PARAM_PROFILE_name => {
                name = value
                    .get_string_raw()
                    .ok()
                    .flatten()
                    .and_then(|value| value.to_str().ok())
                    .map(bounded)
                    .unwrap_or_default();
            }
            pw::spa::sys::SPA_PARAM_PROFILE_description => {
                description = value
                    .get_string_raw()
                    .ok()
                    .flatten()
                    .and_then(|value| value.to_str().ok())
                    .map(bounded)
                    .unwrap_or_default();
            }
            pw::spa::sys::SPA_PARAM_PROFILE_available => {
                availability = match value.get_id().ok().map(|value| value.0) {
                    Some(pw::spa::sys::SPA_PARAM_AVAILABILITY_no) => ProfileAvailability::No,
                    Some(pw::spa::sys::SPA_PARAM_AVAILABILITY_yes) => ProfileAvailability::Yes,
                    _ => ProfileAvailability::Unknown,
                };
            }
            _ => {}
        }
    }
    Ok(DeviceProfile {
        index: index.ok_or("native device profile lacks a valid index")?,
        name,
        description,
        availability,
    })
}

fn bounded(value: &str) -> String {
    value.chars().take(512).collect()
}

fn profile_pod(index: i32) -> Result<Vec<u8>, String> {
    let object = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_ParamProfile,
        id: pw::spa::sys::SPA_PARAM_Profile,
        properties: vec![
            Property::new(pw::spa::sys::SPA_PARAM_PROFILE_index, Value::Int(index)),
            // WirePlumber persists this explicit user selection, including off. Mixer
            // profile loading/reconnecting never issues this operation.
            Property::new(pw::spa::sys::SPA_PARAM_PROFILE_save, Value::Bool(true)),
        ],
    });
    PodSerializer::serialize(Cursor::new(Vec::new()), &object)
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| format!("cannot encode native profile selection: {error:?}"))
}

pub(super) struct PendingProfile {
    sequence: u64,
    request: DeviceProfileRequest,
    proxy_id: u32,
    set_sequence: Option<i32>,
    read_sequence: i32,
    barrier_sequence: i32,
    barrier_done: bool,
    profile_seen: bool,
    deadline: Instant,
}

impl Observed {
    pub fn device_snapshot(&self) -> Vec<AudioDevice> {
        if self.status.phase != Phase::Connected {
            return Vec::new();
        }
        self.devices
            .iter()
            .map(|(&id, record)| record.snapshot(id, self.status.generation))
            .collect()
    }

    fn validate_device_profile(&self, request: &DeviceProfileRequest) -> Result<(), String> {
        if request.engine_pid != self.status.pid {
            return Err("stale engine process; refresh the device list".into());
        }
        if self.status.phase != Phase::Connected || request.generation != self.status.generation {
            return Err("stale device generation or disconnected PipeWire server".into());
        }
        if self.device_operation.is_some() {
            return Err("another OS device profile change is still pending".into());
        }
        let record = self.devices.get(&request.id).ok_or("device was removed")?;
        if record.serial() != Some(request.serial) {
            return Err("stale device object serial; refresh the device list".into());
        }
        if !record.writable() {
            return Err(
                "device profiles require readable EnumProfile/Profile and write/execute access"
                    .into(),
            );
        }
        let profile = record
            .profiles
            .get(&request.profile_index)
            .ok_or("unknown device profile index")?;
        if profile.availability == ProfileAvailability::No {
            return Err("selected device profile is unavailable".into());
        }
        Ok(())
    }

    pub fn expire_device_profile(&mut self, now: Instant) {
        if self
            .device_operation
            .as_ref()
            .is_some_and(|pending| now >= pending.deadline)
        {
            self.fail_device_profile("native device profile change timed out without confirmation");
        }
    }

    pub(super) fn fail_device_profile(&mut self, message: &str) {
        let Some(pending) = self.device_operation.take() else {
            return;
        };
        let error = bounded(message);
        self.status.last_control_error = Some(error.clone());
        self.status.device_profile_change = Some(DeviceProfileChange {
            sequence: pending.sequence,
            request: pending.request,
            state: DeviceProfileState::Failed,
            error: Some(error),
        });
    }

    pub(super) fn device_error(&mut self, id: u32, sequence: i32, message: &str) {
        if self.device_operation.as_ref().is_some_and(|pending| {
            (pending.proxy_id == id
                && (pending
                    .set_sequence
                    .is_some_and(|set| same_native_sequence(sequence, set))
                    || same_native_sequence(sequence, pending.read_sequence)))
                || (id == pw::core::PW_ID_CORE
                    && same_native_sequence(sequence, pending.barrier_sequence))
        }) {
            self.fail_device_profile(message);
        }
    }

    pub(super) fn device_removed(&mut self, id: u32) {
        if self
            .device_operation
            .as_ref()
            .is_some_and(|pending| pending.request.id == id)
        {
            self.fail_device_profile("device removed during native profile change");
        }
    }

    pub(super) fn device_profile_seen(&mut self, id: u32, sequence: i32) {
        let current_matches = self
            .device_operation
            .as_ref()
            .is_some_and(|pending| self.device_profile_matches(&pending.request));
        if let Some(pending) = self.device_operation.as_mut()
            && pending.request.id == id
        {
            if !current_matches {
                pending.profile_seen = false;
            } else if same_native_sequence(sequence, pending.read_sequence)
                // Native pw_impl_device uses sequence 1 for subscribed notifications.
                // A late enum reply from a previous action is not fresh evidence.
                || (pending.barrier_done && sequence == 1)
            {
                pending.profile_seen = true;
            }
        }
        self.confirm_device_profile();
    }

    fn device_profile_matches(&self, request: &DeviceProfileRequest) -> bool {
        self.status.phase == Phase::Connected
            && self.status.pid == request.engine_pid
            && self.status.generation == request.generation
            && self.devices.get(&request.id).is_some_and(|record| {
                record.serial() == Some(request.serial)
                    && record.readable()
                    && record.current == Some(request.profile_index)
            })
    }

    pub(super) fn device_barrier_done(&mut self, sequence: i32) {
        if let Some(pending) = self.device_operation.as_mut()
            && sequence == pending.barrier_sequence
        {
            pending.barrier_done = true;
        }
        self.confirm_device_profile();
    }

    fn confirm_device_profile(&mut self) {
        if !self
            .device_operation
            .as_ref()
            .is_some_and(|pending| self.device_profile_matches(&pending.request))
        {
            if let Some(pending) = self.device_operation.as_mut() {
                pending.profile_seen = false;
            }
            return;
        }
        if !self
            .device_operation
            .as_ref()
            .is_some_and(|pending| pending.barrier_done && pending.profile_seen)
        {
            return;
        }
        if let Some(pending) = self.device_operation.take() {
            self.status.device_profile_change = Some(DeviceProfileChange {
                sequence: pending.sequence,
                request: pending.request,
                state: DeviceProfileState::Confirmed,
                error: None,
            });
        }
    }
}

fn same_native_sequence(actual: i32, expected: i32) -> bool {
    actual >= 0
        && expected >= 0
        && (actual as u32 & pw::spa::sys::SPA_ASYNC_SEQ_MASK)
            == (expected as u32 & pw::spa::sys::SPA_ASYNC_SEQ_MASK)
}

/// pipewire-rs 0.10 discards these return values, but native enum_params
/// replaces its supplied sequence with the returned asynchronous message sequence.
fn native_profile_request(
    core: &pw::core::CoreRc,
    device: &pw::device::Device,
    pod: &Pod,
) -> Result<(Option<i32>, i32), String> {
    use pw::spa::utils::result::{SpaResult, SpaSuccess};
    // SAFETY: the live device binding and core are held for this entire call on
    // the PipeWire main thread. Lookup avoids assumptions about Rust wrapper layout.
    let proxy = unsafe { pw::sys::pw_core_find_proxy(core.as_raw_ptr(), device.upcast_ref().id()) };
    if proxy.is_null() {
        return Err("native device proxy was removed".into());
    }
    // SAFETY: registry binding verified this proxy's Device interface. The POD
    // storage remains alive until the native marshaler has copied the message.
    let result = unsafe {
        pw::spa::spa_interface_call_method!(
            proxy,
            pw::sys::pw_device_methods,
            set_param,
            ParamType::Profile.as_raw(),
            0,
            pod.as_raw_ptr()
        )
    };
    let set_sequence = match SpaResult::from_c(result)
        .into_result()
        .map_err(|error| format!("native device set_param rejected profile: {error}"))?
    {
        SpaSuccess::Async(sequence) => Some(sequence.raw()),
        SpaSuccess::Sync(_) => None,
    };
    // SAFETY: same live Device proxy; null filter and a single Profile result
    // are valid for native enumeration.
    let result = unsafe {
        pw::spa::spa_interface_call_method!(
            proxy,
            pw::sys::pw_device_methods,
            enum_params,
            0,
            ParamType::Profile.as_raw(),
            0,
            1,
            std::ptr::null()
        )
    };
    match SpaResult::from_c(result).into_result() {
        Ok(SpaSuccess::Async(sequence)) => Ok((set_sequence, sequence.raw())),
        Ok(SpaSuccess::Sync(_)) => {
            Err("native profile readback did not provide an asynchronous sequence".into())
        }
        Err(error) => Err(format!("native device profile readback failed: {error}")),
    }
}

impl Connection {
    pub fn set_device_profile(
        &self,
        observed: &mut Observed,
        request: DeviceProfileRequest,
    ) -> Result<DeviceProfileChange, String> {
        observed.validate_device_profile(&request)?;
        let bindings = self._bindings.borrow();
        let Some(super::Binding {
            object: BoundObject::Device(device),
            ..
        }) = bindings.get(&request.id)
        else {
            return Err("device has no live native binding".into());
        };
        let bytes = profile_pod(request.profile_index)?;
        let pod = Pod::from_bytes(&bytes).ok_or("invalid encoded native device profile")?;
        observed.device_operation_sequence = observed
            .device_operation_sequence
            .checked_add(1)
            .ok_or("native device operation sequence exhausted")?;
        let sequence = observed.device_operation_sequence;
        let change = DeviceProfileChange {
            sequence,
            request: request.clone(),
            state: DeviceProfileState::Pending,
            error: None,
        };
        observed.device_operation = Some(PendingProfile {
            sequence,
            request,
            proxy_id: device.upcast_ref().id(),
            set_sequence: None,
            read_sequence: -1,
            barrier_sequence: -1,
            barrier_done: false,
            profile_seen: false,
            deadline: Instant::now() + PROFILE_TIMEOUT,
        });
        observed.status.device_profile_change = Some(change.clone());
        if let Some(record) = observed.devices.get_mut(&change.request.id) {
            record.error = None;
        }
        match native_profile_request(&self._core, device, pod) {
            Ok((set_sequence, read_sequence)) => {
                if let Some(pending) = observed.device_operation.as_mut() {
                    pending.set_sequence = set_sequence;
                    pending.read_sequence = read_sequence;
                }
            }
            Err(error) => {
                observed.fail_device_profile(&error);
                return Err(error);
            }
        }
        match self._core.sync(0) {
            Ok(barrier) => {
                if let Some(pending) = observed.device_operation.as_mut() {
                    pending.barrier_sequence = barrier.seq();
                }
            }
            Err(error) => {
                let error = format!("cannot synchronize native profile change: {error}");
                observed.fail_device_profile(&error);
                return Err(error);
            }
        }
        Ok(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::spa::pod::deserialize::PodDeserializer;

    fn state() -> (Observed, DeviceProfileRequest) {
        let mut observed = Observed::new();
        observed.status.pid = 100;
        observed.status.phase = Phase::Connected;
        observed.status.generation = 7;
        let mut record = DeviceRecord {
            permissions: Some(PermissionFlags::R | PermissionFlags::W | PermissionFlags::X),
            enum_readable: true,
            profile_readable: true,
            profile_writable: true,
            ..DeviceRecord::default()
        };
        record.properties.insert("object.serial", "91".into());
        record.profiles.insert(
            0,
            DeviceProfile {
                index: 0,
                name: "off".into(),
                description: "Off".into(),
                availability: ProfileAvailability::Yes,
            },
        );
        record.profiles.insert(
            1,
            DeviceProfile {
                index: 1,
                name: "duplex".into(),
                description: "Duplex".into(),
                availability: ProfileAvailability::Yes,
            },
        );
        observed.devices.insert(4, record);
        (
            observed,
            DeviceProfileRequest {
                engine_pid: 100,
                generation: 7,
                id: 4,
                serial: 91,
                profile_index: 0,
            },
        )
    }

    #[test]
    fn selection_rejects_stale_reused_unknown_unavailable_and_readonly_targets() {
        let (mut state, request) = state();
        assert!(state.validate_device_profile(&request).is_ok());
        for stale in [
            DeviceProfileRequest {
                engine_pid: 99,
                ..request.clone()
            },
            DeviceProfileRequest {
                generation: 6,
                ..request.clone()
            },
            DeviceProfileRequest {
                serial: 92,
                ..request.clone()
            },
            DeviceProfileRequest {
                id: 5,
                ..request.clone()
            },
            DeviceProfileRequest {
                profile_index: 22,
                ..request.clone()
            },
        ] {
            assert!(state.validate_device_profile(&stale).is_err());
        }
        state
            .devices
            .get_mut(&4)
            .unwrap()
            .profiles
            .get_mut(&0)
            .unwrap()
            .availability = ProfileAvailability::No;
        assert!(state.validate_device_profile(&request).is_err());
        state
            .devices
            .get_mut(&4)
            .unwrap()
            .profiles
            .get_mut(&0)
            .unwrap()
            .availability = ProfileAvailability::Unknown;
        assert!(state.validate_device_profile(&request).is_ok());
        state.devices.get_mut(&4).unwrap().profile_writable = false;
        assert!(state.validate_device_profile(&request).is_err());
    }

    #[test]
    fn selections_require_every_native_access_and_readability_flag() {
        for flags in [
            PermissionFlags::R | PermissionFlags::W,
            PermissionFlags::R | PermissionFlags::X,
            PermissionFlags::W | PermissionFlags::X,
            PermissionFlags::empty(),
        ] {
            let (mut state, request) = state();
            state.devices.get_mut(&4).unwrap().permissions = Some(flags);
            assert!(state.validate_device_profile(&request).is_err());
        }
        for parameter in [ParamType::EnumProfile, ParamType::Profile] {
            let (mut state, request) = state();
            state
                .devices
                .get_mut(&4)
                .unwrap()
                .update_flags(parameter, ParamInfoFlags::empty());
            assert!(state.validate_device_profile(&request).is_err());
        }
    }

    fn pending(state: &mut Observed, request: DeviceProfileRequest) {
        state.status.device_profile_change = Some(DeviceProfileChange {
            sequence: 1,
            request: request.clone(),
            state: DeviceProfileState::Pending,
            error: None,
        });
        state.device_operation = Some(PendingProfile {
            sequence: 1,
            request,
            proxy_id: 15,
            set_sequence: Some(9),
            read_sequence: 10,
            barrier_sequence: 11,
            barrier_done: false,
            profile_seen: false,
            deadline: Instant::now(),
        });
    }

    #[test]
    fn success_requires_fresh_readback_and_barrier_and_off_keeps_restore_choices() {
        let (mut state, request) = state();
        pending(&mut state, request);
        state.devices.get_mut(&4).unwrap().current = Some(0);
        state.device_profile_seen(4, 0);
        state.device_barrier_done(11);
        assert_eq!(
            state.status.device_profile_change.as_ref().unwrap().state,
            DeviceProfileState::Pending
        );
        state.device_profile_seen(4, 10);
        assert_eq!(
            state.status.device_profile_change.as_ref().unwrap().state,
            DeviceProfileState::Confirmed
        );
        let device = &state.device_snapshot()[0];
        assert!(device.disabled);
        assert_eq!(device.profiles.len(), 2);
    }

    #[test]
    fn rejection_timeout_removal_and_disconnect_are_retained_as_failures() {
        for cause in 0..4 {
            let (mut state, request) = state();
            pending(&mut state, request);
            match cause {
                0 => state.device_error(15, 9, "native rejection"),
                1 => state.expire_device_profile(Instant::now()),
                2 => state.device_removed(4),
                _ => state.reset(),
            }
            assert_eq!(
                state.status.device_profile_change.as_ref().unwrap().state,
                DeviceProfileState::Failed
            );
            assert!(state.status.last_control_error.is_some());
        }
    }

    #[test]
    fn late_errors_from_timed_out_actions_do_not_fail_the_next_action() {
        let (mut state, request) = state();
        pending(&mut state, request.clone());
        state.expire_device_profile(Instant::now());
        pending(&mut state, request);
        let pending = state.device_operation.as_mut().unwrap();
        pending.sequence = 2;
        pending.set_sequence = Some(pw::spa::utils::result::AsyncSeq::from_seq(29).raw());
        pending.read_sequence = pw::spa::utils::result::AsyncSeq::from_seq(30).raw();
        pending.barrier_sequence = 31;
        state
            .status
            .device_profile_change
            .as_mut()
            .unwrap()
            .sequence = 2;
        for (id, sequence) in [(15, 9), (15, 10), (pw::core::PW_ID_CORE, 11), (16, 29)] {
            state.device_error(id, sequence, "late or unrelated error");
        }
        assert_eq!(
            state.status.device_profile_change.as_ref().unwrap().state,
            DeviceProfileState::Pending
        );
        state.devices.get_mut(&4).unwrap().current = Some(0);
        state.device_barrier_done(31);
        state.device_profile_seen(4, 10);
        assert!(
            state.device_operation.is_some(),
            "old readback cannot confirm the new action"
        );
        state.device_profile_seen(4, 30);
        assert_eq!(
            state.status.device_profile_change.as_ref().unwrap().state,
            DeviceProfileState::Confirmed
        );
        assert_eq!(
            state
                .status
                .device_profile_change
                .as_ref()
                .unwrap()
                .sequence,
            2
        );
    }

    #[test]
    fn errors_match_native_set_read_and_barrier_sequences_in_either_representation() {
        for (id, sequence) in [(15, 9), (15, 10), (pw::core::PW_ID_CORE, 11)] {
            for sequence in [
                sequence,
                pw::spa::utils::result::AsyncSeq::from_seq(sequence).raw(),
            ] {
                let (mut state, request) = state();
                pending(&mut state, request);
                state.device_error(id, sequence, "matching native failure");
                assert_eq!(
                    state.status.device_profile_change.as_ref().unwrap().state,
                    DeviceProfileState::Failed
                );
            }
        }
    }

    #[test]
    fn conflicting_or_null_observations_invalidate_readback_before_barrier() {
        for contradiction in [Some(1), None] {
            let (mut state, request) = state();
            pending(&mut state, request);
            state.devices.get_mut(&4).unwrap().current = Some(0);
            state.device_profile_seen(4, 10);
            state.devices.get_mut(&4).unwrap().current = contradiction;
            state.device_profile_seen(4, 1);
            // Restoring metadata alone cannot resurrect superseded evidence.
            state.devices.get_mut(&4).unwrap().current = Some(0);
            state.device_barrier_done(11);
            assert_eq!(
                state.status.device_profile_change.as_ref().unwrap().state,
                DeviceProfileState::Pending
            );
            state.device_profile_seen(4, 10);
            assert_eq!(
                state.status.device_profile_change.as_ref().unwrap().state,
                DeviceProfileState::Confirmed
            );
        }
    }

    #[test]
    fn confirmation_rechecks_current_profile_readability_and_target_identity() {
        for change in 0..6 {
            let (mut state, request) = state();
            pending(&mut state, request);
            state.devices.get_mut(&4).unwrap().current = Some(0);
            state.device_profile_seen(4, 10);
            match change {
                0 => state.devices.get_mut(&4).unwrap().current = None,
                1 => state.devices.get_mut(&4).unwrap().profile_readable = false,
                2 => {
                    state
                        .devices
                        .get_mut(&4)
                        .unwrap()
                        .properties
                        .insert("object.serial", "92".into());
                }
                3 => state.status.generation += 1,
                4 => state.status.pid += 1,
                _ => state.status.phase = Phase::Disconnected,
            }
            state.device_barrier_done(11);
            assert_eq!(
                state.status.device_profile_change.as_ref().unwrap().state,
                DeviceProfileState::Pending
            );
        }
    }

    #[test]
    fn selected_profile_pod_requests_wireplumber_save() {
        let bytes = profile_pod(3).unwrap();
        let (_, Value::Object(object)) = PodDeserializer::deserialize_any_from(&bytes).unwrap()
        else {
            panic!("not an object");
        };
        assert!(object.properties.iter().any(|property| property.key
            == pw::spa::sys::SPA_PARAM_PROFILE_save
            && property.value == Value::Bool(true)));
        assert_eq!(
            parse_profile(Pod::from_bytes(&bytes).unwrap())
                .unwrap()
                .index,
            3
        );
    }
}
