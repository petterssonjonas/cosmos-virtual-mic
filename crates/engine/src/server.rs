// SPDX-License-Identifier: MPL-2.0

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use control::socket::OwnedSocket;
use cosmos_control::{
    self as control, BackendStatus, Command, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Profile,
    Response, VERSION,
};

use crate::discovery::Observed;
use crate::persistence::Store;

const MAX_CLIENTS: usize = 8;
const MAX_ACCEPTS_PER_TICK: usize = 16;
const SERVER_TIMEOUT: Duration = Duration::from_secs(1);
const STOP_GRACE: Duration = Duration::from_millis(250);

fn recoverable_accept_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            libc::ECONNABORTED
                | libc::EPROTO
                | libc::EPERM
                | libc::EMFILE
                | libc::ENFILE
                | libc::ENOBUFS
                | libc::ENOMEM
        )
    )
}

fn record_control_error(observed: &mut Observed, operation: &str, error: &io::Error) {
    let mut message = format!("{operation}: {error}");
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    observed.status.last_control_error = Some(message);
}

struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
    written: usize,
    deadline: Instant,
    stopping: bool,
}

impl Client {
    fn response(&mut self, response: Response) -> io::Result<()> {
        self.output = Some(match control::encode(&response, MAX_RESPONSE_BYTES) {
            Ok(bytes) => bytes,
            Err(_) => control::encode(
                &Response::Error {
                    version: VERSION,
                    error: "response exceeds control message limit".into(),
                },
                MAX_RESPONSE_BYTES,
            )?,
        });
        Ok(())
    }

    fn reject(&mut self, error: io::Error) -> io::Result<()> {
        self.response(Response::Error {
            version: VERSION,
            error: error.to_string(),
        })
    }

