#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Native SPA Device off/on and rejection regression; never connects to host audio.

Run after building cosmos-engine: python3 tests/device-profiles.py
The tiny C fixture is compiled into the test's temporary directory using pkg-config
libpipewire-0.3. No fixture binaries or OS device choices are saved in the repo.
"""
import importlib.util
import json
from pathlib import Path
import os
import signal
import shlex
import socket
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("lifecycle", ROOT / "tests/engine-lifecycle.py")
LIFECYCLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFECYCLE)
VERSION = 3


def main():
    with tempfile.TemporaryDirectory(prefix="cosmos-device-profiles-", dir="/var/tmp") as tmp:
        directory = Path(tmp)
        env = LIFECYCLE.environment(directory)
        fixture = directory / "profile-device"
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--cflags", "--libs", "libpipewire-0.3"], text=True))
        # Distribution headers are external; keep -Werror for our fixture source.
        flags = ["-isystem" + flag[2:] if flag.startswith("-I") else flag for flag in flags]
        subprocess.run(["cc", "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        str(ROOT / "tests/private-pipewire/profile-device.c"),
                        "-o", str(fixture), *flags], check=True)
        ipc = directory / "run/control.sock"
        processes = []
        with (directory / "process.log").open("w+") as log:
            def launch(*args):
                process = subprocess.Popen(args, env=env, stdout=log, stderr=log)
                processes.append(process)
                return process

            def request(command, **fields):
                payload = dict(version=VERSION, command=command, **fields)
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(3)
                    client.connect(str(ipc))
                    client.sendall(json.dumps(payload).encode() + b"\n")
                    response = bytearray()
                    while not response.endswith(b"\n"):
                        data = client.recv(65536)
                        assert data, "engine closed before response"
                        response.extend(data)
                        assert len(response) <= 2 * 1024 * 1024
                    return json.loads(response)

            def devices():
                return request("list")["devices"]

            def ready_device():
                return next((device for device in devices()
                             if device["name"] == "cosmos.profile.fixture"
                             and len(device["profiles"]) == 5
                             and device["readable"] and device["writable"]), None)

            def selection(index, **overrides):
                device = LIFECYCLE.eventually(ready_device)
                result = dict(engine_pid=request("status")["status"]["pid"],
                              generation=device["generation"], id=device["id"],
                              serial=device["serial"], profile_index=index)
                result.update(overrides)
                return result

            def begin(index):
                selected = selection(index)
                started = time.monotonic()
                response = request("set_device_profile", device_profile=selected)
                assert time.monotonic() - started < 1, "native change blocked IPC"
                assert response["change"]["state"] == "pending", response
                assert response["change"]["request"] == selected
                return response["change"]

            def finished(change, expected):
                def done():
                    state = request("status")["status"]["device_profile_change"]
                    if state and state["sequence"] == change["sequence"] and state["request"] == change["request"]:
                        return state if state["state"] != "pending" else None
                    return None
                state = LIFECYCLE.eventually(done, timeout=8)
                assert state["state"] == expected, state
                assert bool(state["error"]) == (expected == "failed"), state
                return state

            def endpoints_present():
                names = {node["name"] for node in request("list")["nodes"]}
                return {"cosmos.profile.input", "cosmos.profile.output"} <= names

            def endpoints_absent():
                names = {node["name"] for node in request("list")["nodes"]}
                return not ({"cosmos.profile.input", "cosmos.profile.output"} & names)

            try:
                daemon = launch(str(fixture), "cosmos-test")
                LIFECYCLE.eventually(lambda: (directory / "run/cosmos-test").exists())
                engine = launch(str(LIFECYCLE.BINARY), "run", "--socket", str(ipc),
                                "--config", str(directory / "config/mixer.json"))
                LIFECYCLE.eventually(lambda: request("status")["status"]["phase"] == "connected")
                initial = LIFECYCLE.eventually(ready_device)
                assert initial["disabled"] and initial["current_profile"] == 0, initial
                assert endpoints_absent()
                saved = request("get_profile")["profile"]

                on = begin(1)
                finished(on, "confirmed")
                LIFECYCLE.eventually(endpoints_present)
                assert not ready_device()["disabled"]
                off = begin(0)
                finished(off, "confirmed")
                LIFECYCLE.eventually(endpoints_absent)
                disabled = LIFECYCLE.eventually(ready_device)
                assert disabled["disabled"] and disabled["profiles"] == initial["profiles"], disabled
                # Restore is an explicitly selected advertised profile, never a guessed default.
                restored = begin(1)
                finished(restored, "confirmed")
                assert restored["sequence"] > off["sequence"] > on["sequence"]
                LIFECYCLE.eventually(endpoints_present)
                assert request("get_profile")["profile"] == saved

                for selected in (selection(1, generation=initial["generation"] - 1),
                                 selection(1, serial=initial["serial"] + 1),
                                 selection(1, engine_pid=engine.pid + 1),
                                 selection(999), selection(2), selection(-1)):
                    response = request("set_device_profile", device_profile=selected)
                    assert "error" in response, response
                assert ready_device()["current_profile"] == 1

                rejected = begin(3)
                failure = finished(rejected, "failed")
                assert "-5" in failure["error"] or "Input/output" in failure["error"], failure
                assert request("status")["status"]["last_control_error"]
                assert ready_device()["current_profile"] == 1
                stalled = begin(4)
                # A second action cannot replace a pending native target.
                assert "error" in request("set_device_profile", device_profile=selection(0))
                assert request("list")["status"]["pid"] == engine.pid
                failure = finished(stalled, "failed")
                assert "timed out" in failure["error"], failure

                removed = begin(4)
                os.kill(daemon.pid, signal.SIGUSR1)
                failure = finished(removed, "failed")
                assert "removed" in failure["error"], failure
                LIFECYCLE.eventually(lambda: not devices())
                os.kill(daemon.pid, signal.SIGUSR2)
                replaced = LIFECYCLE.eventually(ready_device)
                assert replaced["serial"] != initial["serial"]
                assert "error" in request("set_device_profile", device_profile=dict(
                    engine_pid=engine.pid, generation=replaced["generation"], id=replaced["id"],
                    serial=initial["serial"], profile_index=1))
                disconnected = begin(4)
                LIFECYCLE.terminate(daemon)
                failure = finished(disconnected, "failed")
                assert "disconnected" in failure["error"], failure
                assert devices() == []
                request("stop")
                assert engine.wait(timeout=4) == 0
                print("PASS: native Device discovery while off; explicit input/output off/on; "
                      "serial/generation/availability validation; saved-profile isolation; "
                      "native rejection, timeout, removal, disconnect; nonblocking IPC")
            except BaseException:
                print(f"Native fixture exit status: {daemon.poll() if 'daemon' in locals() else 'not launched'}")
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                for process in reversed(processes):
                    LIFECYCLE.terminate(process)


if __name__ == "__main__":
    main()
