#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
source ./dev-env.sh
# Distribution archives are created only by an explicitly published Release.
[[ ${GITHUB_EVENT_NAME:-} == release && -n ${GITHUB_EVENT_PATH:-} ]] || {
    printf '%s\n' 'Packaging requires the GitHub release.published event.' >&2
    exit 1
}
source /etc/os-release
[[ $ID == fedora && $VERSION_ID == 45 && $(uname -m) == x86_64 ]] || {
    printf '%s\n' 'Packaging requires the pinned Fedora 45 x86_64 cosmos toolbox.' >&2
    exit 1
}
exec python3 packaging/release.py build
