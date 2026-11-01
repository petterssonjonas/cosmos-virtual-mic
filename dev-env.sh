# SPDX-License-Identifier: MPL-2.0
# Source this file from Bash inside the cosmos toolbox; do not execute it.
if [[ ! -f /run/.containerenv ]] || ! (
    source /run/.containerenv
    [[ "${name:-}" == "cosmos" ]]
); then
    printf '%s\n' 'Enter the cosmos toolbox before sourcing dev-env.sh.' >&2
    return 1
fi

# Toolbox shares HOME. Keep this toolchain separate from host/mise toolchains.
export CARGO_HOME="${XDG_DATA_HOME:-$HOME/.local/share}/cosmos-toolchain/cargo"
export RUSTUP_HOME="${XDG_DATA_HOME:-$HOME/.local/share}/cosmos-toolchain/rustup"
case ":$PATH:" in
    *":$CARGO_HOME/bin:"*) ;;
    *) export PATH="$CARGO_HOME/bin:$PATH" ;;
esac
hash -r
