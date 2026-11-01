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

This is the native dependency baseline. The Rust engine uses PipeWire 0.10.1 bindings with the `v1_2_0` API feature against native PipeWire 1.6.11. The native LV2 filter host links successfully to Lilv; direct Lilv development headers are not needed for this integration. The libcosmic application has not been built, so its complete native build requirements remain unverified. Do not install GTK4, Qt, Tauri, Node, Bun, or a full desktop environment for the current frontend.

DNF resolves package versions from the enabled repositories. This file is NOT an RPM version lock. When CI/release packaging is introduced, use a pinned container image/repository snapshot and record resolved RPM versions; do not claim fully reproducible native dependencies yet.

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

For a fresh toolbox/home without this installation, Fedora provides a `rustup` package. Install it with `sudo dnf install rustup`, source `dev-env.sh`, and invoke `/usr/bin/rustup` explicitly to avoid existing mise shims. Install the version/components specified in `rust-toolchain.toml`. Confirm Cargo/rustc command resolution before building; the current machine already has dedicated rustup proxy binaries.

## Upstream dependency pin

Candidate libcosmic revision inspected during setup:

`95236a944bfd51c63a2fc65fe50a592c47168d40`

Source: <https://github.com/pop-os/libcosmic/commit/95236a944bfd51c63a2fc65fe50a592c47168d40>

The current manifest declares Rust 1.93 and uses Git dependencies and an iced submodule. Pin a tested revision in the future Cargo manifest and retain `Cargo.lock`, including transitive Git revisions. This candidate has NOT been compiled for our application. Select features for a standalone Wayland app, not a panel applet. Accessibility and operation without COSMIC services must be verified. No source fork is authorized.

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

The workspace contains `cosmos-control` (toolkit-independent protocol, profile model, and client) and `cosmos-engine` (native discovery, DSP hosting, routing, metering, and persistence). Build after sourcing `dev-env.sh`:

```sh
cargo build --workspace --locked
./target/debug/cosmos-engine --help
./tests/engine-lifecycle.py
./tests/backend-audio.py --keep
```

The lifecycle test uses a private server and three synthetic nodes. It does not connect to desktop audio. It checks server absence, mono/stereo/unsupported layouts, duplicate descriptions, three server restarts, stale-node removal, duplicate engine rejection, slow/oversized clients, crash recovery, and explicit stop. It also checks that existing user files survive and that non-private socket directories are rejected.

The executable accepts `run [--socket PATH] [--remote NAME] [--config PATH]`, `status|list|profile|stop [--socket PATH]`, and `apply FILE [--socket PATH]`. `run` stays in the foreground; launch it as a separate process when needed. Closing a control client does not stop it. Login startup is not installed or enabled. The default socket is `$XDG_RUNTIME_DIR/cosmos-audio-mixer/control.sock`. Without an explicit remote or `PIPEWIRE_REMOTE`, `run` connects to the default PipeWire server and restores the saved profile. An empty profile creates no audio path. Automated checks always use a private remote, runtime, HOME, and configuration.

Lifecycle invariants:

- Only the engine that holds the persistent socket lock can own the control endpoint. A stale socket can be reclaimed after a crash; ordinary files and symlinks are not replaced.
- Socket parents must be private and user-owned. Ancestors must be root/user-owned, with write access restricted unless sticky. Toolbox maps `/tmp` ownership to UID 65534 here; lifecycle tests use `/var/tmp` rather than weaken these checks.
- Every connection attempt has a new generation. Disconnect removes observed nodes and devices. Numeric IDs are valid only in that generation and are not durable device selection keys.
- Only a core `-EPIPE` error marks a lost native connection. Other native resource errors remain visible as bounded diagnostics and trigger graph reconciliation. Route and meter links are removed before their endpoint owners; ordinary graph changes do not reset unrelated buses.
- Reconnect delay grows from 250 ms to at most 2 s. Handshake timeout is 2 s. Control requests remain available while PipeWire is absent.
- IPC accepts at most eight active clients, 68 KiB requests, and 2 MiB responses. Complete serialized profiles are limited to 64 KiB. Server requests have a 1 s deadline; client operations have a 2 s absolute deadline. Stop has a bounded acknowledgement period.
- Discovery reports identity candidates and channel semantics. Routing uses explicit selectors, never defaults or saved numeric IDs. Missing or ambiguous sources and unsupported layouts produce an error/unavailable state, not another microphone or a downmix.

