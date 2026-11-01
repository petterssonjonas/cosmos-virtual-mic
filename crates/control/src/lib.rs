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

pub mod model;
pub mod socket;
pub use model::{
    BackendStatus, Bus, BusStatus, Channels, DeviceSettings, Effect, EffectInstance, MeterSnapshot,
    NodeRef, Profile, Route, RouteStatus, RuntimeState,
};

pub const VERSION: u32 = 1;
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
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Response {
    Snapshot {
        version: u32,
        status: Status,
        nodes: Vec<Node>,
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
        (Command::Apply, Some(profile)) => profile.validate()?,
        (Command::Apply, None) => return Err(invalid("apply requires a profile")),
        (_, Some(_)) => return Err(invalid("profile is valid only for apply")),
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
        },
    )
}

pub fn apply(path: &Path, profile: Profile) -> io::Result<Response> {
    profile.validate()?;
    send_request(
        path,
        Request {
            version: VERSION,
            command: Command::Apply,
            profile: Some(profile),
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
            br#"{"version":2,"command":"stop"}"#.as_slice(),
            br#"{"version":1,"command":"route"}"#.as_slice(),
            br#"{"version":1,"command":"stop","extra":1}"#.as_slice(),
            br#"{"version":1,"command":"apply"}"#.as_slice(),
            br#"{"version":1,"command":"status","profile":{"version":1,"buses":[],"devices":[],"routes":[]}}"#.as_slice(),
            br#"{"version":1,"command":"apply","profile":{"version":2,"buses":[],"devices":[],"routes":[]}}"#.as_slice(),
        ] {
            assert!(decode_request(bytes).is_err());
        }
    }

    #[test]
    fn apply_accepts_only_validated_profiles_and_profile_query_is_separate() {
        let apply = decode_request(br#"{"version":1,"command":"apply","profile":{"version":1,"buses":[],"devices":[],"routes":[]}}"#).unwrap();
        assert_eq!(apply.command, Command::Apply);
        assert_eq!(apply.profile.unwrap(), Profile::default());
        assert_eq!(
            decode_request(br#"{"version":1,"command":"get_profile"}"#)
                .unwrap()
                .command,
            Command::GetProfile
        );
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
