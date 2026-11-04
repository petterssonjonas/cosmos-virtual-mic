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
        stopping: &mut bool,
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
                        let error = control::invalid("request exceeds control message limit");
                        record_control_error(observed, "invalid control request", &error);
                        self.reject(error)?;
                    } else {
                        self.input.extend_from_slice(&buffer[..length]);
                        match control::frame(&self.input, MAX_REQUEST_BYTES)
                            .and_then(|frame| frame.map(control::decode_request).transpose())
                        {
                            Ok(Some(request)) => {
                                let response = match request.command {
                                    Command::Apply | Command::Update if *stopping => {
                                        Response::Error {
                                            version: VERSION,
                                            error: "engine is stopping; changes are not accepted"
                                                .into(),
                                        }
                                    }
                                    Command::Stop => {
                                        let result = store.flush_on_shutdown();
                                        store.report_status(&mut observed.status);
                                        match result {
                                            Ok(()) => {
                                                *stopping = true;
                                                self.stopping = true;
                                                Response::Stopping {
                                                    version: VERSION,
                                                    stopping: true,
                                                }
                                            }
                                            Err(error) => {
                                                record_control_error(
                                                    observed,
                                                    "cannot save profile before stop",
                                                    &error,
                                                );
                                                Response::Error {
                                                    version: VERSION,
                                                    error: error.to_string(),
                                                }
                                            }
                                        }
                                    }
                                    Command::Flush => {
                                        let result = store.flush();
                                        store.report_status(&mut observed.status);
                                        match result {
                                            Ok(()) => Response::Applied {
                                                version: VERSION,
                                                accepted: true,
                                                status: observed.status.clone(),
                                            },
                                            Err(error) => {
                                                record_control_error(
                                                    observed,
                                                    "cannot flush profile",
                                                    &error,
                                                );
                                                Response::Error {
                                                    version: VERSION,
                                                    error: error.to_string(),
                                                }
                                            }
                                        }
                                    }
                                    Command::GetProfile => Response::Profile {
                                        version: VERSION,
                                        profile: store.profile.clone(),
                                    },
                                    Command::Apply | Command::Update => {
                                        let profile = request.profile.ok_or_else(|| {
                                            control::invalid("apply and update require a profile")
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
                                        let result = validation.and_then(|()| {
                                            if request.command == Command::Update {
                                                store.update(profile, Instant::now())
                                            } else {
                                                store.apply(profile)
                                            }
                                        });
                                        store.report_status(&mut observed.status);
                                        // A rename followed by a directory-sync error may
                                        // still have accepted new desired state.
                                        observed.status.backend =
                                            reconcile(&store.profile, observed);
                                        match result {
                                            Ok(()) => Response::Applied {
                                                version: VERSION,
                                                accepted: true,
                                                status: observed.status.clone(),
                                            },
                                            Err(error) => {
                                                record_control_error(
                                                    observed,
                                                    "cannot apply profile",
                                                    &error,
                                                );
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
                                        devices: if command == Command::List {
                                            observed.device_snapshot()
                                        } else {
                                            Vec::new()
                                        },
                                    },
                                };
                                self.response(response)?;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                record_control_error(observed, "invalid control request", &error);
                                self.reject(error)?;
                            }
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
        store.report_status(&mut observed.status);
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
        let mut stopping = self.stopping_at.is_some();
        self.clients.retain_mut(|client| {
            // One failed/disconnected client must not stop the engine or other clients.
            let keep = match client.poll(observed, store, &mut reconcile, now, &mut stopping) {
                Ok(keep) => keep,
                Err(error) => {
                    record_control_error(observed, "control client failed", &error);
                    false
                }
            };
            stopping |= client.stopping;
            keep
        });
        // Process accepted changes before checking the deadline, so a new value
        // at the old deadline starts its own complete two-second idle interval.
        let _ = store.save_if_due(Instant::now());
        store.report_status(&mut observed.status);
        if stopping && self.stopping_at.is_none() {
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
    use std::fs::{self, DirBuilder};
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn temporary_config() -> PathBuf {
        let directory = PathBuf::from("/var/tmp").join(format!(
            "cosmos-control-save-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        directory.join("profile.json")
    }

    fn desired(name: &str) -> Profile {
        let mut profile = Profile::default();
        profile.buses[0].name = name.into();
        profile
    }

    fn exchange(
        command: Command,
        profile: Option<Profile>,
        store: &mut Store,
        observed: &mut Observed,
        stopping: &mut bool,
        reconciled: &mut Vec<Profile>,
    ) -> Response {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(SERVER_TIMEOUT)).unwrap();
        let request = control::Request {
            version: VERSION,
            command,
            profile,
        };
        peer.write_all(&control::encode(&request, MAX_REQUEST_BYTES).unwrap())
            .unwrap();
        let now = Instant::now();
        let mut client = Client {
            stream,
            input: Vec::new(),
            output: None,
            written: 0,
            deadline: now + SERVER_TIMEOUT,
            stopping: false,
        };
        client
            .poll(
                observed,
                store,
                &mut |profile, _| {
                    reconciled.push(profile.clone());
                    BackendStatus::default()
                },
                now,
                stopping,
            )
            .unwrap();
        let mut response = String::new();
        BufReader::new(peer).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn update_reconciles_immediately_but_flush_and_stop_wait_for_durability() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        store.apply(Profile::default()).unwrap();
        let mut observed = Observed::new();
        let mut stopping = false;
        let mut reconciled = Vec::new();
        let first = desired("Live alias");
        let response = exchange(
            Command::Update,
            Some(first.clone()),
            &mut store,
            &mut observed,
            &mut stopping,
            &mut reconciled,
        );
        assert!(matches!(
            response,
            Response::Applied {
                accepted: true,
                status: control::Status {
                    config_pending: true,
                    ..
                },
                ..
            }
        ));
        assert_eq!(reconciled, vec![first.clone()]);
        assert_eq!(
            crate::persistence::read_profile(&config).unwrap(),
            Profile::default()
        );
        let response = exchange(
            Command::Flush,
            None,
            &mut store,
            &mut observed,
            &mut stopping,
            &mut reconciled,
        );
        assert!(matches!(
            response,
            Response::Applied {
                accepted: true,
                status: control::Status {
                    config_pending: false,
                    ..
                },
                ..
            }
        ));
        assert_eq!(crate::persistence::read_profile(&config).unwrap(), first);
        let latest = desired("Final alias");
        exchange(
            Command::Update,
            Some(latest.clone()),
            &mut store,
            &mut observed,
            &mut stopping,
            &mut reconciled,
        );
        assert!(matches!(
            exchange(
                Command::Stop,
                None,
                &mut store,
                &mut observed,
                &mut stopping,
                &mut reconciled,
            ),
            Response::Stopping { stopping: true, .. }
        ));
        assert!(stopping);
        assert_eq!(crate::persistence::read_profile(&config).unwrap(), latest);
        for command in [Command::Update, Command::Apply] {
            assert!(matches!(
                exchange(
                    command,
                    Some(desired("Too late")),
                    &mut store,
                    &mut observed,
                    &mut stopping,
                    &mut reconciled,
                ),
                Response::Error { .. }
            ));
            assert_eq!(store.profile, latest);
        }
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_flush_and_stop_leave_the_engine_open_for_recovery() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        let latest = desired("Unsaved alias");
        store.update(latest.clone(), Instant::now()).unwrap();
        fs::create_dir(&config).unwrap();
        let mut observed = Observed::new();
        let mut stopping = false;
        let mut reconciled = Vec::new();
        for command in [Command::Flush, Command::Stop] {
            assert!(matches!(
                exchange(
                    command,
                    None,
                    &mut store,
                    &mut observed,
                    &mut stopping,
                    &mut reconciled,
                ),
                Response::Error { .. }
            ));
            assert!(!stopping);
            assert!(observed.status.config_pending);
            assert!(observed.status.config_error.is_some());
            assert_eq!(store.profile, latest);
        }
        fs::remove_dir(&config).unwrap();
        assert!(matches!(
            exchange(
                Command::Stop,
                None,
                &mut store,
                &mut observed,
                &mut stopping,
                &mut reconciled,
            ),
            Response::Stopping { stopping: true, .. }
        ));
        assert!(!observed.status.config_pending);
        assert_eq!(crate::persistence::read_profile(&config).unwrap(), latest);
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

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
