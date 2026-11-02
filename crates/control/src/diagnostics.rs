// SPDX-License-Identifier: MPL-2.0
//! Small terminal-only diagnostics. Never call this module from an audio callback.

use std::fmt::{self, Write as _};
use std::io::{self, IsTerminal, Write};
use std::str::FromStr;
use std::sync::OnceLock;

use crate::{Phase, RuntimeState, Status};

const MAX_MESSAGE_BYTES: usize = 2048;
const MAX_ERROR_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    #[default]
    Warn,
    Info,
    Debug,
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        })
    }
}

impl FromStr for Level {
    type Err = io::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(crate::invalid(
                "--log-level must be error, warn, info, or debug",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogOptions {
    pub level: Level,
    pub stdout: bool,
    /// Preserve explicitly requested diagnostics even when redirected to a file or pipe.
    pub explicit: bool,
}

impl LogOptions {
    pub fn allows(self, level: Level) -> bool {
        level <= self.level
    }

    /// Inherit a terminal by default, or the chosen stream when explicitly requested.
    /// Never pipe a detached child's diagnostics through a GUI-owned reader.
    pub fn child_attached(self) -> bool {
        self.explicit || self.stdout || io::stderr().is_terminal()
    }
}

static SETTINGS: OnceLock<(LogOptions, &'static str)> = OnceLock::new();

/// Configure once at the executable boundary, before starting worker threads.
pub fn init(options: LogOptions, component: &'static str) {
    let _ = SETTINGS.set((options, component));
}

struct Line {
    bytes: [u8; MAX_MESSAGE_BYTES + 1],
    len: usize,
}

impl fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            let length = character.len_utf8();
            if length > MAX_MESSAGE_BYTES.saturating_sub(self.len) {
                break;
            }
            character.encode_utf8(&mut self.bytes[self.len..self.len + length]);
            self.len += length;
        }
        Ok(())
    }
}

fn emit(
    output: &mut impl Write,
    options: LogOptions,
    component: &str,
    level: Level,
    message: fmt::Arguments<'_>,
) -> io::Result<()> {
    if !options.allows(level) {
        return Ok(());
    }
    let mut line = Line {
        bytes: [0; MAX_MESSAGE_BYTES + 1],
        len: 0,
    };
    let _ = write!(&mut line, "[{level} {component}] {message}");
    line.bytes[line.len] = b'\n';
    output.write_all(&line.bytes[..=line.len])
}

/// Best-effort diagnostics: a closed terminal/pipe must never panic or stop audio.
/// Pass only operational summaries and errors, never profiles or device properties.
pub fn write(level: Level, message: fmt::Arguments<'_>) {
    let (options, component) = SETTINGS
        .get()
        .copied()
        .unwrap_or((LogOptions::default(), "cosmos"));
    if !options.allows(level) {
        return;
    }
    if options.stdout {
        let _ = emit(&mut io::stdout().lock(), options, component, level, message);
    } else {
        let _ = emit(&mut io::stderr().lock(), options, component, level, message);
    }
}

fn bounded(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One bounded current error, not an event queue. Repeated polls allocate nothing.
#[derive(Debug, Default)]
pub struct Change {
    previous: Option<String>,
}

impl Change {
    fn transition(&mut self, message: Option<&str>) -> Option<Level> {
        let message = message.map(|text| bounded(text, MAX_ERROR_BYTES));
        if self.previous.as_deref() == message {
            return None;
        }
        self.previous = message.map(str::to_owned);
        Some(if message.is_some() {
            Level::Warn
        } else {
            Level::Info
        })
    }

    /// Emit new/changed failures at warn and recovery at info, once per transition.
    /// Keep a separate Change for each independently changing error field.
    pub fn error(&mut self, scope: impl fmt::Display, message: Option<&str>) {
        if let Some(level) = self.transition(message) {
            match self.previous.as_deref() {
                Some(message) => write(level, format_args!("{scope}: {message}")),
                None => write(level, format_args!("{scope}: recovered")),
            }
        }
    }
}

#[derive(Debug, Default)]
struct EndpointDiagnostics {
    id: String,
    state: Option<RuntimeState>,
    error: Change,
    input_meter: Change,
    output_meter: Change,
}

impl EndpointDiagnostics {
    fn state(&mut self, kind: &str, state: RuntimeState) {
        if self.state != Some(state) {
            write(Level::Debug, format_args!("{kind} {}: {state:?}", self.id));
            self.state = Some(state);
        }
    }
}

fn endpoint<'a>(
    entries: &'a mut Vec<EndpointDiagnostics>,
    id: &str,
) -> &'a mut EndpointDiagnostics {
    let index = match entries.iter().position(|entry| entry.id == id) {
        Some(index) => index,
        None => {
            entries.push(EndpointDiagnostics {
                id: id.to_owned(),
                ..EndpointDiagnostics::default()
            });
            entries.len() - 1
        }
    };
    &mut entries[index]
}

