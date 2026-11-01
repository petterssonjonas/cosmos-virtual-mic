#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise the Rust backend with synthetic audio on a private PipeWire server."""
import argparse
import array
import importlib.util
import json
import math
from pathlib import Path
import shutil
import copy
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("private_pipewire", ROOT / "tests/private-pipewire/verify.py")
PRIVATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRIVATE)
RATE = PRIVATE.RATE
require = PRIVATE.require


class BackendHarness(PRIVATE.Harness):
    def __init__(self, root):
        super().__init__(root)
        self.tools["engine"] = str(ROOT / "target/debug/cosmos-engine")
        self.tools["pw-cli"] = "/usr/bin/pw-cli"
        self.ipc = self.runtime / "control.sock"
        self.profile_path = self.config / "profile.json"
        self.engine = None

    def start_engine(self):
        self.engine = self.spawn("engine", "engine", "run", "--socket", self.ipc,
                                 "--config", self.profile_path)
        def ready():
            try:
                return self.status()["pid"] == self.engine.pid
            except (RuntimeError, OSError):
                return False
        self.wait_for(ready, "engine control owner", (self.engine,))
        return self.engine

    def client(self, action, *args, check=True):
        result = self.command("engine", action, *args, "--socket", self.ipc, check=check)
        return json.loads(result.stdout) if check else result

    def apply(self, profile, check=True):
        request = self.root / "requested-profile.json"
        request.write_text(json.dumps(profile))
        return self.client("apply", request, check=check)

    def audio(self, label, output_name, amplitude, channels=1, source_name="fixture-source",
              duplicate=False, direct_sink=None, source_rate=RATE):
        """Create an explicit synthetic source; the engine, not this test, routes it."""
        source = self.root / f"{label}-input.f32"
        capture = self.root / f"{label}-output.f32"
        samples = array.array("f", (
            amplitude * math.sin(2 * math.pi * (1000 + channel * 1000) * frame / source_rate)
            for frame in range(8 * source_rate) for channel in range(channels)
        ))
        source.write_bytes(samples.tobytes())
        common = ("--raw", "--format", "f32", "--channels", channels,
                  "--channel-map", "MONO" if channels == 1 else "FL,FR",
                  "--latency", "256", "--remote", self.socket, "--target", "0")
        recorder = self.spawn(label + "-record", "pw-cat", "--record", "--rate", RATE, *common,
                              "--sample-count", 2 * RATE,
                              "--properties", "{ node.name = fixture-record node.always-process = true }",
                              "-", output=capture)
        player = None
        second_player = None
        try:
            if source_name is not None:
                player = self.spawn(label + "-play", "pw-cat", "--playback", "--rate", source_rate, *common,
                                    "--properties", json.dumps({"node.name": source_name,
                                                               "node.description": "Synthetic microphone"}),
                                    source)
                if duplicate:
                    second_player = self.spawn(label + "-duplicate", "pw-cat", "--playback",
                                               "--rate", source_rate, *common,
                                               "--properties", json.dumps({"node.name": source_name}),
                                               source)
            self.wait_for(lambda: output_name in self.nodes(), "backend recording endpoint",
                          (self.server, self.engine, recorder))
            self.wait_for(lambda: "fixture-record" in self.nodes(), "recorder node",
                          (self.server, self.engine, recorder))
            self.link_nodes(output_name, "fixture-record", channels)
            if direct_sink is not None:
                self.link_nodes(source_name, direct_sink, channels)
            require(recorder.wait(timeout=8) == 1,
                    "Finite native recorder did not return its documented PipeWire 1.6.11 status")
            if player is not None:
                require(player.poll() is None, "Synthetic player exited during recording")
            graph = self.snapshot()
            bus_names = [item["info"]["props"]["node.name"] for item in graph
                         if item["type"] == "PipeWire:Interface:Node"
                         and item.get("info", {}).get("props", {}).get("node.name", "").startswith("cosmos.bus.")]
            require(len(bus_names) == len(set(bus_names)), "Duplicate engine-owned bus/meter nodes")
            (self.root / f"{label}-graph.json").write_text(json.dumps(graph, indent=2))
            self.last_status = self.status()
            (self.root / f"{label}-status.json").write_text(json.dumps(self.last_status, indent=2))
        finally:
            self.stop(recorder)
            if player is not None:
                self.stop(player)
            if second_player is not None:
                self.stop(second_player)
        for log in self.root.glob(f"*-{label}-*.log"):
            require(log.stat().st_size == 0, f"Native audio diagnostics: {log}")
        raw = capture.read_bytes()
        require(len(raw) == 2 * RATE * channels * 4, "Incorrect recorded sample count")
        values = array.array("f")
        values.frombytes(raw)
        require(all(math.isfinite(value) for value in values), "Non-finite backend output")
        metrics = []
        for channel in range(channels):
            steady = values[RATE * channels + channel::channels]
            rms = math.sqrt(sum(value * value for value in steady) / len(steady))
            metrics.append({"rms": rms, "peak": max(map(abs, steady)),
                            "ratio": rms / (amplitude / math.sqrt(2))})
        print(json.dumps({"case": label, "channels": metrics}), flush=True)
        return metrics

    def status(self):
        return self.client("status")["status"]

    def wait_bus(self, state=None, bus="voice"):
        def ready():
            item = next((item for item in self.status()["backend"]["buses"] if item["id"] == bus), None)
            if item is None:
                return None
            if state is not None:
                return item if item["state"] == state else None
            # A configured source does not exist until audio() starts its fixture.
            if item["state"] not in ("ready", "unavailable"):
                return None
            names = self.nodes()
            return item if {f"cosmos.bus.{bus}.input", f"cosmos.bus.{bus}.output"} <= names.keys() else None
        return self.wait_for(ready, f"bus {bus} state {state or 'endpoint available'}",
                             (self.engine,), timeout=10)

    def start_server(self):
        self.server = self.spawn("server", "pipewire", "-c", "server.conf")
        self.wait_for(self.socket.exists, "private server socket", (self.server,))
        self.wait_for(lambda: "cosmos-test-driver" in self.nodes(connecting=True),
                      "private driver", (self.server,))


    def link_nodes(self, output_name, input_name, channels):
        def ports_ready():
            objects = self.snapshot()
            names = {item["id"]: item.get("info", {}).get("props", {}).get("node.name")
                     for item in objects if item["type"] == "PipeWire:Interface:Node"}
            found = {}
            for node_name, direction in ((output_name, "output"), (input_name, "input")):
                ports = {}
                for item in objects:
                    if item["type"] != "PipeWire:Interface:Port":
                        continue
                    info = item.get("info", {})
                    props = info.get("props", {})
                    if names.get(int(props.get("node.id", -1))) == node_name and info.get("direction") == direction:
                        ports[props.get("audio.channel")] = item["id"]
                if len(ports) != channels:
                    return None
                found[node_name] = ports
            return found
        ports = self.wait_for(ports_ready, "matching recorder ports", (self.server, self.engine))
        for channel in (["MONO"] if channels == 1 else ["FL", "FR"]):
            self.command("pw-link", "-r", self.socket,
                         ports[output_name][channel], ports[input_name][channel])

    def source(self, label, name, frequency, amplitude, properties=None):
        path = self.root / f"{label}-source.f32"
        path.write_bytes(array.array("f", (
            amplitude * math.sin(2 * math.pi * frequency * frame / RATE)
            for frame in range(8 * RATE))).tobytes())
        props = {"node.name": name, **(properties or {})}
        return self.spawn(label, "pw-cat", "--playback", "--raw", "--format", "f32",
                          "--rate", RATE, "--channels", 1, "--channel-map", "MONO",
                          "--remote", self.socket, "--target", "0",
                          "--properties", json.dumps(props), path)


