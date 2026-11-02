# Development environment

Run project commands inside the Fedora `cosmos` toolbox. The application itself is not intended to require Toolbox, COSMIC desktop services, or Flatpak.

## Enter and configure

From a host terminal:

```sh
toolbox enter cosmos
cd ~/Projects/cosmos-audio-mixer
bash
source ./dev-env.sh
```

`dev-env.sh` must be sourced from Bash. It checks the container name and selects dedicated Cargo/Rustup homes under `${XDG_DATA_HOME:-$HOME/.local/share}/cosmos-toolchain/`. Toolbox shares the user's home directory: these files are separate from other toolchains, but are not isolated by a filesystem sandbox. No mise configuration or shell startup file is changed. Source this environment before launching OMP so its child commands inherit it.

## Native packages

All 24 packages in `dependencies-fedora.txt` were verified as installed inside `cosmos` on 2026-11-01. For a fresh toolbox, install them interactively:

```bash
mapfile -t packages < dependencies-fedora.txt
sudo dnf install "${packages[@]}"
```

`dependencies-fedora.txt` is the authoritative initial Fedora package list. Its package names were found in the container's configured Fedora 45 repositories. It includes:

- C/C++ tools, CMake, pkg-config, and Clang development support for native dependencies and generated bindings.
- Expat, Fontconfig, FreeType, xkbcommon, Wayland, and GL/EGL development libraries for libcosmic's native dependency stack.
- PipeWire libraries, headers, daemon, diagnostic tools, and the separately packaged `pipewire-module-filter-chain-lv2` host; WirePlumber for isolated integration tests; LSP LV2 effects and Lilv inspection utilities.

Installing these packages is not permission to start or enable another audio session manager against the host graph. Do not run `systemctl --user enable/start/restart pipewire` or WirePlumber setup commands as part of environment preparation. Toolbox can expose host session sockets and the host user service manager.

The engine uses PipeWire 0.10.1 bindings with the `v1_0_0` API feature. Development verification uses native PipeWire 1.6.11 and LSP 1.2.29. The native LV2 filter host links to Lilv; direct Lilv development headers are not needed for this integration. The frontend uses Wayland and the software renderer. Do not install GTK4, Qt, Tauri, Node, Bun, or a full desktop environment for the frontend. The workstation compatibility baseline below is separate from this Fedora build environment.

DNF resolves package versions from the enabled repositories. This file is NOT an RPM version lock. Development PR validation uses a pinned Fedora 45 toolbox image and records resolved RPM versions in its log; native repository contents are not frozen. Do not claim fully reproducible native dependencies yet.

## Rust toolchain

`rust-toolchain.toml` pins Rust **1.99.1**, with rustfmt, Clippy, rust-src, and rust-analyzer. Rust 1.93 in the inspected libcosmic manifest is its minimum compiler version, not a requirement to use exactly 1.93. Use the pinned current stable compiler; reassess the actual dependency MSRVs when Cargo resolves the graph.

The setup session installed rustup and the pinned toolchain without sudo, using the official x86_64 Linux bootstrap and its published SHA-256 checksum. The bootstrap executable was removed afterward. The compiler compiled and ran a temporary Rust 2024 program successfully inside `cosmos`.

After sourcing `dev-env.sh`, verify:

```sh
command -v rustup cargo rustc
rustup show active-toolchain
rustup component list --installed
rustc --version
cargo --version
```

The commands must resolve under the dedicated Cargo home, not a mise shim. Run from the project directory so the toolchain file selects the version. To update Rust, change `rust-toolchain.toml`, update this document, install the new toolchain/components, and exercise the real application and checks. Do not use floating `stable` as the committed pin.

For a fresh toolbox/home, install Fedora's `rustup` package, which supplies `/usr/bin/rustup-init`. Source `dev-env.sh`, then run `/usr/bin/rustup-init -y --no-modify-path --profile minimal --default-toolchain none`. This creates the proxies in the dedicated Cargo home without changing shell startup files. Use `"$CARGO_HOME/bin/rustup"` to install the version/components specified in `rust-toolchain.toml`. Confirm Cargo/rustc command resolution before building; do not assume the RPM provides `/usr/bin/rustup`.

## Upstream dependency pin

Pinned and compiled libcosmic revision:

`95236a944bfd51c63a2fc65fe50a592c47168d40`

Source: <https://github.com/pop-os/libcosmic/commit/95236a944bfd51c63a2fc65fe50a592c47168d40>

The frontend enables `wayland`, `tokio`, `a11y`, `single-instance`, and `multi-window`, without the default COSMIC configuration-service feature. The panel indicator uses `ksni` 0.3.7 and the StatusNotifierItem/D-BusMenu protocols, not a COSMIC-only applet. `Cargo.lock` retains transitive Git revisions. Private Weston checks prove native rendering without COSMIC services, not accessibility or compatibility with every compositor. No toolkit source fork or upstream submission was made.