/// Deduplicated engine health diagnostics, usable by both engine and GUI.
#[derive(Debug, Default)]
pub struct StatusChanges {
    phase: Option<Phase>,
    backend_ready: Option<bool>,
    connection: Change,
    config: Change,
    discovery: Change,
    control: Change,
    backend: Change,
    device_profile: Change,
    buses: Vec<EndpointDiagnostics>,
    routes: Vec<EndpointDiagnostics>,
}

impl StatusChanges {
    /// Call only on a control/UI thread, never inside an audio callback.
    pub fn observe(&mut self, status: &Status) {
        if self.phase != Some(status.phase) {
            write(
                if status.phase == Phase::Connected {
                    Level::Info
                } else {
                    Level::Debug
                },
                format_args!(
                    "PipeWire {:?} (generation {})",
                    status.phase, status.generation
                ),
            );
            self.phase = Some(status.phase);
        }
        // Connecting clears last_error temporarily: do not re-log the same retry failure.
        if status.last_error.is_some() || status.phase == Phase::Connected {
            self.connection
                .error("PipeWire connection", status.last_error.as_deref());
        }
        self.config
            .error("configuration", status.config_error.as_deref());
        self.discovery
            .error("discovery", status.discovery_error.as_deref());
        self.control
            .error("control", status.last_control_error.as_deref());
        self.backend
            .error("backend", status.backend.error.as_deref());
        self.device_profile.error(
            "native device profile",
            status
                .device_profile_change
                .as_ref()
                .and_then(|change| change.error.as_deref()),
        );
        if self.backend_ready != Some(status.backend.ready) {
            write(
                Level::Info,
                format_args!(
                    "backend {}",
                    if status.backend.ready {
                        "ready"
                    } else {
                        "not ready"
                    }
                ),
            );
            self.backend_ready = Some(status.backend.ready);
        }
        self.buses.retain(|entry| {
            status
                .backend
                .buses
                .iter()
                .take(crate::model::MAX_BUSES)
                .any(|bus| bus.id == entry.id)
        });
        for bus in status.backend.buses.iter().take(crate::model::MAX_BUSES) {
            let entry = endpoint(&mut self.buses, &bus.id);
            entry.state("bus", bus.state);
            entry
                .error
                .error(format_args!("bus {}", bus.id), bus.error.as_deref());
            entry.input_meter.error(
                format_args!("bus {} input meter", bus.id),
                bus.input_meter
                    .as_ref()
                    .and_then(|meter| meter.error.as_deref()),
            );
            entry.output_meter.error(
                format_args!("bus {} output meter", bus.id),
                bus.output_meter
                    .as_ref()
                    .and_then(|meter| meter.error.as_deref()),
            );
        }
        self.routes.retain(|entry| {
            status
                .backend
                .routes
                .iter()
                .take(crate::model::MAX_ROUTES)
                .any(|route| route.id == entry.id)
        });
        for route in status.backend.routes.iter().take(crate::model::MAX_ROUTES) {
            let entry = endpoint(&mut self.routes, &route.id);
            entry.state("route", route.state);
            entry
                .error
                .error(format_args!("route {}", route.id), route.error.as_deref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_supported_levels() {
        for (text, level) in [
            ("error", Level::Error),
            ("warn", Level::Warn),
            ("info", Level::Info),
            ("debug", Level::Debug),
        ] {
            assert_eq!(text.parse::<Level>().unwrap(), level);
        }
        for text in ["trace", "WARNING", "", "Info"] {
            assert!(text.parse::<Level>().is_err());
        }
    }

    #[test]
    fn warning_default_filters_info_but_preserves_failures() {
        let mut output = Vec::new();
        for level in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
            emit(
                &mut output,
                LogOptions::default(),
                "test",
                level,
                format_args!("message"),
            )
            .unwrap();
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[warn test] message\n[error test] message\n"
        );
    }

    #[test]
    fn each_selected_level_filters_less_severe_diagnostics() {
        for threshold in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
            for level in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
                let mut output = Vec::new();
                emit(
                    &mut output,
                    LogOptions {
                        level: threshold,
                        ..LogOptions::default()
                    },
                    "test",
                    level,
                    format_args!("message"),
                )
                .unwrap();
                assert_eq!(!output.is_empty(), level <= threshold);
            }
        }
    }