def profile(channels="mono"):
    return {
        "version": 1,
        "buses": [{"id": "voice", "name": "Voice", "channels": channels,
                   "enabled": True, "hidden": False, "muted": False,
                   "effects": [{"id": "gate", "enabled": True, "effect": {
                       "kind": "gate", "threshold_db": -20.0, "attack_ms": 5.0,
                       "release_ms": 20.0, "hold_ms": 0.0}}]}],
        "devices": [],
        "routes": [{"id": "microphone", "from": {
            "kind": "hardware", "identity": {"node.name": "fixture-source"}},
            "to": {"kind": "bus", "id": "voice"}, "enabled": True}],
    }


def verify_meter_continuity(h):
    desired = profile()
    desired["routes"].append({"id": "second", "from": {
        "kind": "hardware", "identity": {"node.name": "fixture-other"}},
        "to": {"kind": "bus", "id": "voice"}, "enabled": True})
    h.apply(desired)
    first = h.source("meter-first", "fixture-source", 1000, 0.2)
    second = h.source("meter-second", "fixture-other", 2000, 0.2)
    try:
        def levels():
            bus = next(item for item in h.status()["backend"]["buses"] if item["id"] == "voice")
            meter = bus["input_meter"]
            return meter if meter and meter["fresh"] and not meter["error"] else None
        h.wait_for(lambda: (meter := levels()) and meter["peak"][0] > 0.25,
                   "both meter feeds", (h.engine, first, second))
        h.stop(first)
        h.wait_for(lambda: (meter := levels()) and 0.15 < meter["peak"][0] < 0.25,
                   "remaining meter feed without failure backoff", (h.engine, second), timeout=1)
    finally:
        h.stop(first)
        h.stop(second)
    h.apply(profile())
    print("PASS: input meter remains live when one of two sources leaves", flush=True)


