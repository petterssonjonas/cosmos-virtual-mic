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

## Native packages: owner action required

The setup session reached `cosmos`, but `sudo -n true` reported that a password is required. Native dependencies were NOT installed by that session. Run this interactively inside `cosmos`:

```bash
mapfile -t packages < dependencies-fedora.txt
sudo dnf install "${packages[@]}"
```

`dependencies-fedora.txt` is the authoritative initial Fedora package list. Its package names were found in the container's configured Fedora 45 repositories. It includes:

- C/C++ tools, CMake, pkg-config, and Clang development support for native dependencies and generated bindings.
- Expat, Fontconfig, FreeType, xkbcommon, Wayland, and GL/EGL development libraries for libcosmic's native dependency stack.
- PipeWire libraries, headers, daemon, diagnostic tools, and the separately packaged `pipewire-module-filter-chain-lv2` host; WirePlumber for isolated integration tests; LSP LV2 effects and Lilv inspection utilities.

Installing these packages is not permission to start or enable another audio session manager against the host graph. Do not run `systemctl --user enable/start/restart pipewire` or WirePlumber setup commands as part of environment preparation. Toolbox can expose host session sockets and the host user service manager.

This is a source-informed baseline, not a verified complete application build manifest. There is no Cargo application yet. Add any demonstrated missing dependencies to this list and explain them here during the first real build. Do not install GTK4, Qt, Tauri, Node, Bun, or a full desktop environment for the current frontend.

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

Once application code exists, use formatting, Clippy, compilation, relevant contract/behavior tests, real audio-path smoke checks, and GUI interaction checks. Use `--locked` where applicable after generating the lockfile. Avoid `--all-features` if the selected toolkit has mutually exclusive feature combinations; test the supported configurations explicitly.
