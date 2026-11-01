// SPDX-License-Identifier: MPL-2.0

mod discovery;
mod meter;
mod persistence;
mod runtime;
mod server;

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use control::socket::OwnedSocket;
use cosmos_control::{self as control, Command, Phase, Response};
use discovery::{Connection, Observed};
use pipewire as pw;

const TICK: Duration = Duration::from_millis(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(2);
const USAGE: &str = "Cosmos Audio Mixer engine\n\
Usage: cosmos-engine run [--socket PATH] [--remote NAME] [--config PATH]\n\
       cosmos-engine status|list|profile|stop [--socket PATH]\n\
       cosmos-engine apply FILE [--socket PATH]\n\
run stays in the foreground, independent of control clients. Profiles are explicit:\n\
empty defaults create no endpoints or routes and never capture audio.\n";

struct Options {
    command: String,
    socket: PathBuf,
    create_parent: bool,
    remote: Option<String>,
    config: Option<PathBuf>,
    import: Option<PathBuf>,
}

fn next_value(arguments: &mut impl Iterator<Item = OsString>, flag: &str) -> io::Result<OsString> {
    arguments
        .next()
        .ok_or_else(|| control::invalid(format!("{flag} requires a value")))
}

fn options() -> io::Result<Option<Options>> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(control::invalid(USAGE));
    };
    if command == "--help" || command == "-h" {
        print!("{USAGE}");
        return Ok(None);
    }
    let command = command
        .into_string()
        .map_err(|_| control::invalid("command must be UTF-8"))?;
    if !matches!(
        command.as_str(),
        "run" | "status" | "list" | "profile" | "apply" | "stop"
    ) {
        return Err(control::invalid(format!(
            "unknown command: {command}\n{USAGE}"
        )));
    }
    let mut socket = None;
    let mut remote = None;
    let mut config = None;
    let mut import = None;
    while let Some(argument) = arguments.next() {
        if argument == "--socket" && socket.is_none() {
            socket = Some(PathBuf::from(next_value(&mut arguments, "--socket")?));
        } else if argument == "--remote" && remote.is_none() && command == "run" {
            remote = Some(
                next_value(&mut arguments, "--remote")?
                    .into_string()
                    .map_err(|_| control::invalid("remote name must be UTF-8"))?,
            );
        } else if argument == "--config" && config.is_none() && command == "run" {
            config = Some(PathBuf::from(next_value(&mut arguments, "--config")?));
        } else if command == "apply"
            && import.is_none()
            && !argument.as_encoded_bytes().starts_with(b"--")
        {
            import = Some(PathBuf::from(argument));
        } else {
            return Err(control::invalid(format!(
                "unexpected option: {}",
                argument.to_string_lossy()
            )));
        }
    }
    if command == "run" && remote.is_none() {
        remote = match std::env::var("PIPEWIRE_REMOTE") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err(control::invalid("PIPEWIRE_REMOTE must be UTF-8")),
        };
    }
    if let Some(remote) = &remote
        && (remote.is_empty() || remote.len() > 512 || remote.contains('\0'))
    {
        return Err(control::invalid(
            "remote name must be nonempty, at most 512 bytes, and contain no NUL",
        ));
    }
    if command == "apply" && import.is_none() {
        return Err(control::invalid("apply requires a profile FILE"));
    }
    if command == "run" && config.is_none() {
        config = Some(persistence::default_path()?);
    }
    let create_parent = socket.is_none();
    Ok(Some(Options {
        command,
        socket: match socket {
            Some(path) => path,
            None => control::default_socket()?,
        },
        create_parent,
        remote,
        config,
        import,
    }))
}

fn disconnected(observed: &Rc<RefCell<Observed>>, message: String) {
    let mut observed = observed.borrow_mut();
    observed.reset();
    observed.status.phase = Phase::Disconnected;
    observed.status.last_error = Some(message);
}