    fn poll(
        &mut self,
        observed: &mut Observed,
        store: &mut Store,
        reconcile: &mut impl FnMut(&Profile, &Observed) -> BackendStatus,
        now: Instant,
    ) -> io::Result<bool> {
        if now >= self.deadline {
            return Ok(false);
        }
        if self.output.is_none() {
            let mut buffer = [0; 8192];
            match self.stream.read(&mut buffer) {
                Ok(0) => return Ok(false),
                Ok(length) => {
                    if length > MAX_REQUEST_BYTES.saturating_sub(self.input.len()) {
                        self.reject(control::invalid("request exceeds control message limit"))?;
                    } else {
                        self.input.extend_from_slice(&buffer[..length]);
                        match control::frame(&self.input, MAX_REQUEST_BYTES)
                            .and_then(|frame| frame.map(control::decode_request).transpose())
                        {
                            Ok(Some(request)) => {
                                let response = match request.command {
                                    Command::Stop => {
                                        self.stopping = true;
                                        Response::Stopping {
                                            version: VERSION,
                                            stopping: true,
                                        }
                                    }
                                    Command::GetProfile => Response::Profile {
                                        version: VERSION,
                                        profile: store.profile.clone(),
                                    },
                                    Command::Apply => {
                                        let profile = request.profile.ok_or_else(|| {
                                            control::invalid("apply requires a profile")
                                        })?;
                                        let validation =
                                            if observed.status.phase == control::Phase::Connected {
                                                crate::runtime::validate_observed_profile(
                                                    &profile, observed,
                                                )
                                                .map_err(control::invalid)
                                            } else {
                                                Ok(())
                                            };
                                        match validation.and_then(|()| store.apply(profile)) {
                                            Ok(()) => {
                                                observed.status.config_error = store.error.clone();
                                                observed.status.backend =
                                                    reconcile(&store.profile, observed);
                                                Response::Applied {
                                                    version: VERSION,
                                                    accepted: true,
                                                    status: observed.status.clone(),
                                                }
                                            }
                                            Err(error) => {
                                                observed.status.config_error = store.error.clone();
                                                Response::Error {
                                                    version: VERSION,
                                                    error: error.to_string(),
                                                }
                                            }
                                        }
                                    }
                                    command => Response::Snapshot {
                                        version: VERSION,
                                        status: observed.status.clone(),
                                        nodes: if command == Command::List {
                                            observed.snapshot()
                                        } else {
                                            Vec::new()
                                        },
                                    },
                                };
                                self.response(response)?;
                            }
                            Ok(None) => {}
                            Err(error) => self.reject(error)?,
                        }
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
        if let Some(output) = &self.output {
            let end = output.len().min(self.written + 65536);
            match self.stream.write(&output[self.written..end]) {
                Ok(0) => return Ok(false),
                Ok(length) => {
                    self.written += length;
                    if self.written == output.len() {
                        return Ok(false);
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
        Ok(true)
    }
}

pub struct Server {
    socket: OwnedSocket,
    clients: Vec<Client>,
    stopping_at: Option<Instant>,
}

impl Server {
    pub fn new(socket: OwnedSocket) -> Self {
        Self {
            socket,
            clients: Vec::with_capacity(MAX_CLIENTS),
            stopping_at: None,
        }
    }

    /// All sockets are nonblocking. Each tick has fixed accept, read, and write bounds.
    pub fn tick(
        &mut self,
        observed: &mut Observed,
        store: &mut Store,
        mut reconcile: impl FnMut(&Profile, &Observed) -> BackendStatus,
    ) -> io::Result<bool> {
        let now = Instant::now();
        if self.stopping_at.is_none() {
            for _ in 0..MAX_ACCEPTS_PER_TICK {
                match self.socket.listener.accept() {
                    Ok((stream, _)) => {
                        if self.clients.len() >= MAX_CLIENTS
                            || control::check_peer(&stream).is_err()
                        {
                            continue;
                        }
                        if let Err(error) = stream.set_nonblocking(true) {
                            record_control_error(
                                observed,
                                "cannot configure control client",
                                &error,
                            );
                            continue;
                        }
                        self.clients.push(Client {
                            stream,
                            input: Vec::new(),
                            output: None,
                            written: 0,
                            deadline: now + SERVER_TIMEOUT,
                            stopping: false,
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if recoverable_accept_error(&error) => {
                        record_control_error(observed, "cannot accept control client", &error);
                        // Resource pressure or an aborted connection must not terminate
                        // the engine. The next bounded tick retries without a busy loop.
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let mut received_stop = false;
        self.clients.retain_mut(|client| {
            // One failed/disconnected client must not stop the engine or other clients.
            let keep = client
                .poll(observed, store, &mut reconcile, now)
                .unwrap_or(false);
            received_stop |= client.stopping;
            keep
        });
        if received_stop && self.stopping_at.is_none() {
            self.stopping_at = Some(now);
        }
        if let Some(started) = self.stopping_at {
            let stop_reply_pending = self.clients.iter().any(|client| client.stopping);
            if !stop_reply_pending || now.duration_since(started) >= STOP_GRACE {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_resource_and_connection_errors_are_recoverable_but_invalid_listeners_are_not() {
        for code in [
            libc::ECONNABORTED,
            libc::EPROTO,
            libc::EPERM,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOBUFS,
            libc::ENOMEM,
        ] {
            assert!(
                recoverable_accept_error(&io::Error::from_raw_os_error(code)),
                "errno {code}"
            );
        }
        for code in [libc::EBADF, libc::EINVAL, libc::ENOTSOCK] {
            assert!(
                !recoverable_accept_error(&io::Error::from_raw_os_error(code)),
                "errno {code}"
            );
        }
    }

    #[test]
    fn control_diagnostic_retains_only_one_bounded_utf8_message() {
        let mut observed = Observed::new();
        record_control_error(&mut observed, "accept", &io::Error::other("é".repeat(1000)));
        assert!(observed.status.last_control_error.as_ref().unwrap().len() <= 512);
        let diagnostic = observed.status.last_control_error.clone();
        observed.reset();
        assert_eq!(observed.status.last_control_error, diagnostic);
    }
}
