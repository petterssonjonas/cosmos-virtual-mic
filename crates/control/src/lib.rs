// SPDX-License-Identifier: MPL-2.0
//! Toolkit-independent local engine control. One bounded JSON line per connection.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod diagnostics;
pub mod model;
pub mod socket;
pub use model::{
    BackendStatus, Bus, BusStatus, Channels, DeviceSettings, Effect, EffectInstance, MeterSnapshot,
    NodeRef, Profile, Route, RouteStatus, RuntimeState,
};

pub const VERSION: u32 = 3;
pub const MAX_REQUEST_BYTES: usize = model::MAX_PROFILE_BYTES + 4096;
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Status,
    List,
    Stop,
    GetProfile,
    Apply,
    Update,
    Flush,
    SetDeviceProfile,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<DeviceProfileRequest>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Status {
    pub pid: u32,
    pub phase: Phase,
    /// Incremented before each connection attempt. Node IDs are valid only in this generation.
    pub generation: u64,
    pub last_error: Option<String>,
    pub retry_ms: u64,
    pub discovery_error: Option<String>,
    /// Most recent recoverable IPC failure; retained without an unbounded event/log queue.
    pub last_control_error: Option<String>,
    pub backend: BackendStatus,
    /// Recoverable configuration error. Defaults are never written over unreadable data.
    pub config_error: Option<String>,
    /// Accepted live changes that have not yet been durably saved.
    pub config_pending: bool,
    /// Latest explicit OS profile operation; never part of a saved mixer profile.
    pub device_profile_change: Option<DeviceProfileChange>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    Mono,
    Stereo,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Layout {
    pub kind: LayoutKind,
    pub channels: Option<u32>,
    pub positions: Vec<String>,
}

impl Layout {
    /// Report only known channel semantics; never imply a downmix or channel selection.
    pub fn new(channels: Option<u32>, positions: Vec<String>) -> Self {
        let kind = match (channels, positions.as_slice()) {
            (None, _) => LayoutKind::Unknown,
            (Some(1), [position]) if position == "MONO" || position == "FC" => LayoutKind::Mono,
            (Some(2), [left, right]) if left == "FL" && right == "FR" => LayoutKind::Stereo,
            (Some(1 | 2), []) => LayoutKind::Unknown,
            _ => LayoutKind::Unsupported,
        };
        Self {
            kind,
            channels,
            positions,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Node {
    pub id: u32,
    pub generation: u64,
    pub name: String,
    pub description: String,
    pub media_class: String,
    /// Server-provided identity candidates, not a promise of uniqueness or persistence.
    /// Numeric object/device IDs and object.serial are deliberately excluded.
    pub identity: BTreeMap<String, String>,
    pub layout: Layout,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAvailability {
    Unknown,
    No,
    Yes,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeviceProfile {
    pub index: i32,
    pub name: String,
    pub description: String,
    pub availability: ProfileAvailability,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AudioDevice {
    pub id: u32,
    pub generation: u64,
    /// Missing or malformed serials make the device unsafe to target.
    pub serial: Option<u64>,
    pub name: String,
    pub description: String,
    pub current_profile: Option<i32>,
    pub profiles: Vec<DeviceProfile>,
    /// True only for an explicitly advertised current profile named "off".
    pub disabled: bool,
    pub readable: bool,
    pub writable: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfileRequest {
    pub engine_pid: u32,
    pub generation: u64,
    pub id: u32,
    pub serial: u64,
    pub profile_index: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceProfileState {
    Pending,
    Confirmed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeviceProfileChange {
    /// Engine-local correlation token, unique for every accepted native operation.
    pub sequence: u64,
    pub request: DeviceProfileRequest,
    pub state: DeviceProfileState,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Response {
    Snapshot {
        version: u32,
        status: Status,
        nodes: Vec<Node>,
        devices: Vec<AudioDevice>,
    },
    Stopping {
        version: u32,
        stopping: bool,
    },
    Profile {
        version: u32,
        profile: Profile,
    },
    Applied {
        version: u32,
        accepted: bool,
        status: Status,
    },
    DeviceProfile {
        version: u32,
        change: DeviceProfileChange,
    },
    Error {
        version: u32,
        error: String,
    },
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn default_socket() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        invalid("XDG_RUNTIME_DIR is not set; use --socket in a private directory")
    })?;
    Ok(PathBuf::from(runtime).join("cosmos-audio-mixer/control.sock"))
}

/// A frame is complete only at its newline; extra bytes and oversized messages are rejected.
pub fn frame(bytes: &[u8], limit: usize) -> io::Result<Option<&[u8]>> {
    if bytes.len() > limit {
        return Err(invalid("message exceeds byte limit"));
    }
    match bytes.iter().position(|byte| *byte == b'\n') {
        Some(index) if index + 1 == bytes.len() => Ok(Some(&bytes[..index])),
        Some(_) => Err(invalid("only one request per connection is allowed")),
        None => Ok(None),
    }
}

pub fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(invalid("request exceeds control message limit"));
    }
    let request: Request = serde_json::from_slice(bytes).map_err(invalid_json)?;
    if request.version != VERSION {
        return Err(invalid("unsupported control protocol version"));
    }
    match (&request.command, &request.profile) {
        (Command::Apply | Command::Update, Some(profile)) => profile.validate()?,
        (Command::Apply | Command::Update, None) => {
            return Err(invalid("apply and update require a profile"));
        }
        (_, Some(_)) => return Err(invalid("profile is valid only for apply or update")),
        _ => {}
    }
    match (&request.command, &request.device_profile) {
        (Command::SetDeviceProfile, Some(selection)) if selection.profile_index >= 0 => {}
        (Command::SetDeviceProfile, Some(_)) => {
            return Err(invalid("device profile index must be nonnegative"));
        }
        (Command::SetDeviceProfile, None) => {
            return Err(invalid("set_device_profile requires device_profile"));
        }
        (_, Some(_)) => {
            return Err(invalid(
                "device_profile is valid only for set_device_profile",
            ));
        }
        _ => {}
    }
    Ok(request)
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(error.to_string())
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(invalid("encoded response exceeds byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn encode(value: &impl Serialize, limit: usize) -> io::Result<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(invalid_json)?;
    writer.write_all(b"\n")?;
    Ok(writer.bytes)
}

/// Check Linux peer credentials in addition to filesystem access controls.
pub fn check_peer(stream: &UnixStream) -> io::Result<()> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the kernel writes at most `length` bytes to valid, aligned storage.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(invalid("invalid Unix peer credentials"));
    }
    // SAFETY: a successful SO_PEERCRED call returned the complete structure.
    let credentials = unsafe { credentials.assume_init() };
    // SAFETY: geteuid has no pointer or lifetime requirements.
    if credentials.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "engine peer belongs to another user",
        ));
    }
    Ok(())
}

fn wait_fd(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "control operation timed out")
            })?;
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let timeout = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: poll receives one valid pollfd and does not retain it.
        let result = unsafe { libc::poll(&mut poll, 1, timeout) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn connect(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    // SAFETY: socket returns a new descriptor, owned exactly once below.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw is a valid, newly created descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: zero is valid for sockaddr_un; sun_family is set before use.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(invalid("control socket path is too long or contains NUL"));
    }
    for (output, input) in address.sun_path.iter_mut().zip(bytes) {
        *output = *input as libc::c_char;
    }
    // SAFETY: address is initialized and its complete size is supplied.
    let result = unsafe {
        libc::connect(
            descriptor.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    let stream = UnixStream::from(descriptor);
    if result < 0 {
        let error = io::Error::last_os_error();
        // Linux returns EAGAIN when the Unix listener backlog is full; no pending connect exists.
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        wait_fd(&stream, libc::POLLOUT, deadline)?;
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
    }
    Ok(stream)
}

/// Connect, write and read against one absolute deadline (a slow peer cannot extend it).
pub fn request(path: &Path, command: Command) -> io::Result<Response> {
    send_request(
        path,
        Request {
            version: VERSION,
            command,
            profile: None,
            device_profile: None,
        },
    )
}

/// Apply a complete profile and acknowledge it only after a durable save.
pub fn apply(path: &Path, profile: Profile) -> io::Result<Response> {
    send_profile(path, Command::Apply, profile)
}

/// Apply a complete profile to live audio; inspect Status.config_pending for durability.
pub fn update(path: &Path, profile: Profile) -> io::Result<Response> {
    send_profile(path, Command::Update, profile)
}

/// Save all accepted live changes now. A save failure returns Response::Error.
pub fn flush(path: &Path) -> io::Result<Response> {
    request(path, Command::Flush)
}

fn send_profile(path: &Path, command: Command, profile: Profile) -> io::Result<Response> {
    profile.validate()?;
    send_request(
        path,
        Request {
            version: VERSION,
            command,
            profile: Some(profile),
            device_profile: None,
        },
    )
}

/// Submit an explicit OS-level profile selection. A pending response is not success;
/// poll Status.device_profile_change for native confirmation or failure.
pub fn set_device_profile(path: &Path, selection: DeviceProfileRequest) -> io::Result<Response> {
    send_request(
        path,
        Request {
            version: VERSION,
            command: Command::SetDeviceProfile,
            profile: None,
            device_profile: Some(selection),
        },
    )
}

fn send_request(path: &Path, request: Request) -> io::Result<Response> {
    let message = encode(&request, MAX_REQUEST_BYTES)?;
    decode_request(&message[..message.len() - 1])?;
    socket::validate_parent(path)?;
    socket::validate_socket(path)?;
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    let mut stream = connect(path, deadline)?;
    check_peer(&stream)?;
    let mut remaining = message.as_slice();
    while !remaining.is_empty() {
        wait_fd(&stream, libc::POLLOUT, deadline)?;
        match stream.write(remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "engine closed request",
                ));
            }
            Ok(length) => remaining = &remaining[length..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    let mut response = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        wait_fd(&stream, libc::POLLIN, deadline)?;
        match stream.read(&mut buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "engine closed before a complete response",
                ));
            }
            Ok(length) => {
                if length > MAX_RESPONSE_BYTES.saturating_sub(response.len()) {
                    return Err(invalid("engine response exceeds byte limit"));
                }
                response.extend_from_slice(&buffer[..length]);
                if let Some(bytes) = frame(&response, MAX_RESPONSE_BYTES)? {
                    let value: Response = serde_json::from_slice(bytes).map_err(invalid_json)?;
                    let version = match &value {
                        Response::Snapshot { version, .. }
                        | Response::Stopping { version, .. }
                        | Response::Profile { version, .. }
                        | Response::Applied { version, .. }
                        | Response::DeviceProfile { version, .. }
                        | Response::Error { version, .. } => *version,
                    };
                    if version != VERSION {
                        return Err(invalid("unsupported engine protocol version"));
                    }
                    return Ok(value);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_rejects_oversize_and_extra_messages() {
        assert!(frame(b"12345", 4).is_err());
        assert!(frame(b"{}\n{}\n", 32).is_err());
        assert_eq!(frame(b"{}\n", 3).unwrap(), Some(b"{}".as_slice()));
        assert_eq!(frame(b"{}", 3).unwrap(), None);
    }

    #[test]
    fn requests_reject_unknown_commands_versions_and_fields() {
        for bytes in [
            br#"{"version":1,"command":"stop"}"#.as_slice(),
            br#"{"version":3,"command":"route"}"#.as_slice(),
            br#"{"version":3,"command":"stop","extra":1}"#.as_slice(),
            br#"{"version":3,"command":"apply"}"#.as_slice(),
            br#"{"version":3,"command":"update"}"#.as_slice(),
            br#"{"version":3,"command":"status","profile":{"version":1,"buses":[],"devices":[],"routes":[]}}"#.as_slice(),
            br#"{"version":3,"command":"apply","profile":{"version":2,"buses":[],"devices":[],"routes":[]}}"#.as_slice(),
            br#"{"version":3,"command":"update","profile":{"version":2,"buses":[],"devices":[],"routes":[]}}"#.as_slice(),
            br#"{"version":2,"command":"stop"}"#.as_slice(),
        ] {
            assert!(decode_request(bytes).is_err());
        }
    }

    #[test]
    fn profile_commands_accept_only_validated_complete_profiles() {
        for (name, command) in [("apply", Command::Apply), ("update", Command::Update)] {
            let bytes = format!(
                r#"{{"version":3,"command":"{name}","profile":{{"version":1,"buses":[],"devices":[],"routes":[]}}}}"#
            );
            let request = decode_request(bytes.as_bytes()).unwrap();
            assert_eq!(request.command, command);
            assert_eq!(request.profile.unwrap(), Profile::default());
        }
        for (name, command) in [
            ("get_profile", Command::GetProfile),
            ("flush", Command::Flush),
        ] {
            let bytes = format!(r#"{{"version":3,"command":"{name}"}}"#);
            assert_eq!(decode_request(bytes.as_bytes()).unwrap().command, command);
        }
    }

    #[test]
    fn os_profile_requests_are_explicit_and_separate_from_saved_profiles() {
        let request = decode_request(br#"{"version":3,"command":"set_device_profile","device_profile":{"engine_pid":100,"generation":3,"id":9,"serial":42,"profile_index":0}}"#).unwrap();
        assert_eq!(request.device_profile.unwrap().serial, 42);
        for bytes in [
            br#"{"version":3,"command":"set_device_profile"}"#.as_slice(),
            br#"{"version":3,"command":"status","device_profile":{"engine_pid":100,"generation":3,"id":9,"serial":42,"profile_index":0}}"#.as_slice(),
            br#"{"version":3,"command":"set_device_profile","device_profile":{"engine_pid":100,"generation":3,"id":9,"profile_index":0}}"#.as_slice(),
            br#"{"version":3,"command":"set_device_profile","device_profile":{"engine_pid":100,"generation":3,"id":9,"serial":42,"profile_index":-1}}"#.as_slice(),
            br#"{"version":3,"command":"set_device_profile","device_profile":{"generation":3,"id":9,"serial":42,"profile_index":0}}"#.as_slice(),
        ] {
            assert!(decode_request(bytes).is_err());
        }
    }

    #[test]
    fn encoding_bound_includes_newline() {
        assert!(encode(&"abcd", 6).is_err());
        assert_eq!(encode(&"abcd", 7).unwrap(), b"\"abcd\"\n");
    }

    #[test]
    fn layout_never_assumes_stereo_or_downmixes() {
        assert_eq!(Layout::new(Some(2), vec![]).kind, LayoutKind::Unknown);
        assert_eq!(
            Layout::new(Some(2), vec!["FR".into(), "FL".into()]).kind,
            LayoutKind::Unsupported
        );
        assert_eq!(Layout::new(Some(6), vec![]).kind, LayoutKind::Unsupported);
        assert_eq!(
            Layout::new(Some(1), vec!["MONO".into()]).kind,
            LayoutKind::Mono
        );
        assert_eq!(
            Layout::new(Some(2), vec!["FL".into(), "FR".into()]).kind,
            LayoutKind::Stereo
        );
    }
}
