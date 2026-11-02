// SPDX-License-Identifier: MPL-2.0
use std::os::unix::process::CommandExt;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cosmos_control::{Command, Node, Profile, Response, Status};

use crate::Options;
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub profile: Profile,
    pub status: Status,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug)]
pub enum Reply {
    Snapshot(Arc<Snapshot>),
    Applied(Box<Status>),
    Stopped,
}

#[derive(Clone, Debug)]
pub enum Failure {
    Transport(String),
    Conflict(String),
    Rejected(String),
    Uncertain(String),
}

impl Failure {
    fn message(&self) -> &str {
        match self {
            Self::Transport(error)
            | Self::Conflict(error)
            | Self::Rejected(error)
            | Self::Uncertain(error) => error,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Operation {
    Poll,
    Apply {
        baseline: Arc<Profile>,
        desired: Arc<Profile>,
    },
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterSave {
    Close,
    Stop,
    Reload,
}

pub enum Next {
    Request(Operation),
    Close,
}

/// One worker owns the wire. `profile` is the latest valid intent, `baseline` is
/// confirmed engine state, and an Apply owns an immutable copy of what it sent.
/// Failed writes stop automatic writes; only a confirmed uncertain success resumes them.
pub struct Control {
    pub profile: Profile,
    pub text_edits: crate::model::TextEdits,
    pub baseline: Option<Arc<Profile>>,
    pub status: Option<Status>,
    pub nodes: Vec<Node>,
    pub error: Option<String>,
    pub connection_error: Option<String>,
    pub notice: String,
    pub stopped: bool,
    in_flight: Option<Operation>,
    uncertain: Option<Arc<Profile>>,
    blocked: bool,
    conflict: bool,
    poll_due: bool,
    applied_since_poll: bool,
    after_save: Option<AfterSave>,
    start_pending: bool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            text_edits: crate::model::TextEdits::default(),
            baseline: None,
            status: None,
            nodes: Vec::new(),
            error: None,
            connection_error: None,
            notice: "Connecting to the engine…".into(),
            stopped: false,
            in_flight: None,
            uncertain: None,
            blocked: false,
            conflict: false,
            poll_due: true,
            applied_since_poll: false,
            after_save: None,
            start_pending: false,
        }
    }
}

impl Control {
    pub fn dirty(&self) -> bool {
        self.baseline
            .as_deref()
            .is_some_and(|base| base != &self.profile)
    }

    pub fn pending(&self) -> bool {
        self.dirty()
            || matches!(self.in_flight, Some(Operation::Apply { .. }))
            || self.uncertain.is_some()
    }

    pub fn action_busy(&self) -> bool {
        self.after_save.is_some()
            || self.start_pending
            || matches!(self.in_flight, Some(Operation::Start | Operation::Stop))
    }

    pub fn retry_available(&self) -> bool {
        self.blocked && !self.conflict && self.uncertain.is_none()
    }

    pub fn save_blocked(&self) -> bool {
        self.blocked
    }

    pub fn tick(&mut self) {
        self.poll_due = true;
    }

    pub fn start(&mut self) {
        self.start_pending = true;
    }

    pub fn after_save(&mut self, action: AfterSave) -> Result<(), String> {
        self.after_save = None;
        self.text_edits.commit_names(&mut self.profile)?;
        if self.pending() && self.save_unavailable() {
            return Err("The engine is stopped or offline. Start or reconnect it to save pending changes, or explicitly discard them with Reload.".into());
        }
        if self.blocked {
            return Err("Pending changes could not be saved. Retry, or reload and explicitly discard them before continuing.".into());
        }
        self.after_save = Some(action);
        Ok(())
    }

    pub fn save_unavailable(&self) -> bool {
        self.stopped || self.status.is_none() || self.connection_error.is_some()
    }

    pub fn cancel_action(&mut self) {
        self.after_save = None;
    }

    pub fn retry(&mut self) {
        if self.retry_available() {
            self.blocked = false;
            self.error = None;
            self.poll_due = true;
            // Recheck the baseline before an explicit retry, including after reconnect.
            self.applied_since_poll = true;
        }
    }

    /// Explicit recovery after the user confirms discarding unsent edits.
    /// Never forget an outstanding or uncertain write: its completion must be known first.
    pub fn discard_and_reload(&mut self) -> bool {
        if self.in_flight.is_some() || self.uncertain.is_some() {
            return false;
        }
        if let Some(baseline) = &self.baseline {
            self.profile = (**baseline).clone();
        }
        self.text_edits.clear();
        self.blocked = false;
        self.conflict = false;
        self.error = None;
        self.after_save = Some(AfterSave::Reload);
        self.stopped = false;
        true
    }

    pub fn can_discard(&self) -> bool {
        self.in_flight.is_none() && self.uncertain.is_none()
    }

    pub fn next(&mut self) -> Option<Next> {
        if self.after_save.is_some() && self.pending() && self.save_unavailable() {
            self.after_save = None;
            self.error = Some("The engine is stopped or offline. Start or reconnect it to save pending changes, or explicitly discard them with Reload.".into());
        }
        if self.text_edits.pending() {
            self.after_save = None;
        }
        if self.in_flight.is_some() {
            return None;
        }
        let can_apply = self.dirty()
            && !self.blocked
            && self.uncertain.is_none()
            && self.status.is_some()
            && self.connection_error.is_none()
            && !self.stopped;
        let operation = if self.start_pending {
            self.start_pending = false;
            Operation::Start
        } else if !self.pending() && !self.blocked && self.after_save.is_some() {
            // Final actions must not starve behind polls that take longer than one tick.
            match self.after_save.take() {
                Some(AfterSave::Close) => return Some(Next::Close),
                Some(AfterSave::Stop) => Operation::Stop,
                Some(AfterSave::Reload) => {
                    self.poll_due = false;
                    self.applied_since_poll = false;
                    Operation::Poll
                }
                None => return None,
            }
        } else if !self.stopped && self.poll_due && (self.applied_since_poll || !can_apply) {
            self.poll_due = false;
            self.applied_since_poll = false;
            Operation::Poll
        } else if can_apply {
            let baseline = self.baseline.as_ref()?.clone();
            let desired = Arc::new(self.profile.clone());
            self.applied_since_poll = true;
            Operation::Apply { baseline, desired }
        } else {
            return None;
        };
        self.in_flight = Some(operation.clone());
        Some(Next::Request(operation))
    }

    /// Returns true only when an idle editor adopted an external profile.
    pub fn finish(&mut self, result: Result<Reply, Failure>) -> bool {
        let Some(operation) = self.in_flight.take() else {
            return false;
        };
        let mut replaced = false;
        match result {
            Ok(Reply::Snapshot(snapshot)) => {
                let snapshot = Arc::unwrap_or_clone(snapshot);
                let observed = Arc::new(snapshot.profile);
                if self.uncertain.as_deref() == Some(observed.as_ref()) || self.profile == *observed
                {
                    self.baseline = Some(observed);
                    self.uncertain = None;
                    self.blocked = false;
                    self.conflict = false;
                    self.error = None;
                } else if self.baseline.as_deref() == Some(observed.as_ref()) {
                    // A failed Apply was not accepted. Keep its error and require an
                    // explicit retry, rather than repeatedly writing on every poll.
                    self.uncertain = None;
                } else if !self.pending() && !self.blocked {
                    self.profile = (*observed).clone();
                    self.baseline = Some(observed);
                    self.text_edits.prune(&self.profile);
                    replaced = true;
                } else {
                    self.uncertain = None;
                    self.blocked = true;
                    self.conflict = true;
                    self.error = Some(
                        "Another client changed the profile. Reload to use its settings; your pending changes are kept until you confirm discarding them.".into(),
                    );
                }
                self.status = Some(snapshot.status);
                self.nodes = snapshot.nodes;
                self.connection_error = None;
                self.notice = "Changes are heard and saved automatically.".into();
                self.stopped = false;
            }
            Ok(Reply::Applied(status)) => {
                if let Operation::Apply { desired, .. } = operation {
                    self.baseline = Some(desired);
                    self.uncertain = None;
                    self.blocked = false;
                    self.conflict = false;
                    self.error = None;
                    self.connection_error = None;
                    self.notice = "Changes are heard and saved automatically.".into();
                    self.status = Some(*status);
                }
            }
            Ok(Reply::Stopped) => {
                self.stopped = true;
                self.status = None;
                self.nodes.clear();
                self.notice = "Engine stopped. Audio endpoints are unavailable.".into();
            }
            Err(failure) => {
                if let Operation::Apply { desired, .. } = operation {
                    self.blocked = true;
                    self.conflict = matches!(failure, Failure::Conflict(_));
                    if matches!(failure, Failure::Uncertain(_)) {
                        self.uncertain = Some(desired);
                        self.poll_due = true;
                    }
                    self.error = Some(format!("Changes could not be saved: {}", failure.message()));
                }
                if matches!(failure, Failure::Transport(_) | Failure::Uncertain(_)) {
                    self.connection_error = Some(failure.message().to_owned());
                    self.status = None;
                    self.nodes.clear();
                    self.notice = "Engine unavailable. Your changes are kept.".into();
                } else if self.error.is_none() {
                    self.error = Some(failure.message().to_owned());
                }
                // Do not close, stop, or reload following a failed flush.
                self.after_save = None;
            }
        }
        replaced
    }
}

fn apply_failure(error: std::io::Error) -> Failure {
    // The Linux Unix-stream client's path/connect failures precede request writes.
    // All other failures, including write/read errors and timeouts, remain uncertain.
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            Failure::Transport(error.to_string())
        }
        _ => Failure::Uncertain(error.to_string()),
    }
}

