# Cosmos Audio Mixer — implementation handover

## Resume here

Read `AGENTS.md`, this file, and `DEVELOPMENT.md`. Run project commands inside the `cosmos` toolbox and source `dev-env.sh` before Rust commands. The main checkout is `/home/jp/Projects/cosmos-audio-mixer`. The backend implementation worktree is `/home/jp/Projects/.worktrees/cosmos-audio-mixer/backend-routing`, on `feature/backend-routing`. Start new work from `development`.

The Rust backend now hosts native LSP DSP, creates named playback/recording endpoints, reconciles explicit routes, provides real input/output levels, and persists validated profiles. It has passed private synthetic-audio and recovery checks. The libcosmic frontend is not implemented. Physical hardware, latency, clock drift, and long-running reliability remain separate validation work; do not claim release readiness.

## Current implementation — 2026-11-01

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

Next product work: build the libcosmic frontend against the working backend contract. Keep the original reliability matrix as the release requirement. Physical hotplug/Bluetooth, independent-clock hardware, latency, long soak tests, and actual GUI responsiveness still need evidence and, where disruptive, explicit owner permission.

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