def verify_sibling_isolation(h):
    h.wait_for(lambda: "fixture-source" not in h.nodes(), "old synthetic source removal", (h.engine,))
    desired = profile()
    desired["buses"].append({"id": "music", "name": "Music", "channels": "mono",
                             "enabled": True, "hidden": False, "muted": False, "effects": []})
    desired["routes"].extend([
        {"id": "overlap", "from": {"kind": "hardware", "identity": {
            "node.name": "fixture-source", "device.api": "synthetic"}},
         "to": {"kind": "bus", "id": "voice"}, "enabled": True},
        {"id": "music", "from": {"kind": "hardware", "identity": {"node.name": "fixture-other"}},
         "to": {"kind": "bus", "id": "music"}, "enabled": True},
    ])
    h.apply(desired)
    source = h.source("overlap-source", "fixture-source", 1000, 0.4, {"device.api": "synthetic"})
    try:
        h.wait_bus(bus="music")
        assert_passes(h.audio("unrelated-bus-survives", "cosmos.bus.music.output", 0.4,
                             source_name="fixture-other"))
        routes = {route["id"]: route for route in h.last_status["backend"]["routes"]}
        require(routes["microphone"]["state"] == routes["overlap"]["state"] == "error",
                "Resolved duplicate routes were not isolated")
        require(routes["music"]["state"] == "ready", "Unrelated bus was disabled by duplicate routes")
    finally:
        h.stop(source)
    h.apply(profile())


def assert_passes(metrics):
    require(all(0.9 < item["ratio"] < 1.1 for item in metrics),
            f"Expected unchanged above-threshold signal: {metrics}")


def assert_silent(metrics):
    require(all(item["peak"] < 1e-8 for item in metrics),
            f"Unavailable/muted source leaked audio: {metrics}")


def assert_meter(h, amplitude, suppressed=False):
    bus = next(bus for bus in h.last_status["backend"]["buses"] if bus["id"] == "voice")
    input_meter, output_meter = bus["input_meter"], bus["output_meter"]
    require(input_meter is not None and output_meter is not None, "Missing actual bus meters")
    for meter in (input_meter, output_meter):
        require(meter["fresh"] and not meter["error"] and meter["frames"] > 0,
                f"Meter has no current valid sample window: {meter}")
    require(0.85 * amplitude < input_meter["peak"][0] < 1.15 * amplitude,
            f"Input meter does not reflect synthetic level: {input_meter}")
    if suppressed:
        require(0 < output_meter["peak"][0] < amplitude * 0.002, "Gate meter does not show reduction")
    else:
        require(0.85 * amplitude < output_meter["peak"][0] < 1.15 * amplitude,
                "Output meter does not reflect actual output")


