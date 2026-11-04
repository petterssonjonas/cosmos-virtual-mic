# Cosmos Virtual Mic — implementation handover

## Resume here

Run project commands inside `cosmos` and source `dev-env.sh` before Rust commands. The implementation worktree is `/home/jp/Projects/.worktrees/cosmos-audio-mixer/mic-only`, branch `mic-only`, based on `7a5d2ad`. The previous full mixer is preserved locally and remotely as the frozen `full-mixer` snapshot at that commit. Promote the microphone app to `development` through the reviewed PR and passing checks; do not force-replace history. Preserve the original checkout's unrelated `.gitignore` change.

## Microphone-only scope — 2026-11-04

The owner authorized the gray-card-only correction, restoring the original window size, supported floating placement, preservation of the full mixer as `full-mixer`, promotion of the microphone app to `development`, and an updated optimized build/SCP transfer with the `_workstation` key. This does not authorize a merge to `main`, a published Release, remote installation, or login-startup changes.

- Application/executable: `cosmos-virtual-mic`; engine executable: `cosmos-virtual-mic-engine`; user unit: `cosmos-virtual-mic.service`; desktop ID: `io.github.petterssonjonas.CosmosVirtualMic`.
- The gray channel-1 card is the entire visible window: no frosted shell, toolkit header bar, outer diagnostics, or footer. Its original 286×760 logical-pixel size is fixed. Start/Stop, a draggable gap, and top-right Close are integrated in the card. Controls and effects dialogs scroll. Close saves and exits only the frontend. Native min/max size requests match; COSMIC floating follows its fixed-size classification, but actual workstation placement remains unverified.
- The engine enforces one visible `channel-1`, zero/one Hardware-to-channel-1 route, and no device settings. The default includes the strip's eight effects and no automatic input. Known mono/stereo microphone layouts are matched explicitly. Unknown or unsupported selections show an error without changing the profile.
- Only the processed `cosmos.virtual-mic.channel-1.output` is a public audio device. The input is an internal capture stream. Missing, disabled, ambiguous, or failed input produces silence. A foreign input link also silences output with an explicit error; removing it recovers the selected microphone.
- Protocol version 4 removes system device-profile mutation and its native callbacks. Read-only discovery remains. Saved profile version 1 keeps the collection shape but rejects original multi-channel mixer profiles rather than truncating them.
- The new socket and profile use `cosmos-virtual-mic` directories. The installer uses separate executable/service/desktop names and leaves the original mixer installation and settings intact. No original profile is imported automatically.
- Two independent source reviews covered engine/persistence/installer safety and frontend behavior. Findings about foreign input, early unsaved edits, route diagnostics, and unknown/offline state were corrected. Both reviewers reported no remaining blockers.

Build and source-test commands, service behavior, and workstation acceptance are in `DEVELOPMENT.md`. Use `--standalone` for an uninstalled source build; normal installed launches control the new user service.

Physical microphone behavior, real systemd start/enable/login restore, Bluetooth, clocks, latency, launcher focus, screen readers, and long-running stability remain workstation checks. Local builds use the toolbox ABI; historical portable mixer bundles are not Cosmos Virtual Mic builds.

### Local verification

The final card-only candidate passed formatting, locked builds, 114 Rust tests, Clippy with warnings denied, private lifecycle/save-failure checks, mono/stereo policy-startup/restart checks, and installer regressions. The policy run exposed a failed unbound route link incorrectly retained as a discovery error. The native helper classifies only unbound Link proxies; their existing runtime feedback and bounded retry remain intact, while real discovery and transport errors stay visible. The regression test preserves those distinctions. Final policy evidence: `/var/tmp/cosmos-policy-23_9qke_`.

A temporary real-widget driver passed input selection, Start/Stop, effects overlays, mute/gain, name draft/final-save Close, and engine lifetime checks. Temporary source and dependencies were removed. The actual Wayland app rendered with live synthetic levels; the engine stayed running after frontend exit, then was stopped explicitly.

Evidence: `/var/tmp/cosmos-backend-1foejlfw`, `/var/tmp/cosmos-policy-905yn3za`, `/var/tmp/cosmos-mic-widgets-68ez2gp_`, and `/var/tmp/cosmos-mic-ui-7qra27k1`. Final native capture: `wayland-screenshot-2026-11-04_16-41-59.png` in the last directory. Private processes are stopped; no host audio or installed service was changed.

