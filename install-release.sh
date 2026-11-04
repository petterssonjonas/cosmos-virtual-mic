#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Keep all work inside the function: an incomplete curl | bash must not install.
cosmos_install_release() (
    set -euo pipefail
    umask 077
    fail() { printf 'Cosmos release installer: %s\n' "$*" >&2; exit 1; }
    usage='Usage: bash install-release.sh [--version vX.Y[.Z]] [--enable]'
    tag=
    enable=()
    while (( $# )); do
        case $1 in
            --version)
                [[ $# -ge 2 && -z $tag ]] || fail "$usage"
                tag=$2
                [[ -n $tag ]] || fail "$usage"
                shift 2
                ;;
            --enable)
                [[ ${#enable[@]} == 0 ]] || fail "$usage"
                enable=(--enable)
                shift
                ;;
            --help|-h) printf '%s\n' "$usage"; exit 0 ;;
            *) fail "$usage" ;;
        esac
    done
    [[ $EUID != 0 && -z ${SUDO_USER:-} ]] || fail 'Run as your desktop user, without sudo.'
    [[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || fail 'This release requires Linux x86_64.'
    for command in curl tar sha256sum mktemp; do
        command -v "$command" >/dev/null || fail "Required command is missing: $command"
    done
    base=https://github.com/petterssonjonas/cosmos-virtual-mic/releases
    curl_options=(--proto '=https' --proto-redir '=https' --fail --silent --show-error
                  --location --connect-timeout 15 --max-time 600)
    if [[ -z $tag ]]; then
        resolved=$(curl "${curl_options[@]}" --head --output /dev/null --write-out '%{url_effective}' "$base/latest") || fail 'Cannot resolve the latest published release.'
        [[ $resolved == "$base/tag/"* ]] || fail 'GitHub returned an unexpected release URL.'
        tag=${resolved#"$base/tag/"}
    fi
    [[ $tag =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?$ ]] || fail 'Use a release tag such as v0.2 or v0.2.0.'
    version=${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[4]:-0}
    asset=cosmos-virtual-mic-$version-linux-x86_64.tar.gz
    stage=$(mktemp -d "${TMPDIR:-/tmp}/cosmos-release.XXXXXXXXXX")
    trap 'rm -rf -- "$stage"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    download=$base/download/$tag
    printf 'Downloading Cosmos Virtual Mic %s (%s).\n' "$version" "$tag"
    curl "${curl_options[@]}" --output "$stage/SHA256SUMS" "$download/SHA256SUMS" || fail 'Cannot download release checksums.'
    hash=
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line =~ ^([[:xdigit:]]{64})\ \ ([A-Za-z0-9._-]+)$ ]] || fail 'Invalid release checksum manifest.'
        if [[ ${BASH_REMATCH[2]} == "$asset" ]]; then
            [[ -z $hash ]] || fail 'Duplicate archive checksum.'
            hash=${BASH_REMATCH[1]}
        fi
    done <"$stage/SHA256SUMS"
    [[ -n $hash ]] || fail 'The release has no checksum for this archive.'
    curl "${curl_options[@]}" --output "$stage/$asset" "$download/$asset" || fail 'Cannot download the release archive.'
    (cd -- "$stage" && printf '%s  %s\n' "$hash" "$asset" | sha256sum --check --strict --status) || fail 'Release archive checksum verification failed.'

    # GNU tar escapes control characters in names. Check every name and type before
    # extracting anything; even correctly checksummed archives cannot add links,
    # devices, duplicate files, traversal paths, or unexpected installer content.
    names=$(tar --list --gzip --file "$stage/$asset" --quoting-style=escape) || fail 'Cannot read the release archive.'
    details=$(tar --list --verbose --numeric-owner --gzip --file "$stage/$asset" --quoting-style=escape) || fail 'Cannot inspect the release archive.'
    mapfile -t members <<<"$names"
    mapfile -t entries <<<"$details"
    (( ${#members[@]} == ${#entries[@]} )) || fail 'Invalid archive listing.'
    required=(cosmos-virtual-mic cosmos-virtual-mic-engine install.sh LICENSE SHA256SUMS
              data/compatibility.conf data/cosmos-virtual-mic.service
              data/io.github.petterssonjonas.CosmosVirtualMic.desktop
              data/io.github.petterssonjonas.CosmosVirtualMic.svg
              THIRD-PARTY-NOTICES.txt dependency-inventory.json BUILD-PROVENANCE.json
              RUNTIME-REQUIREMENTS.txt)
    declare -A seen=()
    licenses=0
    for i in "${!members[@]}"; do
        member=${members[i]}
        if [[ $member == data/ || $member == LICENSES/ ]]; then
            [[ ${entries[i]:0:1} == d ]] || fail 'Invalid archive directory.'
        else
            [[ ${entries[i]:0:1} == - ]] || fail 'Archive contains a link or special file.'
            if [[ $member =~ ^LICENSES/[0-9a-f]{64}\.txt$ ]]; then
                licenses=$((licenses + 1))
            else
                [[ $member =~ ^[A-Za-z0-9_./-]+$ && " ${required[*]} " == *" $member "* ]] || fail "Unexpected archive member: $member"
            fi
        fi
        # Only allowlisted names can reach an associative-array subscript.
        [[ ! -v seen[$member] ]] || fail 'Duplicate archive member.'
        seen[$member]=1
    done
    for member in "${required[@]}"; do
        [[ -v seen[$member] ]] || fail "Archive is missing $member."
    done
    (( licenses > 0 )) || fail 'Archive is missing dependency licenses.'
    mkdir -- "$stage/bundle"
    tar --extract --gzip --file "$stage/$asset" --directory "$stage/bundle" \
        --no-same-owner --no-same-permissions --keep-old-files || fail 'Cannot extract the verified release archive.'
    bash "$stage/bundle/install.sh" install "${enable[@]}"
)
# The final compound command must also be complete before a pipe can invoke it.
{ cosmos_install_release "$@"; }