def verify(h):
    output = "cosmos.bus.voice.output"
    h.start_engine()
    h.wait_for(lambda: h.status()["phase"] == "disconnected",
               "engine starts without server", (h.engine,))
    initial = h.client("profile")["profile"]
    require(initial["buses"] == [] and initial["routes"] == [], "Defaults create audio routes")
    large = copy.deepcopy(initial)
    large["devices"] = [
        {"identity": {"node.name": f"unused-{index}-" + "x" * 360}, "alias": "x" * 120,
         "hidden": False, "enabled": False, "muted": False} for index in range(100)]
    require(h.apply(large)["accepted"], "Large valid profile failed bounded IPC")
    require(h.client("profile")["profile"] == large, "Large profile did not round-trip")
    desired = profile()
    require(h.apply(desired)["accepted"], "Valid profile rejected while server absent")
    require(h.client("profile")["profile"] == desired, "Desired profile was not retained")
    h.start_server()
    h.wait_bus()
    healthy_generation = h.status()["generation"]
    closed = h.audio("gate-closed", output, 0.02)[0]
    require(2e-5 < closed["ratio"] < 0.002, "Backend gate did not process the quiet signal")
    assert_meter(h, 0.02, suppressed=True)
    assert_passes(h.audio("gate-open", output, 0.4))
    assert_meter(h, 0.4)
    assert_passes(h.audio("source-rate-44100", output, 0.4, source_rate=44100))
    verify_meter_continuity(h)
    verify_sibling_isolation(h)
    meter_name = "cosmos.bus.voice.meter-input"
    old_meter = h.nodes()[meter_name]
    old_serial = old_meter["info"]["props"]["object.serial"]
    h.command("pw-cli", "-r", h.socket, "destroy", old_meter["id"])
    h.wait_for(lambda: h.nodes().get(meter_name, {}).get("info", {}).get("props", {}).get("object.serial")
               not in (None, old_serial), "removed meter recovery", (h.engine,), timeout=12)
    assert_passes(h.audio("meter-recreated", output, 0.4))
    assert_meter(h, 0.4)
    h.wait_for(lambda: any(route["state"] == "unavailable" for route in h.status()["backend"]["routes"]),
               "source removal marks route unavailable", (h.engine,))
    assert_silent(h.audio("no-source-substitution", output, 0.4, source_name="different-source"))
    assert_passes(h.audio("selected-source-returned", output, 0.4))
    assert_silent(h.audio("ambiguous-source", output, 0.4, duplicate=True))
    require(any(route["state"] == "error" for route in h.last_status["backend"]["routes"]),
            "Ambiguous source was not reported")
    assert_passes(h.audio("ambiguity-cleared", output, 0.4))
    unsupported_file = h.root / "six-channel-input.f32"
    unsupported_file.write_bytes(array.array("f", [0.1] * (8 * RATE * 6)).tobytes())
    unsupported = h.spawn("six-channel-source", "pw-cat", "--playback", "--raw", "--format", "f32",
                          "--rate", RATE, "--channels", 6, "--channel-map", "FL,FR,FC,LFE,RL,RR",
                          "--remote", h.socket, "--target", "0",
                          "--properties", '{ node.name = fixture-source }', unsupported_file)
    try:
        h.wait_for(lambda: "fixture-source" in h.nodes(), "six-channel source", (unsupported,))
        assert_silent(h.audio("unsupported-layout", output, 0.4, source_name=None))
        require(any(route["state"] == "error" for route in h.last_status["backend"]["routes"]),
                "Unsupported layout was not reported")
    finally:
        h.stop(unsupported)
    direct = copy.deepcopy(desired)
    direct["routes"] = []
    h.apply(direct)
    quiet = h.audio("direct-virtual-playback", output, 0.02, direct_sink="cosmos.bus.voice.input")[0]
    require(2e-5 < quiet["ratio"] < 0.002, "Direct virtual playback sink did not process application audio")
    h.apply(desired)
    node_serial = h.nodes()[output]["info"]["props"]["object.serial"]

    changed = copy.deepcopy(desired)
    changed["buses"][0]["effects"][0]["effect"]["threshold_db"] = -60.0
    h.apply(changed)
    assert_passes(h.audio("live-threshold", output, 0.02))
    require(h.nodes()[output]["info"]["props"]["object.serial"] == node_serial,
            "Live parameter edit replaced recording endpoint")
    changed["buses"][0]["hidden"] = True
    h.apply(changed)
    assert_passes(h.audio("hidden-is-not-muted", output, 0.02))
    changed["buses"][0]["muted"] = True
    h.apply(changed)
    assert_silent(h.audio("bus-muted", output, 0.4))
    require(h.nodes()[output]["info"]["props"]["object.serial"] == node_serial,
            "Mute replaced recording endpoint")
    changed["buses"][0]["muted"] = False
    changed["routes"][0]["enabled"] = False
    h.apply(changed)
    assert_silent(h.audio("route-disabled", output, 0.4))
    changed["routes"][0]["enabled"] = True
    changed["devices"] = [{"identity": {"node.name": "fixture-source"}, "alias": "My microphone",
                           "hidden": True, "enabled": True, "muted": True}]
    h.apply(changed)
    assert_silent(h.audio("device-muted", output, 0.4))
    changed["devices"][0]["muted"] = False
    h.apply(changed)
    assert_passes(h.audio("device-hidden-alias", output, 0.4))

    # Validation failures must leave both accepted state and persisted bytes unchanged.
    persisted = h.profile_path.read_bytes()
    for label, invalid in (("cycle", copy.deepcopy(changed)), ("parameter", copy.deepcopy(changed))):
        if label == "cycle":
            invalid["routes"].append({"id": "feedback", "from": {"kind": "bus", "id": "voice"},
                                      "to": {"kind": "bus", "id": "voice"}, "enabled": True})
        else:
            invalid["buses"][0]["effects"][0]["effect"]["threshold_db"] = 100.0
        rejected = h.apply(invalid, check=False)
        require(rejected.returncode != 0, f"Invalid {label} profile accepted")
        require(h.profile_path.read_bytes() == persisted, f"Rejected {label} overwrote valid profile")
        require(h.client("profile")["profile"] == changed, f"Rejected {label} changed desired state")

    desired["buses"][0]["effects"].extend([
        {"id": "compressor", "enabled": True, "effect": {
            "kind": "compressor", "threshold_db": -20.0, "ratio": 8.0,
            "attack_ms": 5.0, "release_ms": 20.0, "makeup_db": 0.0}},
        {"id": "eq", "enabled": True, "effect": {
            "kind": "equalizer", "frequency_hz": 1000.0, "gain_db": -12.0, "q": 2.0}},
        {"id": "limiter", "enabled": True, "effect": {
            "kind": "limiter", "ceiling_db": -20.0, "release_ms": 5.0}},
    ])
    h.apply(desired)
    h.wait_bus()
    full = h.audio("ordered-effects", output, 0.8)[0]
    require(0.005 < full["rms"] < 0.1 and full["peak"] <= 0.115,
            "Backend full chain did not process/limit audio")
    require(h.status()["phase"] == "connected" and h.status()["generation"] == healthy_generation,
            "Normal source or topology changes reset the PipeWire connection")
    h.stop(h.engine, crash=True)
    h.start_engine()
    require(h.client("profile")["profile"] == desired, "Engine restart lost desired state")
    h.wait_bus()
    restored = h.audio("engine-restart", output, 0.8)[0]
    require(0.005 < restored["rms"] < 0.1 and restored["peak"] <= 0.115,
            "Engine restart lost actual DSP processing")
    for cycle in range(2):
        h.stop(h.server, crash=True)
        h.wait_for(lambda: h.status()["phase"] == "disconnected", "server loss", (h.engine,))
        h.start_server()
        h.wait_bus()
        restored = h.audio(f"server-restart-{cycle}", output, 0.8)[0]
        require(0.005 < restored["rms"] < 0.1 and restored["peak"] <= 0.115,
                "Server restart lost actual DSP processing")
    multibus = copy.deepcopy(desired)
    multibus["buses"].append({"id": "recording", "name": "Recording", "channels": "mono",
                              "enabled": True, "hidden": False, "muted": False, "effects": []})
    multibus["routes"].append({"id": "to-recording", "from": {"kind": "bus", "id": "voice"},
                               "to": {"kind": "bus", "id": "recording"}, "enabled": True})
    h.apply(multibus)
    h.wait_bus(bus="recording")
    forwarded = h.audio("named-bus-route", "cosmos.bus.recording.output", 0.8)[0]
    require(0.005 < forwarded["rms"] < 0.1 and forwarded["peak"] <= 0.115,
            "Named bus route did not forward processed audio")
    multibus["routes"].append({"id": "feedback", "from": {"kind": "bus", "id": "recording"},
                               "to": {"kind": "bus", "id": "voice"}, "enabled": True})
    require(h.apply(multibus, check=False).returncode != 0, "Multibus feedback cycle accepted")

    stereo = profile("stereo")
    h.apply(stereo)
    h.wait_bus()
    assert_passes(h.audio("stereo-gate", output, 0.4, channels=2))
    stereo["buses"][0]["effects"] = copy.deepcopy(desired["buses"][0]["effects"])
    h.apply(stereo)
    h.wait_bus()
    processed = h.audio("stereo-full-chain", output, 0.8, channels=2)
    require(all(0.005 < item["rms"] < 0.1 and item["peak"] <= 0.115 for item in processed),
            "Stereo effects did not process and limit both channels")
    require(processed[0]["rms"] < processed[1]["rms"] * 0.8,
            "Stereo EQ channel/frequency semantics are incorrect")
    stereo["buses"][0]["enabled"] = False
    h.apply(stereo)
    h.wait_bus("disabled")
    require(h.client("stop")["stopping"], "Final stop not acknowledged")
    require(h.engine.wait(timeout=4) == 0, "Engine did not stop")
    # Missing native plugins must never create a raw fallback path.
    missing = h.root / "empty-lv2"
    missing.mkdir()
    h.env["LV2_PATH"] = str(missing)
    stereo["buses"][0]["enabled"] = True
    h.start_engine()
    h.apply(stereo)
    h.wait_bus("error")
    require("cosmos.bus.voice.input" not in h.nodes() and output not in h.nodes(),
            "Missing mandatory plugin published an unprocessed endpoint")
    h.client("stop")
    require(h.engine.wait(timeout=4) == 0, "Failed-plugin engine did not stop")
    del h.env["LV2_PATH"]
    h.start_engine()
    h.wait_bus()
    processed = h.audio("plugin-restored", output, 0.8, channels=2)
    require(all(0.005 < item["rms"] < 0.1 and item["peak"] <= 0.115 for item in processed),
            "Restored stereo plugins did not process both channels")
    h.client("stop")
    require(h.engine.wait(timeout=4) == 0, "Restored engine did not stop")

    corrupt = b'{\"version\":1,\"buses\":['
    h.profile_path.write_bytes(corrupt)
    h.start_engine()
    require(h.status()["config_error"], "Corrupt profile not reported")
    require(h.profile_path.read_bytes() == corrupt, "Corrupt profile silently overwritten")
    require(h.client("profile")["profile"]["buses"] == [], "Corrupt profile created audio graph")
    h.apply(profile())
    require(h.client("profile")["profile"] == profile(), "Explicit corrupt-profile recovery failed")
    backups = [path for path in h.config.iterdir() if path.is_file() and path != h.profile_path]
    require(any(path.read_bytes() == corrupt for path in backups),
            "Explicit recovery destroyed the only corrupt original")
    h.client("stop")
    require(h.engine.wait(timeout=4) == 0, "Corrupt-profile engine did not stop")
    print("PASS: native backend DSP/routes, live controls, silence, mono/stereo, persistence and recovery")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="cosmos-backend-", dir="/var/tmp"))
    harness = None
    passed = False
    try:
        harness = BackendHarness(root)
        verify(harness)
        passed = True
    except BaseException:
        if harness is not None:
            for name, query in (("failure-status.json", harness.status), ("failure-graph.json", harness.snapshot)):
                try:
                    (root / name).write_text(json.dumps(query(), indent=2))
                except Exception as error:
                    print(f"Could not collect {name}: {error}", file=sys.stderr)
        raise
    finally:
        if harness is not None:
            harness.close()
        if passed and not args.keep:
            shutil.rmtree(root)
        else:
            print(f"Private backend artifacts: {root}", file=sys.stderr)


if __name__ == "__main__":
    main()