The card-only correction passed a temporary real-widget workflow at 286×760: header drag event, microphone selection, Start/Stop, EQ/dynamics modal interaction, lower-controls scrolling and limiter addition, mute/gain, and final name save on Close. Native rendering and Wayland min/max/geometry requests were observed. Evidence: `/var/tmp/cosmos-card-ui-fwfnnkwo/`, with `wayland-screenshot-2026-11-04_21-23-26.png`. Private processes are stopped; temporary drivers/dependencies are removed.

### Card-only workstation candidate

The optimized copy is prepared at `/home/jp/Projects/cosmos-audio-mixer-test/card-only-workstation-7pgyyq8y/`, targeting `jp@192.168.0.10:/home/jp/Downloads/cosmos-virtual-mic-card.fYsi9k/` with the workstation key. Verify the recorded source revision and checksums, not the earlier shell-containing copy below. `DEVELOPMENT.md` has the owner-run update and launch commands. Do not install, enable startup, open a desktop window, or start host audio through SSH as part of the transfer.

### Earlier private workstation delivery — before the card-only correction

The earlier optimized x86-64 build from the uncommitted `mic-only` source was copied with `~/.ssh/id_ed25519_workstation` to `jp@192.168.0.10:/home/jp/Downloads/cosmos-virtual-mic-test.rzgbAt/`. It still has the outer toolkit shell. The workstation reports hostname `Workstation`, Fedora 45 and glibc 2.44, matching the build toolbox baseline. Local copy: `/home/jp/Projects/cosmos-audio-mixer-test/mic-only-workstation-7xo6tity/`.

The directory includes both executables, the Bash installer and data templates, compatibility metadata, licence, build provenance and checksums. Remote checksums, both executable help commands and dynamic-library resolution passed. The optimized engine also passed private mono/stereo audio, gate and foreign-input silence/recovery checks; evidence is `/var/tmp/cosmos-mic-release-smoke-r5z0ord0`. No installation, service operation, GUI launch or audio connection was run on the workstation. `DEVELOPMENT.md` has the owner-run installation commands.

## Historical mixer work

The sections below record earlier mixer decisions, authorizations, and evidence. They are not current branch instructions or verification of Cosmos Virtual Mic.

## Service, persistence, and panel candidate — 2026-11-02 (historical)