    #[test]
    fn unchanged_errors_are_silent_until_change_or_recovery() {
        let mut change = Change::default();
        let transitions = [
            None,
            Some("offline"),
            Some("offline"),
            Some("timeout"),
            None,
            None,
        ]
        .map(|message| change.transition(message));
        assert_eq!(
            transitions,
            [
                None,
                Some(Level::Warn),
                None,
                Some(Level::Warn),
                Some(Level::Info),
                None
            ]
        );
    }

    #[test]
    fn oversized_multibyte_errors_are_bounded_and_deduplicated() {
        let mut change = Change::default();
        let error = "é".repeat(MAX_ERROR_BYTES);
        assert_eq!(change.transition(Some(&error)), Some(Level::Warn));
        assert_eq!(change.previous.as_ref().unwrap().len(), MAX_ERROR_BYTES);
        assert_eq!(change.transition(Some(&error)), None);
    }

    #[test]
    fn terminal_records_are_single_line_and_bounded() {
        let mut output = Vec::new();
        emit(
            &mut output,
            LogOptions::default(),
            "test",
            Level::Error,
            format_args!("bad\n\x1b[31m{}", "é".repeat(MAX_MESSAGE_BYTES)),
        )
        .unwrap();
        assert!(output.len() <= MAX_MESSAGE_BYTES + 1);
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(!output.contains(&0x1b));
    }

    #[test]
    fn explicit_diagnostics_attach_even_without_a_terminal() {
        assert!(
            LogOptions {
                explicit: true,
                ..LogOptions::default()
            }
            .child_attached()
        );
        assert!(
            LogOptions {
                stdout: true,
                ..LogOptions::default()
            }
            .child_attached()
        );
    }

    fn status() -> Status {
        Status {
            pid: 1,
            phase: Phase::Disconnected,
            generation: 0,
            last_error: None,
            retry_ms: 0,
            discovery_error: None,
            last_control_error: None,
            backend: crate::BackendStatus::default(),
            config_error: None,
            config_pending: false,
            device_profile_change: None,
        }
    }

    #[test]
    fn reconnect_attempt_preserves_error_until_actual_recovery() {
        let mut logs = StatusChanges::default();
        let mut status = status();
        status.last_error = Some("offline".into());
        logs.observe(&status);
        status.phase = Phase::Connecting;
        status.last_error = None;
        logs.observe(&status);
        assert_eq!(logs.connection.previous.as_deref(), Some("offline"));
        status.phase = Phase::Connected;
        logs.observe(&status);
        assert_eq!(logs.connection.previous, None);
    }

    #[test]
    fn removed_routes_do_not_leave_an_unbounded_diagnostic_history() {
        let mut logs = StatusChanges::default();
        let mut status = status();
        for index in 0..crate::model::MAX_ROUTES * 2 {
            status.backend.routes = vec![crate::RouteStatus {
                id: format!("route-{index}"),
                state: RuntimeState::Pending,
                error: None,
            }];
            logs.observe(&status);
            assert_eq!(logs.routes.len(), 1);
        }
        status.backend.routes.clear();
        logs.observe(&status);
        assert!(logs.routes.is_empty());
    }

    #[test]
    fn closed_diagnostic_stream_returns_broken_pipe_without_panicking() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = emit(
            &mut Closed,
            LogOptions::default(),
            "test",
            Level::Error,
            format_args!("failed"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