fn profile(options: &Options) -> Result<Profile, Failure> {
    match cosmos_control::request(&options.socket, Command::GetProfile)
        .map_err(|e| Failure::Transport(e.to_string()))?
    {
        Response::Profile { profile, .. } => {
            profile
                .validate()
                .map_err(|e| Failure::Rejected(e.to_string()))?;
            Ok(profile)
        }
        Response::Error { error, .. } => Err(Failure::Rejected(error)),
        _ => Err(Failure::Rejected("Unexpected profile response".into())),
    }
}

fn snapshot(options: &Options) -> Result<Reply, Failure> {
    match cosmos_control::request(&options.socket, Command::List)
        .map_err(|e| Failure::Transport(e.to_string()))?
    {
        Response::Snapshot { status, nodes, .. } => Ok(Reply::Snapshot(Arc::new(Snapshot {
            profile: profile(options)?,
            status,
            nodes,
        }))),
        Response::Error { error, .. } => Err(Failure::Rejected(error)),
        _ => Err(Failure::Rejected("Unexpected engine response".into())),
    }
}

/// Called only on a blocking worker. The UI permits one operation at a time.
pub fn execute(options: Options, operation: Operation) -> Result<Reply, Failure> {
    match operation {
        Operation::Poll => snapshot(&options),
        Operation::Apply { baseline, desired } => {
            if profile(&options)? != *baseline {
                return Err(Failure::Conflict(
                    "The profile changed in another client. Reload before changing its settings."
                        .into(),
                ));
            }
            match cosmos_control::apply(&options.socket, (*desired).clone())
                .map_err(apply_failure)?
            {
                Response::Applied {
                    accepted: true,
                    status,
                    ..
                } => Ok(Reply::Applied(Box::new(status))),
                Response::Error { error, .. } => Err(Failure::Rejected(error)),
                _ => Err(Failure::Uncertain(
                    "Engine did not acknowledge the profile".into(),
                )),
            }
        }
        Operation::Stop => match cosmos_control::request(&options.socket, Command::Stop)
            .map_err(|e| Failure::Transport(e.to_string()))?
        {
            Response::Stopping { stopping: true, .. } => Ok(Reply::Stopped),
            Response::Error { error, .. } => Err(Failure::Rejected(error)),
            _ => Err(Failure::Rejected("Engine did not acknowledge stop".into())),
        },
        Operation::Start => start(&options),
    }
}