- Current authorization: create a new PR to `development` and provide compiled workstation test bundles. **Do not merge until the owner tests and approves.** Earlier merge authorization below is history, not current permission. Do not add package publishing or distribution CI/CD yet.
- Draft [PR #3](https://github.com/petterssonjonas/cosmos-audio-mixer/pull/3) is open and unmerged.
- The default frontend controls `cosmos-audio-mixer.service`. `--standalone` or explicit socket/remote/config options use direct sibling-engine control. The engine remains independent of the frontend and restores its complete saved mixer profile.
- Controls remain live. Protocol-3 Update changes audio now and debounces complete-profile persistence for two seconds. Flush, frontend close/hide/quit, and Stop require a durable save. Signals and fatal exits attempt a final save. Failed saves retain live state and an error; forced termination or power loss can still lose unsaved RAM.
- GUI values snap to useful steps, including 0.5 dB color movement and whole-Hertz EQ frequency. Decimal controls save concise values. Imported settings, DSP samples and measured levels retain their precision.
- External profile changes from the same connected engine are adopted even while its disk save is pending. Unsent edits and accepted-but-unsaved settings after engine loss remain protected. Regression coverage includes engine restart both with and without an observed transport failure.
- Native StatusNotifierItem/D-BusMenu provides Open, Hide, Start/Stop and Quit. Window close hides only with a registered host; host loss restores the window. Quit leaves the engine running. Normal launches use one frontend instance.
- The installer is now 143 lines of Bash, replacing the 493-line Python implementation. It installs ordinary files in `~/.local/bin`, installs the user unit and desktop/icon, and retains itself under `$XDG_DATA_HOME/cosmos-audio-mixer`. There is no Python runtime dependency, ownership database or release directory. Both old opt layouts migrate automatically after a safe old-client stop; their files are archived, not deleted. On later failure, previous files are retained and the error is reported; there is no automatic rollback.
- The bundle ABI floor is glibc 2.39 for x86-64 and ARM64, built with Ubuntu 24.04 sysroots rather than the updated server's glibc 2.44. Stock Ubuntu support starts at 26.04: 24.04 disables LV2. Ubuntu 26 requires `libspa-0.2-modules-extra`. Actual Ubuntu 26 mono/stereo audio passed; ARM64 effects and live gain passed under QEMU, not physical ARM hardware.
- The current work also includes terminal logging flags, per-strip Input routing, input/output labels, confirmed system-card profile controls, and the readable-Format discovery correction. Protocol version is 3; saved profile version stays 1. Replace both executables together through the installer.
- Rust candidate verification remains 128 tests, formatting, Clippy, locked builds, and native backend/GUI scenarios. The Bash replacement passed syntax, installer regressions, native systemd unit validation and GIO launcher parsing. A real engine in the old flat layout saved a pending edit during automatic migration, the installed engine restored it, and uninstall saved its final edit while retaining the profile. The service-manager boundary was isolated; no host audio was changed. Evidence: `/var/tmp/cosmos-bash-native-t9qqsz6t`.
- A real private systemd user manager cannot run in this toolbox because its cgroup is not delegated. Do not change host/podman settings to bypass this. Real service start/enable/login restore, GNOME indicator rendering and launcher focus remain workstation acceptance checks.
- Current test bundles: `/home/jp/Projects/cosmos-audio-mixer-test/bash-installer/{x86_64,aarch64}/`. Binaries are unchanged from `service-and-indicator-r2`. Existing downloads need only the new `install.sh`, `SHA256SUMS` and `BUILD-INFO.json`; the SHA check confirms the whole bundle. `DEVELOPMENT.md` gives exact workstation copy/install/removal commands. Server: `jp@192.168.0.2`. Do not ask the user to move the old opt directory manually.

Current local evidence: `/var/tmp/cosmos-backend-bkdobtdi`, `/var/tmp/cosmos-service-ui-02nf_b52`, `/var/tmp/cosmos-ubuntu26-audio-mtw_3ww_`, and `/var/tmp/cosmos-arm64-audio-bkun6l1_`. The corrected real GUI adopted an external rename and mute during the deferred-save window without a false conflict; its captures are in `/var/tmp/cosmos-profile-adoption-4sh51jvr`. These paths are test artifacts, not source assets or release packages.

## Native frontend — 2026-11-02 (historical)

- Added `crates/mixer`, package/executable `cosmos-audio-mixer`, pinned to libcosmic `95236a944bfd51c63a2fc65fe50a592c47168d40`. The engine and control crates remain toolkit-independent.
- Added mono/stereo channel strips with volume, gate/compressor threshold and enable controls, full dynamics popup, per-channel parametric EQ popup, real input/output meters, and independent mute/hide/enable controls.
- Added a two-axis warmth/brightness color pad with labelled slider alternatives and ordinary EQ processing. No proprietary algorithm, recording controls, cassette UI, or other mixer's application code was used.
- Added explicit routing and local device aliases/visibility/participation controls. Audio controls are live while used, including slider and color drags. There is no Apply button, default-device takeover, or automatic microphone selection.
- The separate engine can be started/stopped explicitly. Closing/reopening the window leaves its saved audio state running. Final close/stop/reload actions wait for pending saves, and failures retain local edits. A fresh default launch lets the engine create its own private socket directory.
- Worker I/O is bounded to one request with the latest desired profile as the pending state. Acknowledgements cannot overwrite newer input, and due meter polls run between writes. Rejected writes stop automatic retries. Unknown write outcomes are reconciled against the exact sent profile before writes resume.
- Added persisted `Bus.gain_db`, default 0 dB for existing profiles. The fader precedes a trailing limiter; a separate terminal stage enforces mute/unavailable silence. The real audio harness checks boosted mono/stereo limiter ceilings, live gain, stable endpoints, and exact silence.
- Corrected the stale discovery warning exposed by the native smoke. In the read-only discovery connection, `ENOENT` can mean that a format is not yet negotiated or an object has disappeared, including removal races reported on the core. It now requests reconciliation without setting a persistent error; permission and transport errors are preserved. Regression coverage checks both diagnostic classification and actual source negotiation.
- Independent state/lifecycle and control/DSP reviews found no remaining blockers after corrections. The profile pre-check and Apply are not atomic across multiple clients; use one editing client at a time.
- Native window rendering succeeded with a private headless Weston compositor after the existing session socket failed with `NoCompositor`. No host audio graph or defaults were changed.
- Temporary pinned-iced widget simulation exercised real mouse events and the actual control worker/engine, not a web mockup. The live-control smoke deliberately omitted mouse-release events and observed successive gain changes in real synthetic output, a coalesced final gain, immediate mute, gate attenuation, EQ/color persistence, stable endpoints, and reopen. The earlier smoke also covered channel creation, popups, retained edits, reload, and fresh default engine launch. The simulator's text-input selector calls an unimplemented upstream method; the smoke used its public raw-text selector API. Temporary smoke code/dependencies are not part of the application.
- AT-SPI tree and screen-reader behavior remain unverified. The available launcher exposes `IsEnabled`, but the pinned AccessKit listens for `ScreenReaderEnabled`. No fake service or source fork was introduced.

Backend audio/lifecycle evidence is at `/var/tmp/cosmos-backend-_28bevqy`, including a Ready source with `discovery_error: null`. The live-control widget/audio smoke passed with evidence at `/var/tmp/cosmos-live-jxat2fse`. Earlier native Weston rendering evidence is at `/var/tmp/cosmos-ui-ontunmpb`. These are local temporary evidence paths, not repository assets. `DEVELOPMENT.md` records final checks and remaining verification limits.

Launch from the feature worktree after sourcing `dev-env.sh`:

```sh
cargo build --workspace --locked
./target/debug/cosmos-audio-mixer --standalone
```

The repository has `origin`. Earlier sessions used HTTPS with the installed `gh` credential helper when SSH authentication was unavailable, without global credential changes. Earlier frontend merge authorization applied to that delivery only. The current service/panel candidate must remain unmerged pending workstation approval.

The workstation target is Fedora 44 Workstation, x86-64, glibc 2.43. Prefer copying both optimized test executables together and installing native runtime dependencies; see `DEVELOPMENT.md`. This is private development testing, not distribution packaging. Do not replace workstation glibc or change host audio defaults.

## Backend milestone — 2026-11-01 (historical)

- Verified all 24 native packages inside `cosmos`, Rust/Cargo 1.99.1, native PipeWire 1.6.11, LSP LV2 1.2.29 discovery, and filter-host runtime linkage.
- Preserved the owner's governance changes in bootstrap commit `7c88940`. With current-task authorization, squash-merged the reviewed discovery milestone into local `development` as `8c07f7c`, then created the backend worktree. No remote exists; no push, PR, or hosted CI was possible.
- Replaced the product-specific exclusion in `AGENTS.md` with the general requirement to build independent application code. The original decision remains below as history.
- `cosmos-control` now defines bounded profiles, effects, durable device selectors, routes, status, and the local client. `cosmos-engine` implements `run`, `status`, `list`, `profile`, `apply FILE`, and `stop`.
- Each named mono/stereo bus has an input playback sink and output recording source. LSP gate/compressor/EQ/limiter instances run in the engine, not the desktop daemon. Parameters and mute change live without replacing endpoints; topology/channel changes can interrupt the affected bus.
- Explicit routes recover when selected sources return. Missing, ambiguous, or unsupported sources produce controlled silence and an explicit state, never microphone substitution or downmix. Bus-to-bus routes and direct application playback are supported.
- Hardware aliases, hide, enable, and mute are independent mixer-local settings. Defaults and unrelated graph objects are untouched.
- Real input-feed/output peak and RMS snapshots use passive streams and allocation-free sample callbacks. Native LV2 output-control meters are not exported by filter-chain; no exact gate-open or gain-reduction claim is made. Input metering follows actual incoming links rather than the ineffective native sink-monitor tap.
- Profiles are bounded, versioned, validated before mutation, atomically saved, and protected by a separate writer lock. Invalid originals are preserved during explicit recovery. Interrupted backup and exact-size boundaries have regression coverage.
- Final checks passed: formatting, locked workspace build, all 37 Rust tests, Clippy with warnings denied, and all three private lifecycle/audio harnesses.
- `tests/backend-audio.py --keep` passed mono/stereo full-chain samples, live threshold/endpoint stability, 44.1 kHz conversion, source loss/return, ambiguity, six-channel rejection, mute/hide distinctions, meter recreation, multiple-source meter continuity, unrelated-bus isolation, named buses, engine crash/restart, two server restarts, missing-plugin recovery, large IPC, invalid-profile rejection, and corrupt-profile preservation. Final evidence is at `/var/tmp/cosmos-backend-jgordl2u`; standalone DSP evidence is at `/tmp/cosmos-pw-f5uso86y`.
- A teardown regression exposed incorrect native error classification. Only core `-EPIPE` now resets the connection; other resource errors remain visible and trigger reconciliation. Links are removed before endpoint owners. The private harness confirmed that normal source removal and topology changes keep the connection generation unchanged.
- Independent persistence, realtime-meter, and native-routing reviews found no remaining blockers after fixes. The standalone private DSP harness remains useful for plugin/host diagnostics; the backend harness tests the actual Rust application.
- No live microphone capture, host service changes, default changes, or unrelated routing changes occurred. No rejected mixer source was used.

Next release work: complete the original reliability matrix, especially physical hotplug/Bluetooth, independent-clock hardware, latency, soak checks, desktop keyboard/window-manager behavior, and accessibility. Disruptive hardware checks still require explicit owner permission. The native frontend is no longer an unimplemented milestone.

`DEVELOPMENT.md` contains commands, profile semantics, lifecycle invariants, and verification boundaries. A complete dependency licence/source/notice audit remains required before distribution.

## User intent and scope

The user wants one native Linux application that makes microphone processing and desktop audio routing straightforward, without requiring several user-operated audio applications.

Required behavior:

- Select a real microphone; process it with a noise gate, compressor, and EQ; expose the result as a virtual microphone usable by recording/voice applications. A limiter belongs in the supported processing set.
- Gate usability is the priority: provide meaningful thresholds, timing, and feedback, not only an on/off switch.
- Handle other hardware and system/application inputs/outputs, with routes to named virtual playback and recording endpoints. Do not impose Linuxmeeter's four fixed bus identities.
- Real and virtual devices can be enabled/disabled in the mixer, hidden/shown, and named. Visibility, mute, and enable state are independent.
- Persist settings and reconcile the desired graph after device/server changes. Make adding supported effects and changing the UI straightforward.
- Native libcosmic frontend, written in Rust, using Wayland. Voicemeeter is interaction inspiration, not a required visual copy.
- The frontend must remain responsive during microphone hotplug, Bluetooth profile/errors, PipeWire restart, default changes, application stream replacement, duplicate device names, routing problems, and channel/clock differences.

The user tested Linuxmeeter and reports it does not work. Accept that observation; do not rerun it to challenge the report. The user explicitly rejected a fork and distrusts its implementation. Do not copy, depend on, or translate its source. It may at most be a reference if genuinely necessary. The previous recommendation to reuse its engine is superseded.

Deferred by the user:

- Exact window placement/session-position restoration.
- Flatpak. Do not create Flatpak manifests or install Flatpak tooling now.
- A separate GTK4 frontend/repository and multi-repo organization. Keep the backend toolkit-independent now; do not create the second project yet.
- `libcosmos` unless actual reusable toolkit gaps justify it.

## Confirmed answers

The user selected these options in the setup session:

1. **Rust application code with native DSP dependencies.** PipeWire C libraries through Rust bindings and LSP LV2 plugins are acceptable. Do not rewrite established DSP merely to eliminate foreign-language dependencies.
2. **Keep audio running after the window closes.** Use a separate background engine. Reopening the launcher shows/reconnects the UI. Provide an explicit engine stop action; login startup is opt-in.
3. **Mixer-local hardware controls.** Hide, alias, and enable/disable participation locally. Hardware stays available to other applications. Names for our own virtual endpoints are system-visible. System-wide hardware disabling/renaming is not part of the agreed initial scope.
4. **Mono/stereo workflows initially.** Detect and clearly report unsupported layouts. Do not crash, silently select channels, or silently downmix a multichannel device. Explicit multichannel mapping is deferred.

Safe defaults stated during the discussion: no automatic takeover of default input/output devices, no automatic substitution of another microphone, and no unprocessed microphone fallback on DSP failure. Expose degraded/unavailable states and recover automatically when the selected resources return.

No product question remains blocking the start of implementation. Hardware-specific validation details can be collected through read-only inspection and deliberate user-assisted checks later. A hosting remote/application ID is not needed to begin local implementation; do not invent ownership or publish anything.

## Naming, licensing, and upstream relationship

Project/display name: **Cosmos Audio Mixer**.
Repository/package name: **cosmos-audio-mixer**.
Upstream toolkit: **libcosmic** (not libcosmos).

Decision: independent branding; no System76/COSMIC logo, official `cosmic-` package identity, `com.system76.*` application ID, or endorsement claim. We can accurately describe our use of libcosmic and compatibility with the COSMIC desktop. The trademark policy encourages `cosmic-ext-` for third-party integrations but does not require our independently branded application to adopt that namespace.

Suitable attribution: “Cosmos Audio Mixer is an independent project built with libcosmic. It is not affiliated with or endorsed by System76. COSMIC is a trademark of System76.” This is an implementation/branding decision consistent with the published policy, not a legal clearance of the word Cosmos or all possible third-party marks.

libcosmic is MPL-2.0. Its file-level copyleft does not require every independent application file to use MPL, but we chose **MPL-2.0 for our original project code** for consistency. A full unmodified licence text is in `LICENSE`. Use SPDX notices in new source. Preserve dependency notices and provide the required covered source when distributing binaries. Do not assume the entire dependency graph inherits MPL: audit actual resolved crates, Git sources, native libraries, plugin packages, and assets before distribution. LSP's LGPLv3 is acceptable to the user; decide distribution/linking compliance from the actual integration. There is no Linuxmeeter code and no corresponding Linuxmeeter GPL inheritance.

The user explicitly confirmed MPL-2.0 after discussing GPL/LGPL alternatives. It is a project choice, not the only licence permitted for an independent application using libcosmic.

The libcosmic PR template explicitly prohibits LLM-generated code, comments, and PR descriptions. Respect that policy; do not submit our generated work upstream or claim it is human-written.

If concrete reusable gaps cannot be addressed with public libcosmic/iced APIs, consider a separate **libcosmos** extension library, described as “an independent, AI-assisted extension library for libcosmic.” It should compose with upstream, not impersonate an official successor. Start mixer-specific controls in this application. Extract only demonstrated reusable pieces. If required internals are inaccessible, explain the blocker and design alternatives; the user has not authorized a source fork disguised as an extension.

Primary sources inspected:

- Trademark: <https://github.com/pop-os/cosmic-epoch/blob/master/TRADEMARK.md>
- libcosmic licence: <https://github.com/pop-os/libcosmic/blob/95236a944bfd51c63a2fc65fe50a592c47168d40/LICENSE>
- Mozilla MPL FAQ: <https://www.mozilla.org/en-US/MPL/2.0/FAQ/>
- AI contribution policy: <https://github.com/pop-os/libcosmic/blob/master/.github/PULL_REQUEST_TEMPLATE.md>
- libcosmic platform documentation: <https://pop-os.github.io/libcosmic-book/introduction.html>

## Architecture direction, not implemented code

Keep one repository with separate Rust crates/modules for the domain/control contract, audio engine, and libcosmic application. Do not introduce a generic framework for hypothetical frontends. The engine must not depend on libcosmic. A later GTK frontend should reuse the same engine/control contract rather than duplicate audio logic.

Run the engine in a separate process from the GUI. This is required by the selected lifecycle and helps contain native-plugin faults. Use local, user-scoped IPC with bounded messages and explicit connection/error states; no network listener is needed. Distinguish control acknowledgements from replaceable meter snapshots so metering cannot fill an unbounded queue or starve control updates. The GUI must never wait synchronously for audio service I/O or engine shutdown.

Use native PipeWire APIs for discovery, graph objects, links, and properties. Do not make a runtime subprocess/shell-command controller. PipeWire's filter-chain host with supported LSP LV2 effects is the initial DSP-hosting candidate. Verify live parameter changes, topology changes, latency, and fault behavior before committing to details. Host DSP in the application's engine process, not by injecting third-party plugins into the desktop PipeWire daemon. Native DSP can still crash its host process; isolation is not a claim that this cannot happen.

Keep PipeWire ownership on its appropriate thread/event loop. Separate real-time processing from disk writes, plugin discovery/loading, log formatting, blocking IPC, and GUI work. No allocations or blocking locks in real-time sample callbacks. Keep timers/reconnection finite per attempt and event-driven; use bounded backoff rather than a busy retry loop.

Maintain explicit desired state and observed state. Reconcile idempotently. Use connection generations so callbacks and acknowledgements from a dead server connection cannot act on newly reused object IDs. Never persist numeric PipeWire object IDs as durable hardware identities. Detect ambiguous device matches and ask for selection rather than capture the wrong source.

Use app-owned virtual endpoints with stable logical names. Keep their identities across ordinary UI closure and, where the architecture allows, device disappearance/effect edits. A PipeWire server restart destroys the graph: recreate endpoints and links after reconnect. Whether another application's recording stream automatically reconnects must be tested; do not promise control over third-party client behavior.

Effects should be an ordered, persistable list of supported instances with stable type IDs and validated parameters. Use small adapters for actual supported plugins, not an arbitrary-plugin marketplace/SDK. Draw native controls; embedding third-party LV2 UIs is not required. Missing/failed mandatory DSP must not silently send a raw microphone signal.

Do not fight WirePlumber for ownership of unrelated links, device profiles, volume, or defaults. Limit graph changes to app-owned objects and explicit user routing requests. Keep our sources out of automatic microphone selection candidates to avoid self-routing. Reject feedback cycles among app-managed routes. Acoustic feedback and cycles created later by external graph managers cannot be completely prevented by our graph validation; make monitoring opt-in and handle detected unsafe topology visibly.

## Reliability acceptance criteria

The user requires smooth behavior and no crashes/freezes caused by normal audio disruptions. This is a release requirement, not an optional polish step. Absolute failure-free operation under arbitrary hardware/kernel/OOM failures cannot be promised. Audio cannot continue through a disconnected source or stopped server. The contract is responsive UI, controlled silence/unavailability, preserved configuration, and correct automatic recovery when resources return.

Build an isolated real-PipeWire harness early. Combine deterministic model tests with actual audio-path and process-lifecycle checks. Each scenario must inspect graph/state and recorded samples where relevant, not only assert that no exception occurred.

| Scenario | Required result |
| --- | --- |
| Start with PipeWire absent | UI opens and remains interactive; engine reports disconnected; no spin or repeated modal errors; connect when the server becomes available. |
| Restart test PipeWire repeatedly | Old generations discarded; exactly the intended virtual endpoints/routes reappear; no duplicate links, leaked objects, or lost settings; UI continues responding. |
| Remove/re-add selected microphone | Preserve selection and processing state; no unrelated microphone fallback; virtual output is silent while unavailable where the server remains present; reconnect when identity matches. |
| Bluetooth profile/error transition | Model old nodes disappearing/new nodes arriving; negotiate supported layout; show unavailable when necessary; do not freeze or change system-wide Bluetooth policy. |
| Default input/output changes | Explicit device selections remain explicit; any follow-default option is deliberate and excludes our own virtual nodes. No unrequested takeover. |
| Application streams appear/disappear | Reconcile intended routing without retaining stale IDs or repeatedly stealing unrelated streams. |
| Duplicate/ambiguous names | Distinguish stable identities; do not silently attach to another device with the same description. |
| Feedback route request | Reject app-managed graph cycles with a clear explanation; no invalid graph committed. |
| Mono/stereo and rate/clock differences | Explicit channel semantics; supported conversion/resampling negotiated with PipeWire; reject/report unsupported layouts, never reinterpret buffers. Exercise clock drift with real hardware before claiming coverage. |
| Missing plugin / invalid parameter / failed graph change | Report actionable error, avoid raw-mic bypass, preserve valid state or silence safely; do not crash the UI. |
| Engine exits/crashes | UI stays responsive and reports loss; controlled restart policy without a crash loop; state restored and any stream interruption visible. |
| Close/reopen UI | Engine PID/audio remain alive; new UI receives current state; no duplicate engine instance. |
| Slow/disconnected UI or high-rate meter events | Bounded memory and queues; no backpressure into real-time audio; controls remain acknowledged or explicitly rejected. |
| Invalid/corrupt configuration or interrupted save | Preserve recoverable data; validated defaults/recovery state; atomic versioned persistence; never silently overwrite the only valid profile. |
| Hide/disable/rename | Hide affects presentation only; disabled participation is distinct from mute; hardware remains available outside the mixer; aliases survive reconnect. |

Set measurable UI responsiveness and reconnection thresholds for the actual hardware/test environment before declaring these criteria passed. Perform repeated/soak tests and check resource growth. Mock graph events alone do not establish real Bluetooth or multi-clock correctness.

No host service restarts or live microphone recording occurred in this preparation session. Disruptive tests must target a private daemon/socket and test-owned processes. Ask before intentional host audio disruption or microphone capture.

## Historical setup-session state

- Destination already had an empty `.git` and a user-owned `AGENTS.md` configured for `cosmos`. Both were preserved. Project-specific constraints and environment instructions were appended to AGENTS.md.
- Working branch: `setup/project-handover`, an unborn branch. No commits, remote, pushes, or published repository were created. Long-lived branch workflow cannot be initialized from an absent `origin/development`; see the bootstrap rule in AGENTS.md.
- New files: `HANDOVER.md`, `DEVELOPMENT.md`, `LICENSE`, `rust-toolchain.toml`, `dev-env.sh`, `dependencies-fedora.txt`, `.gitignore`.
- The OMP session itself was in `dev`, but setup commands used `toolbox run --container cosmos ...`. The next session should start inside `cosmos` as requested.
- `cosmos` is reachable and has configured Fedora 45 repositories. It lacks most native build/audio dependencies.
- `sudo -n true` failed because a password is required. Native package installation remains the owner's action; use the command below. Package names were checked through DNF, including Fedora's separate `pipewire-module-filter-chain-lv2` package.
- The old `rustup` command resolved to a mise shim without a selected version. No global mise state was changed.
- Installed dedicated rustup and **Rust 1.99.1** under `~/.local/share/cosmos-toolchain/` using the official bootstrap and verified SHA-256. The rustup installation also selected a default within that dedicated home, not the user's other Rust homes. `dev-env.sh` selects this installation.
- `rust-toolchain.toml` is the compiler/component source of truth. Components installed: rustfmt, Clippy, rust-src, rust-analyzer, plus compiler/Cargo/standard library.
- Inspected libcosmic candidate revision: `95236a944bfd51c63a2fc65fe50a592c47168d40`; minimum Rust in its manifest: 1.93. Candidate is NOT compiled or validated for this project. The current stable Rust manifest verified 1.99.1. No Cargo.lock or complete transitive licence audit exists yet.
- No Codegraph index exists. The tool reported this; no index was generated. Follow the mounted tool policy about indexing authorization rather than indexing outside project scope.

Observed verification:

- Reached `cosmos` successfully through Toolbox.
- Resolved native package names from configured repositories; native packages were not installed.
- Checked the official rustup bootstrap checksum before execution.
- `bash -n dev-env.sh` passed.
- Sourced `dev-env.sh` twice inside `cosmos`; Cargo/rustup resolved to the dedicated installation.
- `rustup show active-toolchain` selected 1.99.1 from this repository's toolchain file.
- Confirmed required components were installed.
- Compiled and ran a temporary Rust 2024 program in `cosmos`; output: `cosmos toolchain smoke: OK`. Temporary files were removed.
- The environment script rejected sourcing from the `dev` container.
- Confirmed OMP 18.15.0 runs inside `cosmos`; its SDK extension loader discovered and loaded the existing RTK adapter. Exercised command-rewrite/bypass behavior and real Cargo success/failure/JSON handling. No interactive OMP UI or model-backed session was launched by the audit. See `DEVELOPMENT.md` for versions, safeguards, and the vetted skills recommendation.
- Validated the 24-entry native dependency list as package-name arguments.
- No application build, DSP test, GUI test, latency measurement, or fault-recovery validation has been performed.

## Owner restart steps

From the host, enter the toolbox. In Bash inside it:

```sh
toolbox enter cosmos
cd ~/Projects/cosmos-audio-mixer
bash
source ./dev-env.sh
```

Install native packages interactively with the owner's sudo password:

```bash
mapfile -t packages < dependencies-fedora.txt
sudo dnf install "${packages[@]}"
```

Then start OMP from this directory and the configured shell. Suggested opening message:

> Read AGENTS.md, HANDOVER.md, and DEVELOPMENT.md in cosmos-audio-mixer. We are inside the cosmos toolbox. Continue the from-scratch Rust/PipeWire backend and libcosmic frontend according to the confirmed decisions. Verify native dependencies first. Do not reuse Linuxmeeter, build Flatpak, create the GTK repo, or disrupt my live audio session. Start with the backend/control contract and real isolated audio/recovery validation, then deliver the native UI against it. Do not treat a scaffold or a working happy-path microphone demo as completion.

## First implementation actions

1. Confirm the container, source the project environment, and verify native packages and LV2 host/plugin discovery. Resolve actual missing prerequisites without changing the host session.
2. Establish a small, explicit domain/control contract and process lifecycle for the agreed mono/stereo mixer. Keep backend independent of libcosmic. Write down actual reconnect and shutdown invariants before implementing callbacks.
3. Introduce the Rust workspace, pin a tested libcosmic revision, resolve dependencies, retain Cargo.lock, and check licences. Create only crates with real responsibilities, not empty extension or future frontend crates.
4. Prove selected-source processing into an app-owned virtual microphone in a private PipeWire graph with real LSP DSP. Exercise reconnection and safe failure behavior immediately, rather than adding resilience after a UI demo.
5. Implement the complete routing/device/alias/visibility/effect state and its persistence with the same ownership/recovery rules. Add the libcosmic interface over the working contract. Verify actual window interaction, accessibility, and cross-desktop behavior; no Tauri or webview.
6. Complete the reliability matrix and required scope before calling the application done. Keep evidence, measured limits, and outstanding hardware coverage explicit. Decide native release packaging later under the existing release policy.