## OMP, RTK, and agent skills

Launch `omp` from the shell where `dev-env.sh` has been sourced. The tooling audit inside `cosmos` observed OMP **18.15.0**, RTK **1.0.1**, Bun **1.4.4**, and rust-analyzer **1.99.1**. OMP/Bun/RTK use existing shared-home installations; they are development tools, not application dependencies or container-isolated installations. Rust resolves through the dedicated project toolchain. No mise changes are needed.

OMP's own extension discovery and loader successfully found and loaded `~/.omp/agent/extensions/rtk.ts` inside `cosmos`. Preserve its local compatibility patch: return revised tool input, rather than only mutating the event, and bypass non-display commands. Do not replace it with an upstream RTK hook during an update without revalidating those behaviors.

Verified with the loaded adapter and real RTK:

- Human-readable `cargo test --locked` and `git status` commands are rewritten.
- Cargo metadata/JSON diagnostics, Git porcelain/NUL/custom-format commands, compound commands, Bun scripts, and explicit `RTK_DISABLED=1` commands remain unchanged. Already-prefixed RTK commands are not wrapped again. Non-Bash tools remain untouched.
- In a temporary Rust crate, RTK preserved a successful test's exit code `0`, a deliberately failing test's exit code `101` (matching direct Cargo), and the failure assertion text. Bypassed Cargo metadata parsed as JSON. The temporary crate was removed.

RTK telemetry is disabled and consent is false in `~/.config/rtk/config.toml`; local usage tracking is enabled. No telemetry opt-in or global configuration change was made. OMP's native shell minimizer is separately enabled: `RTK_DISABLED=1` bypasses RTK, not that independent layer. For machine-consumed data, parse/capture bytes inside the process instead of treating rendered Bash text as a binary protocol. Inspect original-output artifacts when diagnosing compressed failures.

OMP skills and `~/.agents/skills` discovery are enabled. Existing relevant skills: `rust-best-practices`, `rust-async-patterns`, `rust-code-review`, `security-audit`, `archify`, and `archify-review`. Async guidance applies to control/IPC/lifecycle work, not permission to put Tokio tasks, allocation, or blocking locks in audio callbacks. LSP is enabled/lazy and rust-analyzer resolves correctly after sourcing the environment; project-level LSP behavior remains to be exercised once a Cargo workspace exists.

Skills research used the catalogue and upstream content, including `bun x --bun skills@1.7.4 find ...` with `DISABLE_TELEMETRY=1 DO_NOT_TRACK=1`; it did not install new skills:

