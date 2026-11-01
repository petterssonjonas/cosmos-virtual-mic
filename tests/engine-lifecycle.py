#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise the real engine against a private PipeWire server, never host audio."""
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/cosmos-engine"

DAEMON = """
context.properties = { core.daemon = true core.name = cosmos-test }
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-access args = { access.force = unrestricted } }
]
context.objects = [
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = cosmos-test-mono node.description = "Same description"
        media.class = Audio/Sink audio.channels = 1 audio.position = [ MONO ]
    } }
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = cosmos-test-stereo node.description = "Same description"
        media.class = Audio/Sink audio.channels = 2 audio.position = [ FL FR ]
    } }
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = cosmos-test-surround node.description = "Unsupported layout"
        media.class = Audio/Sink audio.channels = 6
        audio.position = [ FL FR FC LFE RL RR ]
    } }
]
"""


def eventually(check, timeout=12):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, subprocess.SubprocessError, ValueError) as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f"Condition not met in {timeout}s: {last}")


def terminate(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=4)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=4)


def environment(directory):
    env = {"PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"}
    home = directory / "home"
    home.mkdir(mode=0o700)
    env["HOME"] = str(home)
    for key, name in (("XDG_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
                      ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")):
        path = directory / name
        path.mkdir(mode=0o700)
        env[key] = str(path)
    config = directory / "config"
    env["XDG_CONFIG_DIRS"] = str(config)
    env.update(PIPEWIRE_RUNTIME_DIR=env["XDG_RUNTIME_DIR"],
               PIPEWIRE_REMOTE="cosmos-test", PIPEWIRE_CONFIG_DIR=str(config),
               PIPEWIRE_CONFIG_PREFIX="")
    # Native clients still need the distribution client modules, without user drop-ins.
    (config / "client.conf").write_text(
        "context.modules = [ { name = libpipewire-module-protocol-native } ]\n")
    (config / "daemon.conf").write_text(DAEMON)
    return env


def main():
    with tempfile.TemporaryDirectory(prefix="cosmos-lifecycle-", dir="/var/tmp") as tmp:
        directory = Path(tmp)
        env = environment(directory)
        ipc = directory / "run/control.sock"
        processes = []
        with (directory / "process.log").open("w+") as log:
            def launch(*args):
                process = subprocess.Popen(args, env=env, stdout=log, stderr=log)
                processes.append(process)
                return process

            def command(action):
                result = subprocess.run(
                    [str(BINARY), action, "--socket", str(ipc)], env=env,
                    capture_output=True, text=True, timeout=3, check=True)
                return json.loads(result.stdout)

            def phase(value):
                result = command("list")
                if result["status"]["phase"] != value:
                    raise ValueError(result)
                return result

            try:
                # An existing user file is not a stale socket and must survive.
                ipc.write_text("preserve user data")
                refused = subprocess.run(
                    [str(BINARY), "run", "--socket", str(ipc)], env=env,
                    capture_output=True, timeout=3)
                assert refused.returncode != 0
                assert ipc.read_text() == "preserve user data"
                ipc.unlink()
                unsafe = directory / "unsafe"
                unsafe.mkdir(mode=0o755)
                unsafe.chmod(0o755)
                refused = subprocess.run(
                    [str(BINARY), "run", "--socket", str(unsafe / "control.sock")],
                    env=env, capture_output=True, timeout=3)
                assert refused.returncode != 0, "Non-private IPC parent accepted"
                engine = launch(str(BINARY), "run", "--socket", str(ipc))
                absent = eventually(lambda: phase("disconnected"))
                assert absent["nodes"] == []
                assert absent["status"]["pid"] == engine.pid
                duplicate = subprocess.run(
                    [str(BINARY), "run", "--socket", str(ipc)], env=env,
                    capture_output=True, timeout=3)
                assert duplicate.returncode != 0, "Duplicate engine accepted"
                previous_generation = 0
                for cycle in range(3):
                    daemon = launch("pipewire", "-c", "daemon.conf")
                    connected = eventually(lambda: phase("connected"))
                    generation = connected["status"]["generation"]
                    assert generation > previous_generation
                    previous_generation = generation

                    def discovered():
                        result = command("list")
                        nodes = {node["name"]: node for node in result["nodes"]}
                        if len(nodes) != 3:
                            return None
                        for name, kind in (("mono", "mono"), ("stereo", "stereo"),
                                           ("surround", "unsupported")):
                            node = nodes[f"cosmos-test-{name}"]
                            if node["layout"]["kind"] == "unknown":
                                return None
                            assert node["layout"]["kind"] == kind, node
                            assert node["generation"] == generation, node
                        return result

                    eventually(discovered)
                    assert command("status")["status"]["pid"] == engine.pid
                    terminate(daemon)
                    disconnected = eventually(lambda: phase("disconnected"))
                    assert disconnected["nodes"] == [], "Stale nodes survived disconnect"
                    assert disconnected["status"]["generation"] >= generation
                    print(f"Server cycle {cycle + 1}: discovered layouts; stale nodes cleared")

                # One idle client must not block a separate control request.
                with socket.socket(socket.AF_UNIX) as slow:
                    slow.connect(str(ipc))
                    slow.sendall(b'{')
                    assert command("status")["status"]["pid"] == engine.pid
                with socket.socket(socket.AF_UNIX) as oversized:
                    oversized.settimeout(3)
                    oversized.connect(str(ipc))
                    try:
                        oversized.sendall(b"x" * (128 * 1024) + b"\n")
                        oversized.recv(4096)
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                assert command("status")["status"]["pid"] == engine.pid
                engine.kill()
                engine.wait(timeout=4)
                engine = launch(str(BINARY), "run", "--socket", str(ipc))
                eventually(lambda: phase("disconnected"))
                assert command("status")["status"]["pid"] == engine.pid
                assert command("stop")["stopping"]
                assert engine.wait(timeout=4) == 0
                assert not ipc.exists(), "Control socket survived clean stop"
                print("PASS: absent server, duplicate owner, client isolation, oversized input, "
                      "crash recovery, explicit stop")
            except BaseException:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                for process in reversed(processes):
                    terminate(process)


if __name__ == "__main__":
    main()