fn run(options: Options) -> io::Result<()> {
    // Acquire process ownership before making any PipeWire connection.
    let socket = OwnedSocket::bind(&options.socket, options.create_parent)?;
    let mut server = server::Server::new(socket);
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(io::Error::other)?;
    let quit = Rc::new(Cell::new(false));
    let quit_int = quit.clone();
    let _interrupt = main_loop
        .loop_()
        .add_signal_local(pw::loop_::Signal::INT, move || quit_int.set(true));
    let quit_term = quit.clone();
    let _terminate = main_loop
        .loop_()
        .add_signal_local(pw::loop_::Signal::TERM, move || quit_term.set(true));
    let observed = Rc::new(RefCell::new(Observed::new()));
    let mut store = persistence::Store::load(
        options
            .config
            .ok_or_else(|| control::invalid("run requires a config path"))?,
    );
    observed.borrow_mut().status.config_error = store.error.clone();
    let mut runtime = runtime::Runtime::new(&main_loop, options.remote.as_deref());
    let mut connection = None;
    let mut connected_at = Instant::now();
    let mut next_attempt = Instant::now();
    let mut retry = RETRY_MIN;
    while !quit.get() {
        if connection.is_none() && Instant::now() >= next_attempt {
            {
                let mut observed = observed.borrow_mut();
                observed.reset();
                observed.status.generation =
                    observed.status.generation.checked_add(1).ok_or_else(|| {
                        io::Error::other("PipeWire connection generation exhausted")
                    })?;
                observed.status.phase = Phase::Connecting;
                observed.status.retry_ms = 0;
            }
            match Connection::connect(&main_loop, options.remote.as_deref(), &observed) {
                Ok(value) => {
                    connection = Some(value);
                    connected_at = Instant::now();
                }
                Err(error) => {
                    disconnected(&observed, format!("cannot connect to PipeWire: {error}"));
                    next_attempt = Instant::now() + retry;
                    retry = (retry * 2).min(RETRY_MAX);
                }
            }
        }
        let result = main_loop.loop_().iterate(pw::loop_::Timeout::Finite(TICK));
        if result < 0 && result != -libc::EINTR {
            return Err(io::Error::from_raw_os_error(-result));
        }
        if connection.is_some() {
            let failure = {
                let mut observed = observed.borrow_mut();
                observed.connection_error.take().or_else(|| {
                    (observed.status.phase == Phase::Connecting
                        && connected_at.elapsed() >= CONNECT_TIMEOUT)
                        .then(|| "PipeWire connection handshake timed out".into())
                })
            };
            if let Some(message) = failure {
                // Drop every old listener/proxy before making old IDs unavailable and retrying.
                connection = None;
                disconnected(&observed, message);
                next_attempt = Instant::now() + retry;
                retry = (retry * 2).min(RETRY_MAX);
            } else if observed.borrow().status.phase == Phase::Connected {
                retry = RETRY_MIN;
            }
        }
        {
            let mut observed = observed.borrow_mut();
            observed.status.retry_ms = if connection.is_none() {
                next_attempt
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64
            } else {
                0
            };
            observed.status.backend =
                runtime.reconcile(&store.profile, connection.as_ref(), &observed);
            if !server.tick(&mut observed, &mut store, |profile, observed| {
                runtime.reconcile(profile, connection.as_ref(), observed)
            })? {
                break;
            }
        }
    }
    drop(runtime);
    drop(connection);
    Ok(())
}

fn execute() -> io::Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    if options.command == "run" {
        return run(options);
    }
    let response = if options.command == "apply" {
        let path = options
            .import
            .ok_or_else(|| control::invalid("apply requires a profile FILE"))?;
        control::apply(&options.socket, persistence::read_profile(&path)?)?
    } else {
        let command = match options.command.as_str() {
            "status" => Command::Status,
            "list" => Command::List,
            "profile" => Command::GetProfile,
            "stop" => Command::Stop,
            _ => return Err(control::invalid("invalid client command")),
        };
        control::request(&options.socket, command)?
    };
    if let Response::Error { error, .. } = &response {
        return Err(io::Error::other(error.clone()));
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response).map_err(io::Error::other)?;
    stdout.write_all(b"\n")
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cosmos-engine: {error}");
            ExitCode::FAILURE
        }
    }
}