- Recommended optional addition: [Trail of Bits property-based-testing](https://github.com/trailofbits/skills/tree/main/plugins/property-based-testing/skills/property-based-testing), for meaningful routing/configuration/recovery invariants and Rust `proptest`. It does not replace real PipeWire fault injection. The skill repository's CC BY-SA 4.0 licence is separate from this application's MPL-2.0 licence; preserve its notices if vendoring its text.
- Do not add generic WebAudio, C++ DSP, or PipeWire CLI-operation skills as substitutes for Rust real-time host engineering. No sufficiently vetted skill covering the complete PipeWire/LV2 host and recovery problem was found.
- Inspected COSMIC skills are optional references, not trusted API specifications: applet guidance does not establish standalone-app behavior, and COSMIC-only automation does not establish GNOME/Plasma compatibility. Prefer the pinned libcosmic source/examples and upstream PipeWire/LV2 host documentation.

No duplicate project RTK hook, new OMP plugin, global skill bundle, or speculative `libcosmos` library was added.

## Verification boundaries

Use a private runtime/config/state directory and an explicitly selected private PipeWire socket for automated fault injection. Do not rely only on the inherited session environment to isolate a test. Start/stop only test-owned processes. Use synthetic audio by default, not a live microphone. Real desktop/default-device changes and disruptive host tests need explicit authorization.

A native UI requires actual Wayland-session checks. Before a UI launch, verify the display and session sockets available inside the toolbox without changing the host. A headless successful build does not prove GNOME/Plasma/COSMIC compatibility.

For Rust changes, run `cargo fmt --all -- --check`, `cargo build --workspace --locked`, `cargo test --workspace --locked`, and `cargo clippy --workspace --all-targets --locked -- -D warnings`. Exercise the actual changed path as well. Avoid `--all-features` if the selected toolkit has mutually exclusive feature combinations.

Development PRs run `.github/workflows/validate.yml`: the runner creates a `cosmos` toolbox from a pinned Fedora image, installs the baseline packages, and checks formatting, locked compilation, tests, and Clippy with the pinned toolchain. Project commands run inside that toolbox. This is reduced validation, not distribution packaging. Physical/audio smoke evidence is still required locally for relevant changes. Promotion to `main` still requires full validation and critical end-to-end workflows; this development-only workflow does not satisfy that release gate.

## Private DSP verification

Run `./tests/private-pipewire/verify.py` inside `cosmos`. Add `--keep` to retain logs, graph snapshots, plugin metadata, and raw float samples. Failure always retains these files and prints their location.

The harness uses a private socket, runtime, configuration, state, and HOME. It starts no hardware discovery or session manager. It uses native CLI tools only as test drivers, not as application control code. All audio is synthetic mono at 48 kHz. The filter-chain host runs in a separate test-owned process, not in the PipeWire daemon.

Verified on 2026-11-01 with PipeWire 1.6.11 and LSP 1.2.29:

- Gate suppresses a below-threshold signal and passes an above-threshold signal.
- Compressor reduces a loud signal without changing a quiet signal.
- Parametric EQ applies the configured -12 dB cut at 1 kHz.
- Limiter holds the measured peak below its 0.1 ceiling.
- Ordered gate/compressor/EQ/limiter chain produces processed samples before and after host recreation.
- Missing mandatory plugin rejects host startup without publishing an endpoint or raw bypass.
- Two private server restarts allow explicit host recreation and restore measured samples.

PipeWire 1.6.11 `pw-cat` returns status 1 when finite recording reaches its sample limit. The harness checks that specific native result, empty recorder diagnostics, exact sample count, finite values, and DSP outcomes. It does not accept arbitrary failures.

This standalone harness proves native DSP behavior. The separate backend harness below verifies engine-owned hosting, controls, routing, persistence, and recovery. Neither harness proves physical device behavior, clock drift, latency, UI behavior, or the full release acceptance matrix.

## Backend and control client

The workspace contains `cosmos-control` (toolkit-independent protocol, profile model, and client), `cosmos-engine` (native discovery, DSP hosting, routing, metering, and persistence), and `cosmos-audio-mixer` (the libcosmic frontend). Build after sourcing `dev-env.sh`:

```sh
cargo build --workspace --locked
./target/debug/cosmos-engine --help
./tests/engine-lifecycle.py
python3 ./tests/device-profiles.py
./tests/backend-audio.py --keep
```

The lifecycle test uses a private server and three synthetic nodes. It does not connect to desktop audio. It checks server absence, mono/stereo/unsupported layouts, startup without discovery errors, duplicate descriptions, three server restarts, stale-node removal, duplicate engine rejection, slow/oversized clients, crash recovery, and explicit stop. It also checks that existing user files survive and that non-private socket directories are rejected.

The executable accepts `run [--socket PATH] [--remote NAME] [--config PATH]`, `status|list|profile|flush|stop [--socket PATH]`, and `apply|update FILE [--socket PATH]`. `run` stays in the foreground, as required by the user service. Closing a client does not stop it. The default socket is `$XDG_RUNTIME_DIR/cosmos-audio-mixer/control.sock`. Without an explicit remote or `PIPEWIRE_REMOTE`, the engine connects to the default PipeWire server and restores the saved profile. An empty profile creates no audio path. Automated checks always use private runtime, HOME, configuration, and PipeWire instances.

Both executables accept `--log-level error|warn|info|debug` and `--log-stdout`. The default is `warn` on stderr. Recoverable audio/configuration failures use `warn`; fatal executable failures use `error`. `info` adds startup, shutdown, connection and recovery messages. `debug` adds control/state details. Repeated unchanged engine/control failures do not print on each poll, application messages are bounded, and audio callbacks do not write logs.

```sh
./target/debug/cosmos-audio-mixer --standalone --log-level info --log-stdout
./target/debug/cosmos-engine run --log-level debug --log-stdout
./target/debug/cosmos-engine status --log-level debug
```

The default GUI Start/Stop actions use `cosmos-audio-mixer.service`. Its output goes to the user journal; GUI logging flags do not change a running service's logging configuration. In `--standalone` mode, explicit GUI logging flags also apply to the sibling engine it starts, including redirected output. An already-running engine keeps its logging settings. JSON client commands reject `--log-stdout` so stdout remains valid JSON. Native libraries and windowing dependencies can still write repeated messages to stderr; the application logger does not suppress them.

The control protocol is version 3. Update both executables together. The installer uses the old installed engine client to save and stop an existing managed engine before replacement. Saved mixer profiles remain version 1.

Lifecycle invariants:

- Only the engine that holds the persistent socket lock can own the control endpoint. A stale socket can be reclaimed after a crash; ordinary files and symlinks are not replaced.
- Socket parents must be private and user-owned. Ancestors must be root/user-owned, with write access restricted unless sticky. Toolbox maps `/tmp` ownership to UID 65534 here; lifecycle tests use `/var/tmp` rather than weaken these checks.
- Every connection attempt has a new generation. Disconnect removes observed nodes and devices. Numeric IDs are valid only in that generation and are not durable device selection keys.
- Only a core `-EPIPE` error marks a lost native connection. Other native resource errors remain visible as bounded diagnostics and trigger graph reconciliation. Route and meter links are removed before their endpoint owners; ordinary graph changes do not reset unrelated buses.
- Reconnect delay grows from 250 ms to at most 2 s. Handshake timeout is 2 s. Control requests remain available while PipeWire is absent.
- IPC accepts at most eight active clients, 68 KiB requests, and 2 MiB responses. Complete serialized profiles are limited to 64 KiB. Server requests have a 1 s deadline; client operations have a 2 s absolute deadline. Stop has a bounded acknowledgement period.
- Discovery reports identity candidates and channel semantics. Routing uses explicit selectors, never defaults or saved numeric IDs. Missing or ambiguous sources and unsupported layouts produce an error/unavailable state, not another microphone or a downmix.
- Node `Format` and `EnumFormat` subscriptions follow the `SPA_PARAM_INFO_READ` flag. Subscription also starts enumeration, so an unreadable parameter must not be subscribed. When read access is removed, discovery clears the cached layout and ignores late parameter replies. Real I/O and permission errors remain visible.

The locked dependency graph now includes libcosmic and its transitive Git dependencies, in addition to registry packages and the three local crates. The earlier 65-package backend-only inventory no longer describes this workspace. A complete licence/source/notice audit remains required before distribution.

### Profiles and processing

`profile` returns the complete desired profile. `update FILE` validates and applies it immediately, then schedules a save after two seconds without a changed profile. An accepted Update can have `status.config_pending=true`; it is not a durability acknowledgement. `flush` saves pending state immediately. `apply FILE` is the durable administrative operation: it validates and saves before changing live state, except that a directory-sync failure after replacement must adopt the file already written. Check `status.backend`, `status.config_pending`, and `status.config_error`; accepted settings do not imply that hardware or routes are ready.

The version-1 profile contains `buses`, `devices`, and `routes`. See `crates/control/src/model.rs` for the strict schema and bounds, and `tests/backend-audio.py` for runnable synthetic examples.

- Each bus has an independent ID/name, explicit `mono` or `stereo` channels, enable/hidden/mute flags, `gain_db`, and an ordered list of effect instances. Gain accepts finite values from -60 to +12 dB. Existing version-1 profiles without this field load at 0 dB.
- Each bus publishes `cosmos.bus.<id>.input` as a playback sink and `cosmos.bus.<id>.output` as a recording source. No system defaults change. Applications can use the input directly, or receive explicit engine-owned routes.
- Supported effects are gate, compressor, one parametric EQ band per instance, and limiter. Multiple EQ instances form a multi-band chain. Gate controls include threshold, attack, hold, and release. Parameters use explicit dB, Hz, and milliseconds and must fit the installed native plugin ranges.
- Parameter changes, volume, and mute use native live controls and retain endpoint identity. The volume stage is before a trailing limiter, or after the other effects when no trailing limiter exists. A separate final safety stage enforces exact silence for mute/unavailability. Disabling or removing the limiter removes its ceiling protection. Effect topology/channel changes safely replace the affected bus and can interrupt its clients. Endpoint logical names remain stable.
- Device selectors use the identity properties from discovery. The saved profile's device aliases, visibility, enable state, and mute stay local to this mixer. Hiding does not mute or disable. These local settings do not change hardware names, profiles, volume, or availability outside the mixer. Separate, explicit system-profile actions are described below.
- Routes connect selected hardware/application nodes and named buses. Validation rejects feedback and overlapping selector cycles before persistence. Resolved graph validation rejects identity aliases that would create a cycle.
- Plugins run in the engine process, not the desktop PipeWire daemon. Native plugin failure can still terminate that engine process. No raw-audio fallback is created when required processing fails.
- Input-feed and output meters report real per-channel linear peak/RMS values, sample counts, and freshness. They use bounded replaceable snapshots, not an event queue. Input metering follows actual incoming links; it does not rely on the native sink-monitor path. These values are not an exact LV2 gate-open or gain-reduction signal.

### Persistence

The default profile is `$XDG_CONFIG_HOME/cosmos-audio-mixer/profile.json`, or `$HOME/.config/cosmos-audio-mixer/profile.json` when the XDG variable is absent. Use `--config` for an explicit private path. The directory must be private and user-owned; stored files are mode 0600. A separate persistent lock prevents two engines from writing the same profile.

The engine owns persistence, including when no frontend is open. It saves the complete profile after two seconds without a changed Update. Repeated identical updates do not postpone an active save deadline. Volume, mute, channel settings, routing, effect order and enable state, all effect parameters, tone-color EQ bands, and mixer-local device settings remain in the same profile.

Saving writes and syncs a private temporary file, atomically replaces the profile, and syncs its directory. Corrupt or unsupported profiles are reported, not silently overwritten; explicit recovery preserves the invalid original with a no-replace backup rename. A failed autosave retains live state and a visible error, without a busy retry loop. Retry, a new edit, or an explicit Flush can retry the save.

Frontend hide/quit drains pending edits and requires a successful Flush. Stop also requires a successful save. SIGINT/SIGTERM and fatal exits attempt a final save. A failed signal-triggered save keeps the engine alive and reports the error. This cannot protect unsaved RAM from power loss, SIGKILL, or systemd's 15-second stop timeout. Check save errors before shutdown; do not claim durability while `config_pending` is true.

### Backend verification

`./tests/backend-audio.py --keep` exercises the Rust executable, not a substitute filter host. It records synthetic samples and retains logs, graph snapshots, status, profiles, and audio files in the printed private directory.

Verified paths include:

- Apply while the server is absent; large bounded profile transfer; rejection of invalid parameters and feedback without changing saved state.
- Real gate and ordered gate/compressor/EQ/limiter processing in mono and stereo; live thresholds; 44.1 kHz input into the 48 kHz graph.
- Live mono/stereo volume attenuation, +12 dB boost, unchanged endpoints, and exact muted/unavailable silence. With +12 dB before the trailing limiter, measured loud-signal peaks stay below the configured -1 dB mono and -6 dB stereo ceilings.
- Real input/output meters, recovery after removal of a meter node, and continued input metering when one of two sources leaves.
- Source disappearance/return, ambiguous identity, unsupported six-channel input, and no unrelated-source substitution. Unavailable output is silent.
- Direct application playback, named bus-to-bus routing, independent hide/mute/enable behavior, and continued audio on an unrelated bus when another route fails.
- An unchanged connection generation across source removal, meter removal, and bus topology changes while the private server remains running.
- Engine crash/restart and two private PipeWire restarts with automatic restoration from saved state.
- Missing native plugins with no bypass endpoints, plugin restoration, and corrupt-profile preservation/recovery.

Physical microphone hotplug, Bluetooth transitions, independent hardware clocks, end-to-end latency, and long-running resource growth still require separate authorized validation. The private tests do not capture a live microphone or change desktop audio.

### System device profiles

The Devices page discovers native PipeWire Device objects, including cards with profile `off` and no current input/output nodes. It shows the active profile, advertised profile choices, availability, and read/write limitations. Input/output labels on mixer endpoints describe audio direction relative to the mixer; an application playback stream is an input, and an application recording stream is an output.

Choosing a system profile requires confirmation. **Off** disables the entire sound card for all applications, potentially removing both microphones and speakers. To restore it, select an available active profile from the same card. No prior/default profile is guessed. The OS may remember the choice; the native request sets `SPA_PARAM_PROFILE_save=true`. System profile actions are not stored in the mixer profile and are never applied automatically on startup, reconnect, or profile reload. The app does not set system defaults directly, but the session manager or other applications can react to device removal.

The authenticated `SetDeviceProfile` command checks engine PID, connection generation, device ID, object serial, permissions and the advertised profile index. Acceptance is `pending`, not success. Native readback and a core barrier confirm the requested current state. Native request sequences keep late errors from a previous operation separate. Removal, disconnect, native rejection, or a five-second timeout produces a retained failure. The UI does not retry an uncertain OS action automatically; inspect the current profile before trying again.

`python3 tests/device-profiles.py` compiles a small test-only SPA Device into a private temporary directory. It checks off/on input/output removal and restoration, stale PID/generation/serial rejection, unavailable/invalid profile rejection, native `EIO`, timeout, removal, disconnect, and saved-profile isolation. Rust tests also cover read/write access checks and delayed/superseded native replies. The synthetic test does not prove physical-card behavior or WirePlumber persistence across login/restart. Do not use host devices for automated fault injection.

## Native mixer frontend

Run from a Wayland session inside `cosmos`, after sourcing `dev-env.sh`:

```sh
cargo build --workspace --locked
./target/debug/cosmos-audio-mixer --standalone
```

Normal launches use the installed systemd user service and a single frontend instance. **Start engine** starts that service; **Stop engine** first saves and stops the engine through its control socket, then stops the service. The GUI does not block while systemctl runs. `--standalone`, or any explicit `--socket`, `--remote`, or `--config`, selects direct sibling-engine control instead. Both executables must be in the same directory for direct control. An explicit socket needs an existing private parent directory. Custom/test launches do not join the desktop single instance.

### Controls

- Add mono or stereo channel strips. No fixed bus names or automatic microphone routes are imposed.
- Each strip has volume, noise-gate threshold, compressor threshold, effect enable controls, real input/output peak/RMS levels, mute, hide, and enable controls.
- **Input**, below each channel name, selects a live incoming route. Existing routes made in Routing appear here. **No routed input** removes this channel's explicit incoming routes, not outgoing routes or direct application connections to its virtual endpoint. Replacing multiple incoming routes requires confirmation; use Routing to retain a multi-source mix. Missing saved sources remain visible, and invalid feedback edits are rolled back.
- **Dynamics** opens gate threshold/attack/hold/release, compressor threshold/ratio/attack/release/makeup, and limiter ceiling/release controls. Effects can be added or removed.
- **Equalizer** opens the channel's parametric bands with frequency, gain, Q, enable, add, and remove controls.
- The tone-color pad controls 160 Hz warmth and 4.5 kHz brightness EQ bands, each within ±12 dB at Q 0.7. The popup has labelled slider alternatives and enable controls. This is ordinary EQ, not a copy of a proprietary algorithm. New channels include neutral color bands. A neutral reset does not add missing bands to an imported profile; the first non-neutral edit can add them and replace that channel's endpoints.
- **Routing** selects explicit hardware/application/channel connections. **Devices** separates system sound-card profiles from mixer-local aliases, visibility, mute, and participation controls. Input and output roles are labelled. Unsupported layouts and unavailable resources remain visible.
- There are no recording, cassette, system-default, or login-startup controls.

Controls are live. Valid slider, color-pad, checkbox, and routing changes are sent while the control is used, without an Apply button or mouse-release requirement. Disk saves are separate and debounced. Adding/removing channels, effects, or routes can replace affected audio endpoints; destructive removals require confirmation. Parameter-only changes use existing endpoints. Saved settings do not mean every route is ready: read channel/route states and errors.

GUI values snap to the control's declared step. The color pad uses the same 0.5 dB steps as its sliders; EQ frequency saves whole Hertz. Decimal steps are normalized before sending so settings contain values such as `1.3`, not floating-point step noise. Imported profiles, audio samples, meter calculations and test measurements are not rounded.

Control I/O runs on a blocking worker, never the GUI event loop. Only one request is active. Later input replaces the pending desired state rather than building a queue of old slider positions. Acknowledgements update confirmed state without overwriting newer input. Due status/meter polls get a turn during sustained edits. Channel names remain separate drafts until Enter or a confirmed final save: renaming replaces channel endpoints, so it must not happen on every keystroke. Invalid names/aliases remain separate from the valid profile and cannot block other audio controls.

Close, Stop engine, and Reload wait for pending writes and the final disk save. Offline or failed saves retain edits and show recovery actions; Reload permits an explicit discard when safe. Failed writes do not retry continuously. Uncertain writes are reconciled against their exact sent profile. Conflicts require explicit discard-and-reload. The profile pre-check and Update are not atomic across clients: use one editing client at a time.

With a registered panel host, closing the window saves and hides it while the indicator remains. The panel menu provides Open, Hide, Start/Stop, and Quit. **Quit Mixer** saves and exits only the frontend. If no host is available, close exits after saving rather than hiding an inaccessible window. A lost panel host restores a hidden window. Launching the application again restores the existing window. The compositor controls focus; check launcher activation on the workstation.

### Verification and limits

The integrated candidate passed formatting, locked builds, Clippy with warnings denied, and 126 Rust tests. Native lifecycle tests cover delayed saves, explicit flush, restart restore, signals, save failures and safe stop. The native device-profile tests and full backend audio harness also passed; current audio evidence is `/var/tmp/cosmos-backend-bkdobtdi`.

Private native Wayland/D-Bus checks covered indicator registration, hide/show, lost-host recovery, single-instance reopening, and final-save Quit with the engine still running. The final repeated hide/show and saved-control restore checks passed on Ubuntu 26.04's Weston 14.0.2, with live synthetic input/output levels. The actual GUI model produced short saved values for volume, color, compressor ratio and EQ frequency, and the engine restored them unchanged. Evidence and a final native capture are in `/var/tmp/cosmos-service-ui-02nf_b52`.

The GUI also rendered using Ubuntu 24.04's glibc 2.39 and Wayland libraries. Its older Weston 13 kiosk shell later crashed during a reopen cycle; the core trace was in libweston/kiosk-shell, and the engine kept running with the saved profile intact. Do not count that failed cycle as a pass or treat it as support for stock Ubuntu 24.04. The repeated final cycle used the supported newer compositor.

Earlier real-widget simulations exercised input selection and measured audio/silence, OS-profile confirmation/cancel/off/restore, live sliders without mouse release, coalesced changes, and EQ/color restore. Evidence remains in `/var/tmp/cosmos-workstation-ui-4z7l_k4_` and `/var/tmp/cosmos-live-jxat2fse`. These are local test artifacts, not repository assets.

`python3 tests/installer.py` checks verified staging, architecture/glibc/library rejection, path quoting, old-client stop, rollback, restart failure, ownership and retained settings. It uses an isolated service-manager fixture. The rendered user unit also passes `systemd-analyze --user verify`. A real isolated user manager cannot start in this toolbox: creating its `init.scope` fails with cgroup permission denied. Real start/enable/login restore must be checked on the workstation; no host cgroup or service settings were changed to bypass that limit.

Desktop indicator rendering, launcher focus, physical devices, Bluetooth, independent clocks, latency and soak behavior still need workstation checks. AT-SPI/screen-reader operation remains unverified: the available launcher and pinned AccessKit use different enable-property names. Do not claim release readiness or universal desktop support.

## Workstation test binaries

### Platform and dependency baseline

The supplied architectures are **x86-64/amd64** and **aarch64/ARM64**. These are 64-bit Linux binaries, not 32-bit x86 or the x32 ABI. They require glibc, Wayland, PipeWire with its native LV2 host, LSP LV2 plugins, and a systemd user manager for managed mode. `--standalone` remains available without service integration.

The server update installed glibc 2.44. Do not copy a normal Fedora 45 build and assume it works on the Fedora 44 workstation's glibc 2.43. The test binaries were instead linked against verified Ubuntu 24.04 sysroots with the pinned Rust toolchain. Both GUI architectures require symbols through **GLIBC_2.39**; both engines require symbols through **GLIBC_2.34**. The bundle declares a common glibc floor of 2.39. Never replace the workstation's glibc with server libraries.

**Stock Ubuntu support starts at 26.04**, not 24.04. Noble disables PipeWire LV2 support; installing only LSP cannot fix that. Resolute supplies the host in `libspa-0.2-modules-extra`. This was checked against signed package indexes and actual native processing, not version numbers alone. See the [Noble build rules](https://git.launchpad.net/ubuntu/+source/pipewire/plain/debian/rules?h=applied/ubuntu/noble-updates) and [Resolute build rules](https://git.launchpad.net/ubuntu/+source/pipewire/plain/debian/rules?h=applied/ubuntu/resolute).

Ubuntu 26.04 runtime checks used glibc 2.43, PipeWire 1.6.2 and LSP 1.2.27. x86-64 mono/stereo processing passed. ARM64 native effects and live -6 dB gain passed under QEMU; this is not physical ARM latency evidence. Logs are at `/var/tmp/cosmos-ubuntu26-audio-mtw_3ww_` and `/var/tmp/cosmos-arm64-audio-bkun6l1_`. Other distributions need the same runtime facilities and their own validation; a glibc number alone does not prove audio support.

For local development only, `cargo build --workspace --release --locked` builds against the current toolbox. Portable delivery must use the older sysroot and target compiler, then check ELF architecture, versioned symbols, and actual runtime behavior. Do not raise the portable ABI floor by silently using host libraries.

### Install, update, and remove

The verified bundle contains both executables, `install.sh`, the service/desktop/icon templates, `data/compatibility.conf`, and `SHA256SUMS`. This is a private workstation test bundle, not a published Release or distribution package. No package publishing or new distribution CI/CD was added.

Run the installer as the desktop user, never with sudo. It requires Python 3.9 or later, `getconf`, `ldd`, and a working systemd user manager. It checks hashes, ELF architecture, glibc and linked libraries before changing the install. Runtime-loaded Wayland/LV2 modules and LSP plugins still require the packages listed below; `ldd` does not validate them. The script does not run a package manager, change PipeWire/WirePlumber services, change system defaults, or enable lingering.

```sh
sha256sum -c SHA256SUMS
./install.sh install --enable
```

`--enable` is explicit consent to start the engine now and at later user logins. Without it, a fresh install leaves the engine stopped and login startup disabled. Audio resumes when the user session and its PipeWire service are available, not before login. The frontend and indicator start when the user launches the app; the installer does not open a window at every login.

Files go to `~/.local/opt/cosmos-audio-mixer/releases/`, selected through `current`. The user unit is under `$XDG_CONFIG_HOME/systemd/user/`; the desktop entry and independent icon are under `$XDG_DATA_HOME`. Default XDG paths are used when unset. Updates stage and verify both binaries before stopping the old engine, preserve the service's run/enable state, and attempt rollback on failure. A save failure blocks replacement. An unowned old install directory or standalone engine is not silently taken over.

```sh
# Update from a new downloaded bundle; preserve current startup/run choices.
./install.sh install

# Remove the managed binaries, launcher, icon and service. Keep mixer settings.
"$HOME/.local/opt/cosmos-audio-mixer/current/install.sh" uninstall
```

Uninstall retains `$XDG_CONFIG_HOME/cosmos-audio-mixer/` and its profiles/backups. The service restarts a failed engine with a two-second delay, limited to three starts in 30 seconds. After fixing a repeated startup failure, use `systemctl --user reset-failed cosmos-audio-mixer.service` before Start. Service stop sends SIGTERM; the final-save failure/timeout limit above still applies.

### Copy from this server

The corrected server test bundles are under `/home/jp/Projects/cosmos-audio-mixer-test/service-and-indicator-r2/`, with `x86_64` and `aarch64` subdirectories. The following commands are for the supplied Fedora 44 x86-64 workstation, in its normal desktop session:

```sh
sudo dnf install python3 pipewire-libs pipewire-module-filter-chain-lv2 lsp-plugins-lv2 libxkbcommon 'libwayland-client.so.0()(64bit)'
mkdir -p "$HOME/Downloads"
download=$(mktemp -d "$HOME/Downloads/cosmos-service.XXXXXX")
scp -r jp@192.168.0.2:/home/jp/Projects/cosmos-audio-mixer-test/service-and-indicator-r2/x86_64 "$download/"
cd "$download/x86_64"
sha256sum -c SHA256SUMS
```

Close the old frontend first. If the previous executables were copied directly into `~/.local/opt/cosmos-audio-mixer/`, preserve that unowned directory before the first managed install:

```sh
(
    set -eu
    old="$HOME/.local/opt/cosmos-audio-mixer"
    if [ -d "$old" ] && [ ! -f "$old/.cosmos-install.json" ]; then
        test -x "$old/cosmos-engine"
        if [ -S "${XDG_RUNTIME_DIR:?}/cosmos-audio-mixer/control.sock" ]; then
            "$old/cosmos-engine" stop
        fi
        backup=$(mktemp -d "$HOME/.local/opt/cosmos-before-service.XXXXXX")
        mv -- "$old" "$backup/files"
        printf 'Old executables retained in %s/files\n' "$backup"
    fi
)
```

If that command fails, keep the old files and resolve the error before installing. The saved mixer profile is separate and is not moved. Then run:

```sh
./install.sh install --enable
systemctl --user --no-pager status cosmos-audio-mixer.service
"$HOME/.local/opt/cosmos-audio-mixer/current/cosmos-audio-mixer"
```

The terminal launch remains attached to that terminal. Prefer the desktop application menu for normal use. In another terminal, follow service output with:

```sh
journalctl --user -u cosmos-audio-mixer.service -f
```

The launcher is also available in the desktop application menu. The default service log level is Warn. For Info/Debug, edit the user unit with `systemctl --user edit cosmos-audio-mixer.service`, clear `ExecStart=`, then set the installed engine command with `run --log-level info` or `debug`, reload and restart the service. A restart interrupts mixer audio; save successfully first.

GNOME needs a StatusNotifier/AppIndicator host. On Fedora:

```sh
sudo dnf install gnome-shell-extension-appindicator
gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com
```

If GNOME does not discover the newly installed extension, log out and back in, then enable it. The [extension](https://extensions.gnome.org/extension/615/appindicator-support/) is not required on a desktop that already supplies a working host.

On Ubuntu 26.04, the equivalent native audio prerequisites include `pipewire`, `libpipewire-0.3-0t64`, `libspa-0.2-modules-extra`, `lsp-plugins-lv2`, `libxkbcommon0`, `libwayland-client0`, Python 3 and systemd. Ubuntu GNOME can use `gnome-shell-extension-appindicator`. No Rust toolchain, GTK4, COSMIC desktop or Toolbox is required to run these binaries.

### Workstation acceptance

At low volume, check live volume, mute, gate, compressor, EQ and tone color. Wait two seconds, then check restore. Also change a setting and immediately close/quit: it must save without waiting for the idle timer. With the panel host, close hides and Open restores; Quit removes only the frontend. Test service stop/start and logout/login restore, including every effect's parameters and enable state. Confirm launcher focus and panel rendering. Do not merge the PR until the owner approves these workstation results.

System card profiles remain OS-owned; the mixer does not replay a destructive OS profile action on startup. Test Off/restore only on a card that you intend to change for all applications. No installation command should restart desktop audio services or change audio defaults.

