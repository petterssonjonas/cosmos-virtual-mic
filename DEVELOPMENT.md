# Development environment

Run project commands inside the Fedora `cosmos` toolbox. The application itself is not intended to require Toolbox, COSMIC desktop services, or Flatpak.

## Enter and configure

From a host terminal:

```sh
toolbox enter cosmos
cd ~/Projects/cosmos-virtual-mic
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

PRs to `development` and `main` run `.github/workflows/validate.yml`: the runner creates a `cosmos` toolbox from a pinned Fedora image, installs the baseline packages, and checks formatting, locked compilation, tests, Clippy, installer failure paths, and isolated session policy. This validation does not build distribution packages. Promotions to `main` also run engine lifecycle, synthetic backend audio, and read-only device checks. Physical microphone and desktop acceptance limits remain as listed below.

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

## Cosmos Virtual Mic

Cosmos Virtual Mic has one selected microphone, one effects chain, and one public virtual microphone. The gray channel-1 card is the entire visible window, not a card inside a toolkit shell. Mixer, routing, device-management, and panel-indicator interfaces are removed. The remote `full-mixer` branch preserves the previous full application at `7a5d2ad`. Continued development uses `development`; reviewed releases use `main`.

The workspace contains `cosmos-control` (toolkit-independent model and protocol), `cosmos-engine` (native audio engine), and `cosmos-virtual-mic` (libcosmic frontend). The engine package builds the `cosmos-virtual-mic-engine` executable.

### Build and run

Inside `cosmos`, after sourcing `dev-env.sh`:

```sh
cargo build --workspace --locked
./target/debug/cosmos-virtual-mic-engine --help
./target/debug/cosmos-virtual-mic --standalone
```

Use a Wayland session for the frontend. `--standalone` is for source testing: Start launches the sibling engine executable without systemd. Both executables must be in the same directory. Closing the frontend leaves that engine running; use Stop engine to stop it.

A normal installed launch uses `cosmos-virtual-mic.service` and one frontend instance. Start starts the user service. Stop saves through the control socket before it stops the engine and service. Service operations run on a worker, not the GUI event loop. Explicit `--socket`, `--remote`, or `--config` options also select direct control, not the installed service.

Do not use an ordinary development launch for fault injection: without an explicit remote it connects to the desktop PipeWire instance. Automated audio checks use private servers and synthetic sources.

### Microphone window

The card uses the original **286 × 760 logical-pixel** size. Start/Stop and Close are inside its top row. Drag the empty space between those buttons to move the window. There is no outer header bar, frosted wrapper, status area, or footer. Errors and save-recovery controls stay inside the card; audio controls and effect dialogs scroll when necessary.

The window is deliberately fixed-size. It sends equal Wayland minimum and maximum sizes, and disables resizing. COSMIC's [fixed-size window classification](https://github.com/pop-os/cosmic-comp/blob/9feaa865/src/shell/layout/mod.rs#L46-L54) uses these limits to select floating placement. No compositor configuration or desktop rule is changed. The actual limits and geometry were observed in a private Wayland trace; placement on the workstation's COSMIC session still needs the owner's check.


- Start the engine to load its settings. Audio controls are not shown before the first profile loads, so early edits cannot be replaced by that profile.
- Select **Microphone input** explicitly. No default microphone or replacement microphone is selected automatically.
- The selected known mono/stereo layout sets the virtual microphone layout. Unknown formats require discovery before selection. Unsupported layouts are reported, not remapped.
- Edit the virtual microphone name and press Enter. The name is system-visible. Renaming or changing effect topology can interrupt clients.
- Gain, mute, noise gate, compressor, parametric EQ, limiter, tone color, and input/output levels remain from the original strip. Effects open as overlays in the same window.
- Gain, sliders, color, and effect switches update live. The name stays a draft until Enter or a final save. GUI steps do not round imported profiles, audio samples, or meter values.
- The top-right **Close** button saves and exits only the frontend. There is no hidden tray process. The engine keeps running.
- Stop and Close wait for pending updates and a final durable save. A failed save retains edits and keeps the window open. Reload allows an explicit discard when it is safe.
- The window shows microphone route state and errors, including unavailable, ambiguous, or mismatched inputs. When the engine or server is offline, a saved selection is not reported as proven missing.

The card-only native capture and widget evidence is in `/var/tmp/cosmos-card-ui-fwfnnkwo/`. `wayland-screenshot-2026-11-04_21-23-26.png` shows only the gray card with synthetic levels. Real widget events covered the draggable header, microphone selection, Start/Stop, EQ/dynamics modal dialogs, scrolling to and adding a limiter, mute/gain, and a final name save on Close. The engine remained active after both widget Close and native frontend exit; private processes were then stopped. Temporary drivers and dependencies were removed.

The initial channel is `channel-1`, named `Cosmos Virtual Mic`, with mono audio and no selected input. Gate and compressor start bypassed. The bass/mid/treble and color bands start neutral; the limiter starts enabled at -1 dB. These are initial settings, not a replacement for a saved profile.

### Engine and control protocol

```sh
./target/debug/cosmos-virtual-mic-engine run
./target/debug/cosmos-virtual-mic-engine status
./target/debug/cosmos-virtual-mic-engine profile
./target/debug/cosmos-virtual-mic-engine update profile.json
./target/debug/cosmos-virtual-mic-engine flush
./target/debug/cosmos-virtual-mic-engine stop
```

The engine accepts `run [--socket PATH] [--remote NAME] [--config PATH]`, `status|list|profile|flush|stop [--socket PATH]`, and `apply|update FILE [--socket PATH]`. `run` stays in the foreground for systemd. The protocol is **version 4**; install the frontend and engine together. OS sound-card profile mutation is removed. Discovery is read-only.

The version-1 saved profile requires exactly one visible bus with ID `channel-1`, at most one Hardware-to-channel-1 route, and an empty `devices` list. Other buses, output routes, bus-to-bus routes, and mixer-local device settings are rejected. Original multi-channel mixer profiles are not truncated or imported automatically.

The selected node must be an `Audio/Source`. Application playback/recording streams and sinks cannot be microphone sources. Mono and stereo remain explicit; mismatched, ambiguous, or unavailable sources produce an error and controlled silence.

The only public audio device is `cosmos.virtual-mic.channel-1.output`, an `Audio/Source`. `cosmos.virtual-mic.channel-1.input` is the internal DSP capture stream, not a public playback device. No selection, disabled input, device loss, or failed route keeps the output silent. An unexpected external link into the capture stream also silences the output and reports an error; removing that link permits recovery without replacing the virtual microphone.

Gate, compressor, EQ, and limiter use native LSP LV2 plugins in the engine, not the desktop daemon. The fader precedes a trailing limiter. A final safety stage enforces mute and unavailable silence. Parameter-only changes retain endpoint identity. Effect topology or channel-layout changes can replace endpoints. Missing mandatory plugins never create a raw-audio fallback.

The engine does not change system defaults, hardware names, card profiles, or unrelated routes. Desktop policy can still react to new or removed endpoints under its own settings. Do not claim uninterrupted audio while the selected microphone or server is absent.

### Persistence and isolation

The new application uses separate paths:

| Resource | Path or name |
| --- | --- |
| Control socket | `$XDG_RUNTIME_DIR/cosmos-virtual-mic/control.sock` |
| Profile | `${XDG_CONFIG_HOME:-$HOME/.config}/cosmos-virtual-mic/profile.json` |
| User service | `cosmos-virtual-mic.service` |
| Frontend | `cosmos-virtual-mic` |
| Engine | `cosmos-virtual-mic-engine` |
| Desktop ID | `io.github.petterssonjonas.CosmosVirtualMic` |

The original `cosmos-audio-mixer` installation, service, and profile are not migrated, stopped, or overwritten.

`update` applies live state and saves the complete profile after two idle seconds. Acceptance is not a durability acknowledgement: check `config_pending` and `config_error`. `flush` saves immediately. `apply` validates and saves before changing the graph, except that a directory-sync error after replacement must adopt the file already written.

Profiles and socket parents must be private and user-owned. Independent locks prevent duplicate socket owners and profile writers. Saving writes and syncs a private temporary file, atomically replaces the profile, and syncs its directory. Corrupt or unsupported profiles remain intact until explicit recovery preserves a backup. Failed saves retain state and report an error without a busy retry loop.

SIGINT/SIGTERM and fatal exits attempt a final save. Failed signal-triggered saves keep the engine running. SIGKILL, power loss, or systemd's stop timeout can still lose unsaved RAM. The frontend's profile pre-check and Update are not atomic across multiple clients; use one editing client at a time.

PipeWire reconnect uses bounded delays. Control requests remain available while the server is absent. Disconnect clears observed nodes and devices; saved selectors use durable identity properties, not numeric node IDs. Missing or ambiguous identities never trigger microphone substitution.

### Service and installation

`data/cosmos-virtual-mic.service` and the desktop file are installer templates. For an authorized distribution bundle, `install.sh` checks checksums, architecture, and both executables before it replaces files. It installs the executables in `~/.local/bin`, the user unit under `$XDG_CONFIG_HOME/systemd/user`, and the launcher/icon/retained installer under `$XDG_DATA_HOME`.

The installer saves and stops only the matching installed virtual-mic engine before an update or removal. It retains profile data and unrelated files. A save failure prevents replacement; a later failure retains previous files in its reported staging directory. There is no automatic rollback or legacy mixer migration.

`install --enable` is an explicit opt-in to engine startup now and at login. A fresh install without it leaves login startup disabled. Updates preserve existing startup choices. The service restarts failures after two seconds, limited to three starts in 30 seconds. After correcting repeated startup failures, use `systemctl --user reset-failed cosmos-virtual-mic.service`.

```sh
journalctl --user -u cosmos-virtual-mic.service -f
bash "${XDG_DATA_HOME:-$HOME/.local/share}/cosmos-virtual-mic/install.sh" uninstall
```

Both executables accept `--log-level error|warn|info|debug` and `--log-stdout`. Default diagnostics use Warn on stderr. JSON client commands reject `--log-stdout`. Service logs go to the journal; GUI logging flags do not change a running service.

Public installation instructions and runtime requirements are in [README.md](README.md). Release binaries use the Fedora 45 x86_64 baseline and require glibc 2.44 or later, Wayland, PipeWire with native LV2 hosting, LSP LV2 plugins, and a systemd user manager for managed mode. The DEB is not compatible with stock Ubuntu 24.04 or 26.04. Neither a tray extension nor the COSMIC desktop is required.

### Release CD

`.github/workflows/release.yml` runs only when a GitHub Release is **published**. A pushed tag alone must not build distribution packages. Publish from `main` only after reviewed promotion and passing validation. `v0.2` maps to package version `0.2.0`; later tags can use `vMAJOR.MINOR` or `vMAJOR.MINOR.PATCH`.

CD builds both executables with the pinned toolchain and `Cargo.lock` inside `cosmos`. It audits dependency licences and retains notices, then creates an x86_64 RPM, amd64 DEB, and binary tarball. The assets include the release download installer and a SHA-256 manifest. They are attached to the triggering Release. Package publication and ordinary PR validation are separate.

The licence audit covers the Linux normal/build graph selected by Cargo. It rejects missing or unaccepted declarations. `packaging/license-clarifications.json` records reviewed upstream omissions with exact source identities, manifest hashes, locked Git revisions, and hashed licence evidence. These records must be reviewed again when a dependency changes. The inventory is not a vulnerability scan or a legal opinion. The bundle and native packages retain the full texts; per-user installation retains `LICENSE` and the complete `THIRD-PARTY-NOTICES.txt`.

`install-release.sh` is the curl-to-Bash entry point. It resolves or accepts a release tag, verifies the archive checksum, rejects unsafe archive members, and runs the existing bundle installer. The local `install.sh` still owns save-before-stop, file replacement, service installation, and removal. Run `python3 tests/release-installer.py` and `python3 tests/installer.py` for isolated install and failure-path checks.

Native packages install under `/usr`, including a systemd user unit. They have no start/enable hooks. Do not mix a native package with a per-user installation. Neither method enables login startup by default. Package generation and release-asset verification must not operate the host audio graph or service manager.

### Card-only workstation candidate

The updated optimized x86-64 test copy is prepared in `/home/jp/Projects/cosmos-audio-mixer-test/card-only-workstation-7pgyyq8y/`, for transfer with `~/.ssh/id_ed25519_workstation` to `jp@192.168.0.10:/home/jp/Downloads/cosmos-virtual-mic-card.fYsi9k/`. `BUILD-INFO.json` records the candidate revision and source hashes; `SHA256SUMS` covers the copied files. This is a private test copy, not a published Release. No remote installation or audio operation is part of the transfer.

On the workstation, close the earlier frontend, then run without sudo:

```sh
cd ~/Downloads/cosmos-virtual-mic-card.fYsi9k
sha256sum --check --strict SHA256SUMS &&
bash install.sh install &&
"$HOME/.local/bin/cosmos-virtual-mic"
```

Use Start engine in the card and check that COSMIC floats the compact window. Login startup is not enabled by a fresh install without `--enable`.

### Earlier workstation test copy — before the card-only correction

Built the current uncommitted `mic-only` source with Rust 1.99.1 using `cargo build --workspace --release --locked` inside `cosmos`. The target workstation was checked over SSH: `jp@192.168.0.10`, hostname `Workstation`, Fedora 45, x86-64, glibc 2.44. Its PipeWire/LV2 host, LSP plugins, xkbcommon, and Wayland client library are installed.

Local files: `/home/jp/Projects/cosmos-audio-mixer-test/mic-only-workstation-7xo6tity/`. Copied with `~/.ssh/id_ed25519_workstation` to `/home/jp/Downloads/cosmos-virtual-mic-test.rzgbAt/` on that workstation. The private test directory contains both optimized executables, the Bash installer, service/desktop/icon templates, compatibility metadata, the project licence, build provenance, and `SHA256SUMS`. It is not a published Release or distribution package.

The optimized engine passed isolated synthetic mono/stereo audio, gate suppression, unexpected-input silence, and recovery with stable endpoint identity. Both executables started with `--help` locally and on the workstation. All copied checksums and the checksum-manifest hash matched; linked libraries resolved on the workstation. Evidence: `/var/tmp/cosmos-mic-release-smoke-r5z0ord0`. No remote installer, GUI, service operation, or audio connection was run.

On the workstation, without sudo:

```sh
cd ~/Downloads/cosmos-virtual-mic-test.rzgbAt
sha256sum --check --strict SHA256SUMS &&
bash install.sh install &&
"$HOME/.local/bin/cosmos-virtual-mic"
```

Use Start engine in the window, then select the intended microphone. A fresh `install` without `--enable` does not enable login startup. The installer leaves the original mixer installation and its profile unchanged.


### Validation and workstation acceptance

```sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 tests/engine-lifecycle.py
python3 tests/device-profiles.py
python3 tests/backend-audio.py --keep
python3 tests/session-policy.py --keep
bash -n install.sh
python3 tests/installer.py
```

The lifecycle harness checks private socket ownership, server absence/restarts, layout discovery, bounded clients, save debounce, signal flush, failed-save retention, crash recovery, and safe stop. The device harness checks read-only native discovery and rejection of removed OS mutations. The audio harness checks the one-microphone contract, actual DSP samples, gain, limiter, mute/unavailable silence, extra-input rejection, source recovery, mono/stereo, persistence, plugin failure, and corrupt-profile preservation.

The final card-only candidate passed formatting, locked workspace builds, **114 Rust tests**, and Clippy with warnings denied. The final lifecycle/save-failure, mono/stereo policy-startup/restart, and installer checks passed. The policy run exposed a route-creation error that was incorrectly retained as a discovery fault. Unbound link proxy failures now stay with the runtime's existing link error and bounded retry; real discovery/permission/transport errors remain visible. A regression test checks that link failures neither latch nor clear unrelated discovery errors. Final policy evidence: `/var/tmp/cosmos-policy-23_9qke_`.

A temporary real-widget driver passed microphone dropdown selection, Start/Stop, effects overlays, mute/gain, name editing, final-save Close, and independent engine lifetime. The driver and its temporary dependencies were removed. The actual application rendered under private Weston 14 with live synthetic input/output levels. A final native capture is `/var/tmp/cosmos-mic-ui-7qra27k1/wayland-screenshot-2026-11-04_16-41-59.png`. The engine remained active after frontend exit and was then stopped explicitly.

Audio evidence is in `/var/tmp/cosmos-backend-1foejlfw`; policy evidence is in `/var/tmp/cosmos-policy-905yn3za`; widget/persistence evidence is in `/var/tmp/cosmos-mic-widgets-68ez2gp_`. All processes were test-owned and stopped. These are local evidence paths, not source assets or distribution packages.

The policy harness uses private WirePlumber with hardware monitors and D-Bus disabled. It checks unrelated playback device changes, normal virtual-microphone recording, mono/stereo startup orders, and recovery after policy restart. Recovery can interrupt microphone audio; finite sample checks start after the input path recovers. Each port has one configuration owner: the engine configures its internal capture and meter ports, while desktop policy configures the public source.

Installer checks use a private service-manager fixture, real ELF test clients, `systemd-analyze --user verify`, and GIO launcher parsing. They check original-mixer isolation and retained settings. A real isolated systemd user manager cannot run in this toolbox because its cgroup is not delegated. Service start/enable/login restore must be checked on the workstation; do not change host cgroups or audio services for the test.

At low volume, select the intended physical microphone and choose the virtual microphone in a recording application. Check live gain, mute, gate, compressor, EQ, color, and meters. Close/reopen immediately after an edit and check saved settings while audio continues. Test Stop/Start, unplug/replug of the selected microphone, and explicit unavailable states. Test opt-in login restore only when desired.

Physical microphones, Bluetooth, independent clocks, latency, long-running stability, launcher focus, and screen-reader behavior still need workstation checks. Headless Wayland rendering and synthetic audio do not prove these properties.