The locked dependency metadata contains 65 packages, including the two local crates. All external sources are crates.io registry packages; all have licence declarations. This metadata inspection is not a full licence/source/notice audit and does not authorize distribution. libcosmic is not yet a resolved dependency.

### Profiles and processing

`profile` returns the desired profile. `apply FILE` validates the complete replacement before saving or changing the graph. Acceptance means the desired state was saved; it does not mean that hardware, plugins, or links are ready. Check `status.backend` for bus and route states and `status.config_error` for persistence errors.

The version-1 profile contains `buses`, `devices`, and `routes`. See `crates/control/src/model.rs` for the strict schema and bounds, and `tests/backend-audio.py` for runnable synthetic examples.

- Each bus has an independent ID/name, explicit `mono` or `stereo` channels, enable/hidden/mute flags, and an ordered list of effect instances.
- Each bus publishes `cosmos.bus.<id>.input` as a playback sink and `cosmos.bus.<id>.output` as a recording source. No system defaults change. Applications can use the input directly, or receive explicit engine-owned routes.
- Supported effects are gate, compressor, one parametric EQ band per instance, and limiter. Multiple EQ instances form a multi-band chain. Gate controls include threshold, attack, hold, and release. Parameters use explicit dB, Hz, and milliseconds and must fit the installed native plugin ranges.
- Parameter changes and mute use native live controls and retain endpoint identity. Effect topology/channel changes safely replace the affected bus and can interrupt its clients. Endpoint logical names remain stable.
- Device selectors use the identity properties from discovery. Device aliases, visibility, enable state, and mute stay local to this mixer. Hiding does not mute or disable. Hardware names, profiles, volume, and availability outside the mixer are not changed.
- Routes connect selected hardware/application nodes and named buses. Validation rejects feedback and overlapping selector cycles before persistence. Resolved graph validation rejects identity aliases that would create a cycle.
- Plugins run in the engine process, not the desktop PipeWire daemon. Native plugin failure can still terminate that engine process. No raw-audio fallback is created when required processing fails.
- Input-feed and output meters report real per-channel linear peak/RMS values, sample counts, and freshness. They use bounded replaceable snapshots, not an event queue. Input metering follows actual incoming links; it does not rely on the native sink-monitor path. These values are not an exact LV2 gate-open or gain-reduction signal.

### Persistence

The default profile is `$XDG_CONFIG_HOME/cosmos-audio-mixer/profile.json`, or `$HOME/.config/cosmos-audio-mixer/profile.json` when the XDG variable is absent. Use `--config` for an explicit private path. The directory must be private and user-owned; stored files are mode 0600. A separate persistent lock prevents two engines from writing the same profile.

Saving writes and syncs a private temporary file, atomically replaces the profile, and syncs its directory. Corrupt or unsupported profiles are reported and are not silently overwritten. Explicit Apply preserves the invalid original with a no-replace backup rename before replacement. An interrupted backup leaves the original recoverable; a later Apply can complete recovery. A directory-sync error after replacement is reported separately because the new desired state was already accepted.

### Backend verification

`./tests/backend-audio.py --keep` exercises the Rust executable, not a substitute filter host. It records synthetic samples and retains logs, graph snapshots, status, profiles, and audio files in the printed private directory.

Verified paths include:

- Apply while the server is absent; large bounded profile transfer; rejection of invalid parameters and feedback without changing saved state.
- Real gate and ordered gate/compressor/EQ/limiter processing in mono and stereo; live thresholds; 44.1 kHz input into the 48 kHz graph.
- Real input/output meters, recovery after removal of a meter node, and continued input metering when one of two sources leaves.
- Source disappearance/return, ambiguous identity, unsupported six-channel input, and no unrelated-source substitution. Unavailable output is silent.
- Direct application playback, named bus-to-bus routing, independent hide/mute/enable behavior, and continued audio on an unrelated bus when another route fails.
- An unchanged connection generation across source removal, meter removal, and bus topology changes while the private server remains running.
- Engine crash/restart and two private PipeWire restarts with automatic restoration from saved state.
- Missing native plugins with no bypass endpoints, plugin restoration, and corrupt-profile preservation/recovery.

Physical microphone hotplug, Bluetooth transitions, independent hardware clocks, end-to-end latency, long-running resource growth, and frontend responsiveness still require separate authorized validation. The private tests do not capture a live microphone or change desktop audio.
