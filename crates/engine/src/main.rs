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

use control::diagnostics::{self, Level, LogOptions};
use control::socket::OwnedSocket;
use cosmos_control::{self as control, Command, Phase, Response};
use discovery::{Connection, Observed};
use pipewire as pw;

const TICK: Duration = Duration::from_millis(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(2);
const USAGE: &str = "Cosmos Audio Mixer engine\n\
Usage: cosmos-engine run [--socket PATH] [--remote NAME] [--config PATH] [--log-level LEVEL] [--log-stdout]\n\
       cosmos-engine status|list|profile|flush|stop [--socket PATH] [--log-level LEVEL]\n\
       cosmos-engine apply|update FILE [--socket PATH] [--log-level LEVEL]\n\
LEVEL is error|warn|info|debug (default warn). Diagnostics use stderr unless\n\
--log-stdout is explicitly selected for run; JSON client commands reject it.\n\
run stays in the foreground, independent of control clients. Profiles are explicit:\n\
empty defaults create no endpoints or routes and never capture audio.\n\
update changes live audio now and saves after two idle seconds; flush saves now.\n";

#[derive(Debug)]
struct Options {
    command: String,
    socket: PathBuf,
    create_parent: bool,
    remote: Option<String>,
    config: Option<PathBuf>,
    import: Option<PathBuf>,
    diagnostics: LogOptions,
}

fn next_value(arguments: &mut impl Iterator<Item = OsString>, flag: &str) -> io::Result<OsString> {
    arguments
        .next()
        .ok_or_else(|| control::invalid(format!("{flag} requires a value")))
}

fn options() -> io::Result<Option<Options>> {
    options_from(std::env::args_os().skip(1))
}

fn options_from(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Option<Options>> {
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
        "run" | "status" | "list" | "profile" | "apply" | "update" | "flush" | "stop"
    ) {
        return Err(control::invalid(format!(
            "unknown command: {command}\n{USAGE}"
        )));
    }
    let mut socket = None;
    let mut remote = None;
    let mut config = None;
    let mut import = None;
    let mut log_level = None;
    let mut log_stdout = false;
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
        } else if argument == "--log-level" && log_level.is_none() {
            log_level = Some(
                next_value(&mut arguments, "--log-level")?
                    .into_string()
                    .map_err(|_| control::invalid("--log-level must be UTF-8"))?
                    .parse::<Level>()?,
            );
        } else if argument == "--log-stdout" && !log_stdout {
            log_stdout = true;
        } else if matches!(command.as_str(), "apply" | "update")
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
    if log_stdout && command != "run" {
        return Err(control::invalid(
            "--log-stdout is only valid for run; client stdout is reserved for JSON",
        ));
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
    if matches!(command.as_str(), "apply" | "update") && import.is_none() {
        return Err(control::invalid(format!(
            "{command} requires a profile FILE"
        )));
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
        diagnostics: LogOptions {
            level: log_level.unwrap_or_default(),
            stdout: log_stdout,
            explicit: log_level.is_some() || log_stdout,
        },
    }))
}

fn disconnected(observed: &Rc<RefCell<Observed>>, message: String) {
    let mut observed = observed.borrow_mut();
    observed.reset();
    observed.status.phase = Phase::Disconnected;
    observed.status.last_error = Some(message);
}

fn signal_shutdown(
    quit: &Cell<bool>,
    store: &mut persistence::Store,
    status: &mut control::Status,
) -> bool {
    if !quit.replace(false) {
        return false;
    }
    let result = store.flush_on_shutdown();
    store.report_status(status);
    match result {
        Ok(()) => true,
        Err(error) => {
            diagnostics::write(
                Level::Warn,
                format_args!(
                    "cannot save profile before shutdown; engine remains running: {error}"
                ),
            );
            false
        }
    }
}

fn save_before_error_exit(store: &mut persistence::Store, error: io::Error) -> io::Error {
    if let Err(save_error) = store.flush_on_shutdown() {
        diagnostics::write(
            Level::Error,
            format_args!("Cannot save pending settings before engine failure: {save_error}"),
        );
    }
    error
}