fn start(options: &Options) -> Result<Reply, Failure> {
    match cosmos_control::request(&options.socket, Command::Status) {
        Ok(Response::Snapshot { .. }) => return snapshot(options),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(Failure::Transport(error.to_string())),
        _ => {
            return Err(Failure::Rejected(
                "The existing engine returned an invalid response".into(),
            ));
        }
    }
    let executable = std::env::current_exe()
        .map_err(|e| Failure::Transport(e.to_string()))?
        .with_file_name("cosmos-engine");
    let mut command = Process::new(executable);
    command
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    if options.explicit_socket {
        command.arg("--socket").arg(&options.socket);
    }
    if let Some(remote) = &options.remote {
        command.arg("--remote").arg(remote);
    }
    if let Some(config) = &options.config {
        command.arg("--config").arg(config);
    }
    let mut child = command.spawn().map_err(|e| {
        Failure::Transport(format!(
            "Cannot start the sibling cosmos-engine executable: {e}"
        ))
    })?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let result = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| Failure::Transport(e.to_string()))?
        {
            return Err(Failure::Transport(format!(
                "Engine exited with {status}. Check its terminal diagnostics."
            )));
        }
        match snapshot(options) {
            Ok(reply) => break Ok(reply),
            Err(error) if Instant::now() >= deadline => {
                break Err(Failure::Transport(format!(
                    "Engine started but is not reachable: {}",
                    error.message()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    // Reap an engine that exits while its launcher is still open; never kill it on window close.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{self, Parameter};
    use cosmos_control::{BackendStatus, Channels, Phase};

    fn status(generation: u64) -> Status {
        Status {
            pid: 42,
            phase: Phase::Connected,
            generation,
            last_error: None,
            retry_ms: 0,
            discovery_error: None,
            last_control_error: None,
            backend: BackendStatus::default(),
            config_error: None,
        }
    }

    fn observed(profile: Profile, generation: u64) -> Result<Reply, Failure> {
        Ok(Reply::Snapshot(Arc::new(Snapshot {
            profile,
            status: status(generation),
            nodes: Vec::new(),
        })))
    }

    fn connected() -> Control {
        let mut control = Control::default();
        let mut profile = Profile::default();
        profile
            .buses
            .push(model::new_bus(&profile, Channels::Stereo).unwrap());
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(profile, 1));
        control
    }

    fn gain(control: &mut Control, value: f32) {
        model::set_parameter(&mut control.profile.buses[0], None, Parameter::Gain, value).unwrap();
    }

    fn take_apply(control: &mut Control) -> (Arc<Profile>, Arc<Profile>) {
        match control.next() {
            Some(Next::Request(Operation::Apply { baseline, desired })) => (baseline, desired),
            _ => panic!("expected a pending live update"),
        }
    }

    fn acknowledge(control: &mut Control) {
        control.finish(Ok(Reply::Applied(Box::new(status(1)))));
    }

    #[test]
    fn rapid_updates_coalesce_without_overlapping_or_overwriting_newer_intent() {
        let mut control = connected();
        gain(&mut control, -1.0);
        let (_, first) = take_apply(&mut control);
        for value in [-2.0, -3.0, -4.0, -5.0] {
            gain(&mut control, value);
            assert!(control.next().is_none());
        }
        acknowledge(&mut control);
        assert_eq!(control.baseline.as_deref(), Some(first.as_ref()));
        assert_eq!(control.profile.buses[0].gain_db, -5.0);
        let (baseline, latest) = take_apply(&mut control);
        assert_eq!(baseline, first);
        assert_eq!(latest.buses[0].gain_db, -5.0);
        acknowledge(&mut control);
        assert!(!control.pending());
        assert!(control.next().is_none());
    }

    #[test]
    fn reverting_to_old_baseline_during_a_write_still_sends_the_revert() {
        let mut control = connected();
        gain(&mut control, -6.0);
        take_apply(&mut control);
        gain(&mut control, 0.0);
        assert!(control.pending());
        control.after_save(AfterSave::Close).unwrap();
        assert!(control.next().is_none());
        acknowledge(&mut control);
        let (baseline, desired) = take_apply(&mut control);
        assert_eq!(baseline.buses[0].gain_db, -6.0);
        assert_eq!(desired.buses[0].gain_db, 0.0);
        acknowledge(&mut control);
        assert!(matches!(control.next(), Some(Next::Close)));
    }

    #[test]
    fn poll_started_before_an_edit_does_not_replace_it() {
        let mut control = connected();
        let old = control.profile.clone();
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        gain(&mut control, -9.0);
        control.finish(observed(old, 2));
        assert_eq!(control.profile.buses[0].gain_db, -9.0);
        assert_eq!(control.status.as_ref().unwrap().generation, 2);
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].gain_db, -9.0);
    }

    #[test]
    fn due_polls_get_a_turn_during_continuous_edits_and_acknowledgements() {
        let mut control = connected();
        for generation in 2..8 {
            gain(&mut control, -(generation as f32));
            let (_, sent) = take_apply(&mut control);
            control.tick();
            gain(&mut control, -(generation as f32) - 0.5);
            acknowledge(&mut control);
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Poll))
            ));
            gain(&mut control, -(generation as f32) - 0.75);
            control.finish(observed((*sent).clone(), generation));
            assert_eq!(control.status.as_ref().unwrap().generation, generation);
            assert_eq!(
                control.profile.buses[0].gain_db,
                -(generation as f32) - 0.75
            );
        }
        take_apply(&mut control);
        acknowledge(&mut control);
        assert!(!control.pending());
    }

    #[test]
    fn external_poll_change_conflicts_with_newer_local_edit_without_overwriting_either() {
        let mut control = connected();
        let baseline = control.baseline.clone();
        let mut external = control.profile.clone();
        external.buses[0].muted = true;
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        gain(&mut control, -3.0);
        control.finish(observed(external, 2));
        assert_eq!(control.baseline, baseline);
        assert_eq!(control.profile.buses[0].gain_db, -3.0);
        assert!(!control.profile.buses[0].muted);
        assert!(control.save_blocked());
        assert!(!control.retry_available());
        assert!(control.error.is_some());
        assert!(control.next().is_none());
    }

    #[test]
    fn preflight_conflict_never_retries_or_closes_and_requires_confirmed_reload() {
        let mut control = connected();
        gain(&mut control, -3.0);
        take_apply(&mut control);
        control.after_save(AfterSave::Close).unwrap();
        control.finish(Err(Failure::Conflict("another client wrote".into())));
        assert!(!control.retry_available());
        assert!(!control.action_busy());
        for _ in 0..3 {
            control.tick();
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Poll))
            ));
            control.finish(observed((**control.baseline.as_ref().unwrap()).clone(), 2));
            assert!(control.next().is_none());
        }
        assert_eq!(control.profile.buses[0].gain_db, -3.0);
        assert!(control.discard_and_reload());
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
    }

    #[test]
    fn rejected_write_keeps_newer_edits_and_needs_one_explicit_retry() {
        let mut control = connected();
        gain(&mut control, -4.0);
        take_apply(&mut control);
        gain(&mut control, -8.0);
        control.finish(Err(Failure::Rejected("disk is read-only".into())));
        assert!(control.retry_available());
        for _ in 0..3 {
            control.tick();
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Poll))
            ));
            control.finish(observed((**control.baseline.as_ref().unwrap()).clone(), 1));
            assert!(control.error.is_some());
            assert!(control.next().is_none());
        }
        control.retry();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed((**control.baseline.as_ref().unwrap()).clone(), 1));
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].gain_db, -8.0);
    }

    #[test]
    fn disconnect_during_poll_preserves_edits_and_checks_reconnected_baseline() {
        let mut control = connected();
        let baseline = control.profile.clone();
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        gain(&mut control, -5.0);
        control.finish(Err(Failure::Transport("disconnected".into())));
        assert!(control.status.is_none());
        assert!(control.connection_error.is_some());
        assert!(control.next().is_none());
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(baseline, 2));
        assert!(control.connection_error.is_none());
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].gain_db, -5.0);
    }

    #[test]
    fn uncertain_success_is_reconciled_against_sent_not_latest_profile() {
        let mut control = connected();
        gain(&mut control, -2.0);
        let (_, sent) = take_apply(&mut control);
        gain(&mut control, -7.0);
        control.finish(Err(Failure::Uncertain("reply was lost".into())));
        assert!(!control.can_discard());
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed((*sent).clone(), 2));
        assert!(!control.save_blocked());
        assert!(control.error.is_none());
        let (baseline, desired) = take_apply(&mut control);
        assert_eq!(baseline, sent);
        assert_eq!(desired.buses[0].gain_db, -7.0);
    }

    #[test]
    fn uncertain_failure_does_not_loop_on_old_profile_or_transport_failure() {
        let mut control = connected();
        let baseline = control.profile.clone();
        gain(&mut control, -2.0);
        take_apply(&mut control);
        control.finish(Err(Failure::Uncertain("reply was lost".into())));
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(Err(Failure::Transport("still offline".into())));
        assert!(control.next().is_none());
        assert!(!control.discard_and_reload());
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(baseline, 2));
        assert!(control.retry_available());
        assert!(control.error.is_some());
        assert!(control.next().is_none());
    }

    #[test]
    fn close_stop_and_reload_wait_for_final_edits_and_abort_after_failure() {
        for action in [AfterSave::Close, AfterSave::Stop, AfterSave::Reload] {
            let mut control = connected();
            gain(&mut control, -1.0);
            take_apply(&mut control);
            control.after_save(action).unwrap();
            gain(&mut control, -8.0);
            assert!(control.next().is_none());
            assert!(!control.discard_and_reload());
            acknowledge(&mut control);
            let (_, desired) = take_apply(&mut control);
            assert_eq!(desired.buses[0].gain_db, -8.0);
            acknowledge(&mut control);
            assert!(matches!(
                (action, control.next()),
                (AfterSave::Close, Some(Next::Close))
                    | (AfterSave::Stop, Some(Next::Request(Operation::Stop)))
                    | (AfterSave::Reload, Some(Next::Request(Operation::Poll)))
            ));

            let mut control = connected();
            gain(&mut control, -1.0);
            take_apply(&mut control);
            control.after_save(action).unwrap();
            control.finish(Err(Failure::Rejected("cannot save".into())));
            assert!(control.pending());
            assert!(!control.action_busy());
            assert!(control.next().is_none());
        }
    }

    #[test]
    fn cancelling_close_does_not_cancel_the_live_write() {
        let mut control = connected();
        gain(&mut control, -6.0);
        take_apply(&mut control);
        control.after_save(AfterSave::Close).unwrap();
        control.cancel_action();
        acknowledge(&mut control);
        assert_eq!(control.baseline.as_ref().unwrap().buses[0].gain_db, -6.0);
        assert!(control.next().is_none());
    }

    #[test]
    fn invalid_name_text_remains_visible_without_blocking_live_gain_updates() {
        let mut control = connected();
        let previous = control.profile.buses[0].name.clone();
        control
            .text_edits
            .rename(&mut control.profile, 0, String::new());
        assert_eq!(control.text_edits.name(&control.profile.buses[0]), "");
        assert!(control.text_edits.error().is_some());
        gain(&mut control, -12.0);
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].name, previous);
        assert_eq!(desired.buses[0].gain_db, -12.0);
        assert!(desired.validate().is_ok());
        acknowledge(&mut control);
        assert!(control.text_edits.pending());
        control
            .text_edits
            .rename(&mut control.profile, 0, "Voice".into());
        assert!(control.next().is_none());
        control
            .text_edits
            .commit_name(&mut control.profile, 0)
            .unwrap();
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].name, "Voice");
        assert!(!control.text_edits.pending());
    }

    #[test]
    fn invalid_text_entered_during_final_write_cancels_close_without_losing_audio() {
        for action in [AfterSave::Close, AfterSave::Stop, AfterSave::Reload] {
            let mut control = connected();
            gain(&mut control, -6.0);
            take_apply(&mut control);
            control.after_save(action).unwrap();
            control
                .text_edits
                .rename(&mut control.profile, 0, String::new());
            assert!(control.next().is_none());
            acknowledge(&mut control);
            assert!(control.next().is_none());
            assert_eq!(control.baseline.as_ref().unwrap().buses[0].gain_db, -6.0);
            assert_eq!(control.text_edits.name(&control.profile.buses[0]), "");
        }
    }

    #[test]
    fn final_actions_are_not_starved_by_a_poll_that_outlasts_the_tick_interval() {
        for action in [AfterSave::Close, AfterSave::Stop, AfterSave::Reload] {
            let mut control = connected();
            let profile = control.profile.clone();
            control.tick();
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Poll))
            ));
            control.after_save(action).unwrap();
            control.tick();
            control.finish(observed(profile, 2));
            assert!(matches!(
                (action, control.next()),
                (AfterSave::Close, Some(Next::Close))
                    | (AfterSave::Stop, Some(Next::Request(Operation::Stop)))
                    | (AfterSave::Reload, Some(Next::Request(Operation::Poll)))
            ));
        }
    }

    #[test]
    fn reconnect_with_an_external_profile_preserves_pending_edits_as_a_conflict() {
        let mut control = connected();
        let mut external = control.profile.clone();
        external.buses[0].muted = true;
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        gain(&mut control, -10.0);
        control.finish(Err(Failure::Transport("engine exited".into())));
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(external, 2));
        assert!(control.save_blocked());
        assert!(!control.retry_available());
        assert_eq!(control.profile.buses[0].gain_db, -10.0);
        assert!(!control.profile.buses[0].muted);
        assert!(control.next().is_none());
    }

    #[test]
    fn stopped_edits_refuse_final_actions_without_locking_start_then_flush() {
        for action in [AfterSave::Close, AfterSave::Reload, AfterSave::Stop] {
            let mut control = connected();
            let baseline = control.profile.clone();
            control.after_save(AfterSave::Stop).unwrap();
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Stop))
            ));
            control.finish(Ok(Reply::Stopped));
            gain(&mut control, -8.0);
            assert!(
                control
                    .after_save(action)
                    .unwrap_err()
                    .contains("stopped or offline")
            );
            assert!(!control.action_busy());
            assert!(control.next().is_none());
            assert!(control.can_discard());
            control.start();
            assert!(matches!(
                control.next(),
                Some(Next::Request(Operation::Start))
            ));
            control.finish(observed(baseline, 2));
            let (_, desired) = take_apply(&mut control);
            assert_eq!(desired.buses[0].gain_db, -8.0);
            acknowledge(&mut control);
            control.after_save(action).unwrap();
            assert!(matches!(
                (action, control.next()),
                (AfterSave::Close, Some(Next::Close))
                    | (AfterSave::Stop, Some(Next::Request(Operation::Stop)))
                    | (AfterSave::Reload, Some(Next::Request(Operation::Poll)))
            ));
        }
    }

    #[test]
    fn valid_name_typing_preserves_endpoints_while_audio_is_live_until_enter() {
        let mut control = connected();
        let original = control.profile.buses[0].name.clone();
        for name in ["V", "Vo", "Voi", "Voice"] {
            control
                .text_edits
                .rename(&mut control.profile, 0, name.into());
            assert_eq!(control.profile.buses[0].name, original);
            assert_eq!(control.text_edits.name(&control.profile.buses[0]), name);
            assert!(control.text_edits.error().is_none());
            assert!(control.next().is_none());
        }
        gain(&mut control, -3.0);
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].name, original);
        assert_eq!(desired.buses[0].gain_db, -3.0);
        acknowledge(&mut control);
        assert!(control.text_edits.pending());
        control
            .text_edits
            .commit_name(&mut control.profile, 0)
            .unwrap();
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].name, "Voice");
        assert!(!control.text_edits.pending());
        acknowledge(&mut control);
        assert!(control.next().is_none());
    }

    #[test]
    fn final_save_commits_valid_names_but_invalid_names_never_block_live_audio() {
        for action in [AfterSave::Close, AfterSave::Stop, AfterSave::Reload] {
            let mut control = connected();
            control
                .text_edits
                .rename(&mut control.profile, 0, "Voice".into());
            control.after_save(action).unwrap();
            assert!(!control.text_edits.pending());
            let (_, desired) = take_apply(&mut control);
            assert_eq!(desired.buses[0].name, "Voice");
            acknowledge(&mut control);
            assert!(matches!(
                (action, control.next()),
                (AfterSave::Close, Some(Next::Close))
                    | (AfterSave::Stop, Some(Next::Request(Operation::Stop)))
                    | (AfterSave::Reload, Some(Next::Request(Operation::Poll)))
            ));

            let mut control = connected();
            control
                .text_edits
                .rename(&mut control.profile, 0, String::new());
            assert!(control.after_save(action).is_err());
            assert!(
                control
                    .text_edits
                    .commit_name(&mut control.profile, 0)
                    .is_err()
            );
            gain(&mut control, -4.0);
            let (_, desired) = take_apply(&mut control);
            assert_eq!(desired.buses[0].gain_db, -4.0);
            assert!(!desired.buses[0].name.is_empty());
            assert_eq!(control.text_edits.name(&control.profile.buses[0]), "");
            assert!(!control.action_busy());
        }
    }

    #[test]
    fn offline_valid_rename_is_kept_for_start_without_queueing_a_final_action() {
        let mut control = connected();
        let baseline = control.profile.clone();
        control.stopped = true;
        control.status = None;
        control
            .text_edits
            .rename(&mut control.profile, 0, "Voice".into());
        let error = control.after_save(AfterSave::Close).unwrap_err();
        assert!(error.contains("stopped or offline"));
        assert!(!control.text_edits.pending());
        assert_eq!(control.profile.buses[0].name, "Voice");
        assert!(!control.action_busy());
        control.start();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Start))
        ));
        control.finish(observed(baseline, 2));
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].name, "Voice");
    }

    #[test]
    fn preflight_transport_failure_requires_explicit_retry_after_reconnect() {
        let mut control = connected();
        let baseline = control.profile.clone();
        gain(&mut control, -1.0);
        take_apply(&mut control);
        gain(&mut control, -9.0);
        control.finish(Err(Failure::Transport(
            "socket disappeared before send".into(),
        )));
        assert!(control.retry_available());
        assert!(control.can_discard());
        assert!(control.next().is_none());
        control.tick();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(baseline.clone(), 2));
        assert!(control.save_blocked());
        assert!(control.next().is_none());
        control.retry();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        control.finish(observed(baseline, 2));
        let (_, desired) = take_apply(&mut control);
        assert_eq!(desired.buses[0].gain_db, -9.0);
    }

    #[test]
    fn confirmed_discard_after_stop_polls_and_adopts_the_remote_profile() {
        let mut control = connected();
        let mut remote = control.profile.clone();
        remote.buses[0].name = "Remote".into();
        remote.buses[0].muted = true;
        control.stopped = true;
        control.status = None;
        gain(&mut control, -9.0);
        control
            .text_edits
            .rename(&mut control.profile, 0, "Local".into());
        assert!(control.after_save(AfterSave::Reload).is_err());
        assert!(control.discard_and_reload());
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Poll))
        ));
        assert!(control.finish(observed(remote.clone(), 3)));
        assert_eq!(control.profile, remote);
        assert_eq!(control.baseline.as_deref(), Some(&remote));
        assert!(!control.text_edits.pending());
        assert!(!control.pending());
        assert!(!control.action_busy());
        assert!(!control.save_blocked());
    }

    #[test]
    fn start_failure_clears_lifecycle_and_keeps_pending_edits_and_recovery_usable() {
        let mut control = connected();
        control.stopped = true;
        control.status = None;
        gain(&mut control, -7.0);
        assert!(control.after_save(AfterSave::Close).is_err());
        control.start();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Start))
        ));
        assert!(control.after_save(AfterSave::Reload).is_err());
        control.finish(Err(Failure::Transport(
            "engine executable not found".into(),
        )));
        assert!(!control.action_busy());
        assert!(control.connection_error.is_some());
        assert_eq!(control.profile.buses[0].gain_db, -7.0);
        assert!(control.next().is_none());
        assert!(control.can_discard());
        control.start();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Start))
        ));
    }

    #[test]
    fn edits_during_stop_do_not_leave_a_queued_close_stuck_after_stop_finishes() {
        let mut control = connected();
        control.after_save(AfterSave::Stop).unwrap();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Stop))
        ));
        gain(&mut control, -5.0);
        control.after_save(AfterSave::Close).unwrap();
        control.finish(Ok(Reply::Stopped));
        assert!(control.next().is_none());
        assert!(!control.action_busy());
        assert!(control.error.is_some());
        assert_eq!(control.profile.buses[0].gain_db, -5.0);
        control.start();
        assert!(matches!(
            control.next(),
            Some(Next::Request(Operation::Start))
        ));
    }

    #[test]
    fn only_clear_socket_path_and_connect_failures_are_classified_as_unsent() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::NotFound, ErrorKind::ConnectionRefused] {
            assert!(matches!(
                apply_failure(Error::from(kind)),
                Failure::Transport(_)
            ));
        }
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
            ErrorKind::WriteZero,
            ErrorKind::UnexpectedEof,
            ErrorKind::InvalidData,
            ErrorKind::PermissionDenied,
        ] {
            assert!(matches!(
                apply_failure(Error::from(kind)),
                Failure::Uncertain(_)
            ));
        }
    }
}
