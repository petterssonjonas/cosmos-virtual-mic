// SPDX-License-Identifier: MPL-2.0
mod app;
mod color;
mod connection;
mod model;
mod tray;
mod view;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use cosmos_control::diagnostics::{self, Level, LogOptions};

const USAGE: &str = "Cosmos Audio Mixer\n\
Usage: cosmos-audio-mixer [--standalone] [--socket PATH] [--remote NAME] [--config PATH] [--log-level LEVEL] [--log-stdout]\n\n\
LEVEL is error|warn|info|debug (default warn). Diagnostics use stderr unless\n\
--log-stdout is explicitly selected. The installed engine service logs to the journal.\n\
Use journalctl --user -u cosmos-audio-mixer.service -f to follow its logs.\n\
Start/Stop controls the installed user service. --standalone or a custom socket,\n\
remote, or config instead starts a sibling engine with these diagnostic options.\n\
Closing the window does not stop audio. No recording or automatic default-device changes.\n";

#[derive(Clone, Debug)]
pub struct Options {
    socket: PathBuf,
    explicit_socket: bool,
    remote: Option<String>,
    config: Option<PathBuf>,
    standalone: bool,
    pub diagnostics: LogOptions,
}

impl Options {
    pub fn uses_service(&self) -> bool {
        !self.standalone && !self.explicit_socket && self.remote.is_none() && self.config.is_none()
    }
}

impl cosmic::app::CosmicFlags for Options {
    type SubCommand = String;
    type Args = Vec<String>;
}

fn next_value(arguments: &mut impl Iterator<Item = OsString>, flag: &str) -> io::Result<OsString> {
    arguments
        .next()
        .ok_or_else(|| cosmos_control::invalid(format!("{flag} requires a value")))
}

fn options_from(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Option<Options>> {
    let mut socket = None;
    let mut remote = None;
    let mut config = None;
    let mut log_level = None;
    let mut log_stdout = false;
    let mut standalone = false;
    while let Some(argument) = arguments.next() {
        if argument == "--help" || argument == "-h" {
            print!("{USAGE}");
            return Ok(None);
        } else if argument == "--socket" && socket.is_none() {
            socket = Some(PathBuf::from(next_value(&mut arguments, "--socket")?));
        } else if argument == "--remote" && remote.is_none() {
            remote = Some(
                next_value(&mut arguments, "--remote")?
                    .into_string()
                    .map_err(|_| cosmos_control::invalid("remote name must be UTF-8"))?,
            );
        } else if argument == "--config" && config.is_none() {
            config = Some(PathBuf::from(next_value(&mut arguments, "--config")?));
        } else if argument == "--log-level" && log_level.is_none() {
            log_level = Some(
                next_value(&mut arguments, "--log-level")?
                    .into_string()
                    .map_err(|_| cosmos_control::invalid("--log-level must be UTF-8"))?
                    .parse::<Level>()?,
            );
        } else if argument == "--log-stdout" && !log_stdout {
            log_stdout = true;
        } else if argument == "--standalone" && !standalone {
            standalone = true;
        } else {
            return Err(cosmos_control::invalid(format!(
                "unexpected option: {}",
                argument.to_string_lossy()
            )));
        }
    }
    Ok(Some(Options {
        explicit_socket: socket.is_some(),
        socket: socket.map_or_else(cosmos_control::default_socket, Ok)?,
        remote,
        config,
        standalone,
        diagnostics: LogOptions {
            level: log_level.unwrap_or_default(),
            stdout: log_stdout,
            explicit: log_level.is_some() || log_stdout,
        },
    }))
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let Some(options) = options_from(std::env::args_os().skip(1))? else {
        return Ok(());
    };
    diagnostics::init(options.diagnostics, "cosmos-audio-mixer");
    diagnostics::write(Level::Info, format_args!("mixer starting"));
    let settings = cosmic::app::Settings::default()
        .size(cosmic::iced::Size::new(1220.0, 820.0))
        .exit_on_close(false);
    if options.uses_service() {
        cosmic::app::run_single_instance::<app::Mixer>(settings, options)?;
    } else {
        cosmic::app::run::<app::Mixer>(settings, options)?;
    }
    diagnostics::write(
        Level::Info,
        format_args!("mixer closed; engine lifetime is independent"),
    );
    Ok(())
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
    fn diagnostics_default_to_warning_on_stderr() {
        let options = parse(&["--socket", "/unused/control.sock"])
            .unwrap()
            .unwrap();
        assert_eq!(options.diagnostics, LogOptions::default());
    }

    #[test]
    fn parses_explicit_diagnostic_destination_and_level() {
        let options = parse(&[
            "--socket",
            "/unused/control.sock",
            "--log-stdout",
            "--log-level",
            "debug",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            options.diagnostics,
            LogOptions {
                level: Level::Debug,
                stdout: true,
                explicit: true
            }
        );
    }

    #[test]
    fn rejects_missing_unknown_and_duplicate_logging_flags() {
        for arguments in [
            vec!["--log-level"],
            vec!["--log-level", "trace"],
            vec!["--log-level", "info", "--log-level", "warn"],
            vec!["--log-stdout", "--log-stdout"],
        ] {
            assert!(parse(&arguments).is_err());
        }
    }
}
