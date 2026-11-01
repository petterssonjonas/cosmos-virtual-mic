#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Run real LSP DSP with synthetic samples on a disposable PipeWire server.

Run ./tests/private-pipewire/verify.py [--keep]. No WirePlumber is started.
The application does not use these CLI tools; they are test drivers only.
"""

import argparse
import array
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time


HERE = Path(__file__).resolve().parent
RATE = 48000
REMOTE = "cosmos-private"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Harness:
    def __init__(self, root):
        self.root = root
        self.children = []
        self.handles = []
        self.sequence = 0
        self.server = None
        for name in ("runtime", "config", "state", "cache", "data", "home"):
            (root / name).mkdir(mode=0o700)
        self.runtime = root / "runtime"
        self.config = root / "config"
        self.socket = self.runtime / REMOTE
        # An allowlist, not a modified desktop environment. There is no host
        # runtime, HOME config, inherited remote, D-Bus, Pulse, or JACK address.
        self.env = {
            "PATH": "/usr/bin:/bin",
            "LANG": "C",
            "LC_ALL": "C",
            "HOME": str(root / "home"),
            "XDG_RUNTIME_DIR": str(self.runtime),
            "XDG_CONFIG_HOME": str(self.config),
            "XDG_CONFIG_DIRS": str(self.config),
            "XDG_STATE_HOME": str(root / "state"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "XDG_DATA_HOME": str(root / "data"),
            "PIPEWIRE_RUNTIME_DIR": str(self.runtime),
            "PIPEWIRE_REMOTE": str(self.socket),
            "PIPEWIRE_CONFIG_DIR": str(self.config),
            "PIPEWIRE_CONFIG_PREFIX": "",
            "PIPEWIRE_DEBUG": "2",
        }
        self.tools = {}
        for name in ("pipewire", "pw-cat", "pw-link", "pw-dump", "lv2info"):
            executable = shutil.which(name, path=self.env["PATH"])
            if executable is None:
                raise RuntimeError(f"Required native tool is not installed: {name}")
            self.tools[name] = executable
        for name in ("server.conf", "client.conf"):
            shutil.copyfile(HERE / name, self.config / name)
        self.effects = json.loads((HERE / "effects.json").read_text())

    def command(self, tool, *args, check=True):
        result = subprocess.run(
            [self.tools[tool], *map(str, args)], env=self.env,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, timeout=10,
        )
        if check and result.returncode:
            raise RuntimeError(
                f"{tool} failed ({result.returncode}): "
                + result.stderr.decode(errors="replace")
            )
        return result

    def spawn(self, label, tool, *args, output=None, diagnostics=None):
        self.sequence += 1
        log = (diagnostics or self.root / f"{self.sequence:02}-{label}.log").open("wb")
        self.handles.append(log)
        if output is not None:
            stdout = output.open("wb")
            self.handles.append(stdout)
        else:
            stdout = log
        process = subprocess.Popen(
            [self.tools[tool], *map(str, args)], env=self.env,
            stdin=subprocess.DEVNULL, stdout=stdout, stderr=log,
        )
        self.children.append(process)
        return process

    @staticmethod
    def stop(process, crash=False):
        # Only a Popen child is signalled. No service manager, process search,
        # process group, or inherited PID is used.
        if process.poll() is None:
            if crash:
                process.kill()
            else:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)

    def close(self):
        for process in reversed(self.children):
            self.stop(process)
        for handle in self.handles:
            handle.close()

    def wait_for(self, predicate, description, processes=(), timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for process in processes:
                require(process.poll() is None,
                        f"Child {process.pid} exited while waiting for {description}; see logs")
            value = predicate()
            if value:
                return value
            time.sleep(0.025)
        raise TimeoutError(f"Timed out waiting for {description}; see {self.root}")

    def snapshot(self, connecting=False):
        result = self.command("pw-dump", "-r", self.socket, check=not connecting)
        if result.returncode:
            return []
        objects = json.loads(result.stdout)
        require(not any(item["type"] == "PipeWire:Interface:Device" for item in objects),
                "The private graph must not contain a hardware device")
        return objects

    def nodes(self, connecting=False):
        return {
            item.get("info", {}).get("props", {}).get("node.name"): item
            for item in self.snapshot(connecting=connecting)
            if item["type"] == "PipeWire:Interface:Node"
        }

    def start_server(self):
        require(self.socket.parent == self.runtime, "Socket must remain private")
        self.server = self.spawn("server", "pipewire", "-c", "server.conf")
        self.wait_for(self.socket.exists, "private server socket", (self.server,))
        self.wait_for(lambda: "cosmos-test-driver" in self.nodes(connecting=True),
                      "private driver", (self.server,))
        require(set(self.nodes()) == {"cosmos-test-driver"},
                "Unexpected node on the empty private server")

    def inspect_plugins(self):
        # lv2info reads metadata; it does not run an audio path. Check the exact
        # installed ABI symbols and ranges before asking the native host to load.
        for name, effect in self.effects.items():
            info = self.command("lv2info", effect["plugin"]).stdout.decode()
            (self.root / f"lv2info-{name}.txt").write_text(info)
            ports = {}
            for block in re.split(r"\n\s*Port \d+:\n", info)[1:]:
                symbol = re.search(r"Symbol:\s+(\S+)", block)
                if symbol:
                    ports[symbol[1]] = block
            require("in" in ports and "out" in ports,
                    f"{name}: expected mono audio ports are missing")
            for symbol, value in effect["control"].items():
                require(symbol in ports, f"{name}: control {symbol} is missing")
                block = ports[symbol]
                require("#InputPort" in block and "#ControlPort" in block,
                        f"{name}:{symbol} is not an input control")
                lower = re.search(r"Minimum:\s+([-+\d.eE]+)", block)
                upper = re.search(r"Maximum:\s+([-+\d.eE]+)", block)
                require(lower is not None and upper is not None,
                        f"{name}:{symbol} lacks numeric limits")
                require(float(lower[1]) - 1e-6 <= value <= float(upper[1]) + 1e-6,
                        f"{name}:{symbol} value {value} is outside installed limits")

    def filter_config(self, names, missing=False):
        nodes = []
        for name in names:
            effect = self.effects[name]
            nodes.append({"type": "lv2", "name": name,
                          "plugin": effect["plugin"], "control": effect["control"]})
        if missing:
            nodes[0]["plugin"] = "urn:cosmos:test:plugin-that-does-not-exist"
        graph = {
            "nodes": nodes,
            "links": [{"output": f"{left}:out", "input": f"{right}:in"}
                      for left, right in zip(names, names[1:])],
            "inputs": [f"{names[0]}:in"], "outputs": [f"{names[-1]}:out"],
        }
        props = {
            "node.autoconnect": False, "node.group": "cosmos.private",
            "audio.channels": 1, "audio.position": ["MONO"],
            "adapter.auto-port-config": {"mode": "dsp"},
        }
        module = {
            "name": "libpipewire-module-filter-chain",
            # No nofail flag: a failed mandatory effect must not become bypass.
            "args": {
                "node.description": "Cosmos private DSP proof",
                "audio.rate": RATE, "audio.channels": 1, "audio.position": ["MONO"],
                "filter.graph": graph,
                "capture.props": dict(props, **{"node.name": "cosmos-dsp-input",
                                                "media.class": "Audio/Sink"}),
                "playback.props": dict(props, **{"node.name": "cosmos-dsp-output",
                                                 "media.class": "Audio/Source"}),
            },
        }
        template = (HERE / "client.conf").read_text()
        require(template.count("# FILTER_MODULE") == 1, "Invalid filter host template")
        config = self.config / "filter.conf"
        # Keep closing delimiters apart: PipeWire 1.6.11 parse_modules
        # terminates its mutable args buffer in place.
        config.write_text(template.replace("# FILTER_MODULE", json.dumps(module, indent=2)))
        return config.name

    def start_filter(self, names):
        process = self.spawn("filter", "pipewire", "-c", self.filter_config(names))
        self.wait_for(lambda: {"cosmos-dsp-input", "cosmos-dsp-output"} <= self.nodes().keys(),
                      "DSP endpoints", (self.server, process))
        return process

    def port(self, node_name, direction):
        objects = self.snapshot()
        nodes = [item for item in objects if item["type"] == "PipeWire:Interface:Node"
                 and item.get("info", {}).get("props", {}).get("node.name") == node_name]
        if not nodes:
            return None
        ports = [item for item in objects if item["type"] == "PipeWire:Interface:Port"
                 and str(item.get("info", {}).get("props", {}).get("node.id")) == str(nodes[0]["id"])
                 and item.get("info", {}).get("direction") == direction]
        require(len(ports) <= 1, f"Unexpected channel layout on {node_name}")
        return ports[0]["id"] if ports else None

    def measure(self, label, amplitude, frequency=1000):
        source = self.root / f"{label}-input.f32"
        capture = self.root / f"{label}-output.f32"
        diagnostics = self.root / f"{label}-record.log"
        player_diagnostics = self.root / f"{label}-play.log"
        samples = array.array("f", (
            amplitude * math.sin(2 * math.pi * frequency * frame / RATE)
            for frame in range(4 * RATE)
        ))
        source.write_bytes(samples.tobytes())
        common = ("--raw", "--format", "f32", "--rate", RATE,
                  "--channels", 1, "--channel-map", "MONO", "--latency", "256",
                  "--remote", self.socket, "--target", "0")
        recorder = self.spawn("record", "pw-cat", "--record", *common,
                              "--sample-count", 2 * RATE,
                              "--properties", "{ node.name = cosmos-test-record node.always-process = true }",
                              "-", output=capture, diagnostics=diagnostics)
        player = self.spawn("play", "pw-cat", "--playback", *common,
                            "--properties", "{ node.name = cosmos-test-play }", source,
                            diagnostics=player_diagnostics)
        try:
            ports = {}
            for name, direction in (("cosmos-test-record", "input"),
                                    ("cosmos-test-play", "output"),
                                    ("cosmos-dsp-input", "input"),
                                    ("cosmos-dsp-output", "output")):
                ports[name] = self.wait_for(lambda n=name, d=direction: self.port(n, d),
                                           f"{name} {direction} port", (self.server, recorder, player))
            self.command("pw-link", "-r", self.socket,
                         ports["cosmos-dsp-output"], ports["cosmos-test-record"])
            self.command("pw-link", "-r", self.socket,
                         ports["cosmos-test-play"], ports["cosmos-dsp-input"])
            graph = self.snapshot()
            links = [item for item in graph if item["type"] == "PipeWire:Interface:Link"]
            require(len(links) == 2, f"Expected only two explicit audio links, got {len(links)}")
            (self.root / f"{label}-graph.json").write_text(json.dumps(graph, indent=2))
            result = recorder.wait(timeout=8)
            # PipeWire 1.6.11 pw-cat.c: on_process quits on sample_limit
            # without setting drained; main returns success only if drained.
            # A finite recording therefore returns 1. Require that exact
            # completion status, no diagnostics, and the full sample count.
            # https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/1.6.11/src/tools/pw-cat.c
            require(result == 1, f"Unexpected finite recorder status ({result}); see logs")
            require(diagnostics.read_bytes() == b"",
                    f"Recorder reported diagnostics; see {diagnostics}")
            require(player.poll() is None, "Synthetic player exited before recording completed")
            require(player_diagnostics.read_bytes() == b"",
                    f"Synthetic player reported diagnostics; see {player_diagnostics}")
        finally:
            self.stop(recorder)
            self.stop(player)
        raw = capture.read_bytes()
        require(len(raw) == 2 * RATE * 4,
                f"{label}: expected {2 * RATE} float samples, got {len(raw)} bytes")
        recorded = array.array("f")
        recorded.frombytes(raw)
        require(all(math.isfinite(value) for value in recorded), f"{label}: non-finite output")
        # Exclude start/link scheduling and native detector/limiter settling.
        steady = recorded[RATE:]
        rms = math.sqrt(sum(value * value for value in steady) / len(steady))
        peak = max(map(abs, steady))
        ratio = rms / (amplitude / math.sqrt(2))
        metric = {"case": label, "frequency": frequency, "input_peak": amplitude,
                  "output_rms": rms, "output_peak": peak, "rms_ratio": ratio}
        print(json.dumps(metric), flush=True)
        return metric

    def remove_filter(self, process, crash=False):
        self.stop(process, crash=crash)
        self.wait_for(lambda: not ({"cosmos-dsp-input", "cosmos-dsp-output"} & self.nodes().keys()),
                      "DSP endpoint removal", (self.server,))
        require(not any(item["type"] == "PipeWire:Interface:Link" for item in self.snapshot()),
                "Audio links survived removal of their owning processes")

    def verify_dsp(self):
        gate = self.start_filter(["gate"])
        try:
            low = self.measure("gate-closed", 0.02)
            high = self.measure("gate-open", 0.4)
            require(2e-5 < low["rms_ratio"] < 0.002,
                    "Gate output is silent or did not suppress the below-threshold signal")
            require(0.9 < high["rms_ratio"] < 1.1, "Gate did not pass the above-threshold signal")
        finally:
            self.remove_filter(gate)

        compressor = self.start_filter(["compressor"])
        try:
            low = self.measure("compressor-below", 0.02)
            high = self.measure("compressor-above", 0.8)
            require(0.9 < low["rms_ratio"] < 1.1, "Compressor changed the below-threshold signal")
            require(0.08 < high["rms_ratio"] < 0.5, "Compressor did not reduce the loud signal")
        finally:
            self.remove_filter(compressor)

        equalizer = self.start_filter(["eq"])
        try:
            center = self.measure("eq-center", 0.2, 1000)
            outside = self.measure("eq-outside", 0.2, 8000)
            require(0.19 < center["rms_ratio"] < 0.32, "EQ did not make the configured -12 dB center cut")
            require(0.85 < outside["rms_ratio"] < 1.1, "EQ changed the far-outside tone too much")
        finally:
            self.remove_filter(equalizer)

        limiter = self.start_filter(["limiter"])
        try:
            limited = self.measure("limiter", 0.8)
            require(0.03 < limited["output_peak"] <= 0.115,
                    "Limiter did not enforce its 0.1 ceiling (with settling tolerance)")
        finally:
            self.remove_filter(limiter)

        # Real ordered gate -> compressor -> EQ -> limiter, not separate fake
        # observations. Recreate it after an abrupt native-host exit as well.
        for iteration in range(2):
            chain = self.start_filter(["gate", "compressor", "eq", "limiter"])
            try:
                low = self.measure(f"chain-{iteration}-closed", 0.02)
                high = self.measure(f"chain-{iteration}-open", 0.8)
                require(1e-5 < low["rms_ratio"] < 0.002,
                        "Full chain output is silent or leaked the closed-gate signal")
                require(0.005 < high["output_rms"] < 0.1 and high["output_peak"] <= 0.115,
                        "Full chain did not produce processed, limited audio")
            finally:
                self.remove_filter(chain, crash=iteration == 0)

    def verify_missing_plugin(self):
        failed = self.spawn("missing-plugin", "pipewire", "-c", self.filter_config(["gate"], missing=True))
        result = failed.wait(timeout=8)
        require(result != 0, "Missing mandatory plugin did not fail native host startup")
        nodes = self.nodes()
        require(set(nodes) == {"cosmos-test-driver"},
                "Failed graph published a raw bypass or partial DSP endpoint")
        require(not any(item["type"] == "PipeWire:Interface:Link" for item in self.snapshot()),
                "Failed graph retained an audio route")
        # The same private daemon must still accept a valid native DSP graph.
        recovered = self.start_filter(["gate"])
        try:
            signal = self.measure("after-missing-plugin", 0.4)
            require(0.9 < signal["rms_ratio"] < 1.1, "Valid graph did not recover after plugin failure")
            reduced = self.measure("after-missing-plugin-closed", 0.02)
            require(2e-5 < reduced["rms_ratio"] < 0.002,
                    "Recovered gate is silent or did not process the below-threshold signal")
        finally:
            self.remove_filter(recovered)

    def verify_server_restart(self):
        # PipeWire does not automatically resurrect client filter-chain hosts.
        # Prove explicit test orchestration restores DSP; do not claim this is
        # application-engine recovery or uninterrupted audio.
        for iteration in range(2):
            old = self.start_filter(["gate"])
            self.stop(self.server, crash=True)
            self.stop(old)
            self.start_server()
            restored = self.start_filter(["gate"])
            try:
                signal = self.measure(f"server-restart-{iteration}", 0.4)
                require(0.9 < signal["rms_ratio"] < 1.1,
                        "DSP samples were not restored after private server restart")
                reduced = self.measure(f"server-restart-{iteration}-closed", 0.02)
                require(2e-5 < reduced["rms_ratio"] < 0.002,
                        "Restarted gate is silent or did not process the below-threshold signal")
            finally:
                self.remove_filter(restored)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", action="store_true", help="Keep private logs, graphs, and float samples on success")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="cosmos-pw-", dir="/tmp"))
    harness = None
    passed = False
    try:
        harness = Harness(root)
        harness.inspect_plugins()
        harness.start_server()
        harness.verify_dsp()
        harness.verify_missing_plugin()
        harness.verify_server_restart()
        passed = True
        print("PASS: real private LSP samples, native host failure, and explicit server restart recovery")
    finally:
        if harness is not None:
            harness.close()
        if passed and not args.keep:
            shutil.rmtree(root)
        else:
            print(f"Private test artifacts: {root}", file=sys.stderr)


if __name__ == "__main__":
    main()