fn run(options: Options) -> io::Result<()> {
    // Acquire process ownership before making any PipeWire connection.
    diagnostics::write(Level::Info, format_args!("engine starting"));
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
    store.report_status(&mut observed.borrow_mut().status);
    let mut logs = diagnostics::StatusChanges::default();
    logs.observe(&observed.borrow().status);
    let mut runtime = runtime::Runtime::new(&main_loop, options.remote.as_deref());
    let mut connection = None;
    let mut connected_at = Instant::now();
    let mut next_attempt = Instant::now();
    let mut retry = RETRY_MIN;
    loop {
        if connection.is_none() && Instant::now() >= next_attempt {
            {
                let mut observed = observed.borrow_mut();
                observed.reset();
                observed.status.generation = observed
                    .status
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("PipeWire connection generation exhausted"))
                    .map_err(|error| save_before_error_exit(&mut store, error))?;
                observed.status.phase = Phase::Connecting;
                observed.status.retry_ms = 0;
            }
            diagnostics::write(Level::Debug, format_args!("PipeWire connection attempt"));
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
            return Err(save_before_error_exit(
                &mut store,
                io::Error::from_raw_os_error(-result),
            ));
        }
        if signal_shutdown(&quit, &mut store, &mut observed.borrow_mut().status) {
            break;
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
            let keep_running = server
                .tick(
                    &mut observed,
                    &mut store,
                    connection.as_ref(),
                    |profile, observed| runtime.reconcile(profile, connection.as_ref(), observed),
                )
                .map_err(|error| save_before_error_exit(&mut store, error))?;
            logs.observe(&observed.status);
            if !keep_running {
                diagnostics::write(
                    Level::Info,
                    format_args!("engine stop requested by control client"),
                );
                break;
            }
        }
    }
    drop(runtime);
    drop(connection);
    diagnostics::write(Level::Info, format_args!("engine stopped"));
    Ok(())
}

fn execute() -> io::Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    diagnostics::init(options.diagnostics, "cosmos-engine");
    diagnostics::write(Level::Debug, format_args!("executing {}", options.command));
    if options.command == "run" {
        return run(options);
    }
    let response = if matches!(options.command.as_str(), "apply" | "update") {
        let path = options.import.ok_or_else(|| {
            control::invalid(format!("{} requires a profile FILE", options.command))
        })?;
        let profile = persistence::read_profile(&path)?;
        if options.command == "update" {
            control::update(&options.socket, profile)?
        } else {
            control::apply(&options.socket, profile)?
        }
    } else {
        let command = match options.command.as_str() {
            "status" => Command::Status,
            "list" => Command::List,
            "profile" => Command::GetProfile,
            "stop" => Command::Stop,
            "flush" => Command::Flush,
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
            diagnostics::write(Level::Error, format_args!("{error}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> io::Result<Option<Options>> {
        options_from(arguments.iter().map(OsString::from))
    }

    #[test]
    fn client_diagnostics_default_to_warning_on_stderr() {
        let options = parse(&["status", "--socket", "/unused/control.sock"])
            .unwrap()
            .unwrap();
        assert_eq!(options.diagnostics, LogOptions::default());
    }

    #[test]
    fn run_accepts_explicit_stdout_diagnostics() {
        let options = parse(&[
            "run",
            "--socket",
            "/unused/control.sock",
            "--config",
            "/unused/profile.json",
            "--remote",
            "private-test",
            "--log-level",
            "info",
            "--log-stdout",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            options.diagnostics,
            LogOptions {
                level: Level::Info,
                stdout: true,
                explicit: true
            }
        );
    }

    #[test]
    fn every_json_client_rejects_stdout_diagnostics() {
        for command in [
            "status", "list", "profile", "stop", "apply", "update", "flush",
        ] {
            let error =
                parse(&[command, "--socket", "/unused/control.sock", "--log-stdout"]).unwrap_err();
            assert!(error.to_string().contains("stdout is reserved for JSON"));
        }
    }

    #[test]
    fn json_clients_allow_level_selection_without_changing_destination() {
        for command in ["status", "list", "profile", "stop", "flush"] {
            let options = parse(&[
                command,
                "--socket",
                "/unused/control.sock",
                "--log-level",
                "debug",
            ])
            .unwrap()
            .unwrap();
            assert_eq!(
                options.diagnostics,
                LogOptions {
                    level: Level::Debug,
                    stdout: false,
                    explicit: true
                }
            );
        }
    }

    #[test]
    fn profile_import_commands_require_one_file_and_flush_accepts_no_file() {
        for command in ["apply", "update"] {
            assert!(parse(&[command, "--socket", "/unused/control.sock"]).is_err());
            let options = parse(&[
                command,
                "/unused/requested-profile.json",
                "--socket",
                "/unused/control.sock",
            ])
            .unwrap()
            .unwrap();
            assert_eq!(
                options.import,
                Some(PathBuf::from("/unused/requested-profile.json"))
            );
            assert!(
                parse(&[
                    command,
                    "/unused/first.json",
                    "/unused/second.json",
                    "--socket",
                    "/unused/control.sock",
                ])
                .is_err()
            );
        }
        assert!(
            parse(&[
                "flush",
                "/unused/requested-profile.json",
                "--socket",
                "/unused/control.sock",
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_missing_unknown_and_duplicate_diagnostic_flags() {
        for arguments in [
            vec!["status", "--log-level"],
            vec!["status", "--log-level", "trace"],
            vec!["status", "--log-level", "warn", "--log-level", "debug"],
            vec!["run", "--log-stdout", "--log-stdout"],
        ] {
            assert!(parse(&arguments).is_err());
        }
    }
}
