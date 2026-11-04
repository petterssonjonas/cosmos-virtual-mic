#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
[ -n "${BASH_VERSION:-}" ] || exec bash "$0" "$@"
set -euo pipefail
umask 077

fail() { printf 'Cosmos installer: %s\n' "$*" >&2; exit 1; }
usage='Usage: bash install.sh install [--enable] | uninstall | --help'
action=${1:-install}
[[ $action != --help && $action != -h ]] || { printf '%s\n' "$usage"; exit; }
[[ $# -le 2 && ( $action == install && ( ${2:-} == '' || ${2:-} == --enable ) || $action == uninstall && $# -le 1 ) ]] || fail "$usage"
[[ $EUID != 0 && -z ${SUDO_USER:-} ]] || fail 'Run as your desktop user, without sudo.'
: "${HOME:?}" "${XDG_RUNTIME_DIR:?Run in your desktop session}"
config=${XDG_CONFIG_HOME:-$HOME/.config}
data=${XDG_DATA_HOME:-$HOME/.local/share}
for directory in "$HOME" "$config" "$data" "$XDG_RUNTIME_DIR"; do
    [[ $directory == /* && $directory != / && $directory != *[[:cntrl:]]* ]] || fail "Invalid user directory: $directory"
done
[[ -d $XDG_RUNTIME_DIR && -O $XDG_RUNTIME_DIR ]] || fail 'Runtime directory must belong to you.'
app=cosmos-virtual-mic
id=io.github.petterssonjonas.CosmosVirtualMic
unit=$app.service
bin=$HOME/.local/bin
paths=("$bin/$app" "$bin/cosmos-virtual-mic-engine" "$config/systemd/user/$unit"
       "$data/applications/$id.desktop" "$data/icons/hicolor/scalable/apps/$id.svg" "$data/$app/install.sh")
sources=("$app" cosmos-virtual-mic-engine unit desktop "data/$id.svg" install.sh)
ctl() { timeout 25 systemctl --user --no-pager --no-ask-password "$@"; }
exec 9>"$XDG_RUNTIME_DIR/$app-install.lock"
flock -n 9 || fail 'Another installer is running.'
ctl show-environment >/dev/null
fragment=$(ctl show "$unit" --property=FragmentPath --value)
[[ -z $fragment || $fragment -ef ${paths[2]} ]] || fail "A different installation owns the service: $fragment"
state=$(ctl is-active "$unit") || { [[ $state == inactive || $state == failed || $state == unknown || $state == activating || $state == deactivating ]] || fail "Cannot read service state: ${state:-unknown}"; }
mkdir -p -- "$HOME/.local"
stage=$(mktemp -d "$HOME/.local/.cosmos-install.XXXXXX")
temporary=
cleanup() {
    local result=$?
    [[ -z ${temporary:-} ]] || rm -f -- "$temporary"
    if (( result )) && [[ -d $stage/previous ]]; then
        printf 'Installation stopped. Previous files are retained in %s/previous. Fix the error and rerun this installer.\n' "$stage" >&2
    else
        rm -rf -- "$stage"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Escape the two file formats, not shell commands. Never source bundle metadata.
quote_unit() {
    local value=${1//\\/\\\\}
    value=${value//\"/\\\"}; value=${value//%/%%}
    printf '"%s"' "$value"
}
quote_desktop() {
    local value=${1//\\/\\\\\\\\}
    value=${value//\"/\\\\\"}; value=${value//\`/\\\\\`}
    value=${value//\$/\\\\\$}; value=${value//%/%%}
    printf '"%s"' "$value"
}
if [[ $action == install ]]; then
    bundle=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    cp -- "$bundle/SHA256SUMS" "$stage/SHA256SUMS"
    while read -r hash name; do
        [[ $hash =~ ^[[:xdigit:]]{64}$ && $name =~ ^[A-Za-z0-9_./-]+$ && $name != /* && $name != *..* && $name != SHA256SUMS ]] || fail 'Invalid checksum manifest.'
        install -D -m 644 -- "$bundle/$name" "$stage/$name"
    done <"$stage/SHA256SUMS"
    (cd -- "$stage" && sha256sum --check --strict SHA256SUMS)
    grep -qx "ARCH=$(uname -m)" "$stage/data/compatibility.conf" || fail 'Download the bundle for this machine architecture.'
    for executable in "$app" cosmos-virtual-mic-engine; do
        chmod 755 "$stage/$executable"
        timeout 10 "$stage/$executable" --help >/dev/null || fail "Cannot run $executable; check its architecture and runtime libraries."
    done
    text=$(<"$stage/data/$unit")
    value=":/usr/bin/env $(quote_unit "--chdir=$bin") ./cosmos-virtual-mic-engine run"
    text=${text//@EXEC_START@/"$value"}
    value="$(quote_unit "XDG_CONFIG_HOME=$config") $(quote_unit "XDG_DATA_HOME=$data")"
    printf '%s\n' "${text//@ENVIRONMENT@/"$value"}" >"$stage/unit"
    text=$(<"$stage/data/$id.desktop")
    value="/usr/bin/env $(quote_desktop "--chdir=$bin") $(quote_desktop "XDG_CONFIG_HOME=$config") $(quote_desktop "XDG_DATA_HOME=$data") ./$app"
    text=${text//@EXEC@/"$value"}
    printf '%s\n' "${text//@ICON@/"$id"}" >"$stage/desktop"
    for source in "${sources[@]}"; do
        [[ -f $stage/$source ]] || fail "Bundle is missing $source."
    done
fi

# Save through the installed client before replacing the engine.
old=$bin/cosmos-virtual-mic-engine
socket=$XDG_RUNTIME_DIR/$app/control.sock
if [[ -e $socket || -S $socket ]]; then
    [[ -x $old ]] || fail 'No installed engine client can save and stop the running engine.'
    timeout 20 "$old" stop
    for ((i=0; i<100; i++)); do
        [[ -e $socket || -S $socket ]] || break
        sleep .1
    done
    [[ ! -e $socket && ! -S $socket ]] || fail 'Engine did not release its socket; no files changed.'
elif [[ -n $fragment ]]; then
    [[ $(ctl show "$unit" --property=MainPID --value) == 0 ]] || fail 'Engine has no control socket; refusing to stop it without saving.'
fi
[[ -z $fragment ]] || ctl stop "$unit"
mkdir -- "$stage/previous"
for path in "${paths[@]}"; do
    [[ ! -e $path && ! -L $path ]] || cp -a -- "$path" "$stage/previous/"
done
if [[ $action == uninstall ]]; then
    [[ -z $fragment ]] || { ctl disable "$unit"; ctl disable --runtime "$unit"; }
    rm -f -- "${paths[@]}"
else
    for i in "${!paths[@]}"; do
        path=${paths[i]}
        mkdir -p -- "$(dirname -- "$path")"
        temporary=$(mktemp -- "$path.XXXXXX")
        mode=644; [[ $i != 0 && $i != 1 && $i != 5 ]] || mode=755
        install -m "$mode" -- "$stage/${sources[i]}" "$temporary"
        mv -fT -- "$temporary" "$path"
    done
fi
ctl daemon-reload
if [[ $action == install ]]; then
    [[ $(ctl show "$unit" --property=FragmentPath --value) -ef ${paths[2]} ]] || fail 'The user manager did not load the installed unit; check its XDG configuration path.'
    [[ ${2:-} != --enable ]] || ctl enable "$unit"
    if [[ ${2:-} == --enable || $state == active || $state == activating || $state == reloading ]]; then
        ctl start "$unit"
        ctl is-active --quiet "$unit"
    fi
fi
printf '%s complete. Microphone settings were not changed.\n' "$action"
[[ $action != install ]] || printf 'Launch: %q\nRemove: bash %q uninstall\n' "$bin/$app" "$data/$app/install.sh"
