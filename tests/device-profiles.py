#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Verify OS device-profile mutations are rejected on a synthetic-only server.

Run after building cosmos-virtual-mic-engine: python3 tests/device-profiles.py
The native fixture remains isolated from host devices and session policy.
"""
import importlib.util
import json
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("lifecycle", ROOT / "tests/engine-lifecycle.py")
LIFECYCLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFECYCLE)
VERSION = 4


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
                             and device["readable"]), None)

            def selection(index, **overrides):
                device = LIFECYCLE.eventually(ready_device)
                result = dict(engine_pid=request("status")["status"]["pid"],
                              generation=device["generation"], id=device["id"],
                              serial=device["serial"], profile_index=index)
                result.update(overrides)
                return result


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

                # Even a fresh advertised profile selection must not mutate OS hardware.
                for selected in (selection(1), selection(0), selection(999), selection(-1),
                                 selection(1, generation=initial["generation"] - 1),
                                 selection(1, serial=initial["serial"] + 1),
                                 selection(1, engine_pid=engine.pid + 1)):
                    response = request("set_device_profile", device_profile=selected)
                    assert "error" in response, response
                    assert ready_device()["current_profile"] == 0
                    assert endpoints_absent()
                    assert request("get_profile")["profile"] == saved
                request("stop")
                assert engine.wait(timeout=4) == 0
                print("PASS: native Device discovery is read-only; OS profile mutations rejected; "
                      "saved microphone profile and native off state unchanged")
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
