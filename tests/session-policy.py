#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Check mixer coexistence with real WirePlumber on a synthetic-only server."""
import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("backend", ROOT / "tests/backend-audio.py")
BACKEND = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BACKEND)
require = BACKEND.require


def linked_to(h, player_name, sink_name):
    graph = h.snapshot()
    nodes = {item.get("info", {}).get("props", {}).get("node.name"): item["id"]
             for item in graph if item["type"] == "PipeWire:Interface:Node"}
    links = [item["info"] for item in graph if item["type"] == "PipeWire:Interface:Link"
             and item["info"]["output-node-id"] == nodes.get(player_name)]
    return (len(links) == 2 and all(link["input-node-id"] == nodes.get(sink_name)
                                  and link["state"] == "active" for link in links))


def meters_live(h, output=False):
    buses = h.status()["backend"]["buses"]
    keys = ("input_meter", "output_meter") if output else ("input_meter",)
    return buses and buses[0]["state"] == "ready" and all(
        (meter := buses[0][key]) and meter["fresh"] and meter["error"] is None
        and 0.3 < meter["peak"][0] < 0.5 for key in keys)


def policy_ready(h, policy):
    graph = h.snapshot()
    clients = {item["id"] for item in graph if item["type"] == "PipeWire:Interface:Client"
               and item.get("info", {}).get("props", {}).get("pipewire.sec.pid") == policy.pid}
    return any(item["type"] == "PipeWire:Interface:Metadata"
               and item.get("props", {}).get("metadata.name") == "default"
               and item["props"].get("client.id") in clients
               and any(entry["key"] == "default.audio.sink" for entry in item.get("metadata", []))
               for item in graph)


def capture_bus(h, count, label):
    # WirePlumber, not pw-link, must connect this normal recording application.
    capture = h.root / f"{label}.f32"
    recorder = h.spawn(label, "pw-cat", "--record", "--raw", "--format", "f32",
                       "--rate", BACKEND.RATE, "--channels", count, "--latency", "256",
                       "--target", "cosmos.bus.voice.output", "--sample-count", 2 * BACKEND.RATE,
                       "--properties", "{ node.autoconnect = true }", "-", output=capture)
    # An idle public source need not process audio. Both meters must be live
    # while a real consumer is recording.
    h.wait_for(lambda: meters_live(h, output=True), "live recording meters", (h.engine, recorder))
    require(recorder.wait(timeout=8) == 1, "Native finite recorder did not reach its sample limit")
    samples = BACKEND.array.array("f")
    samples.frombytes(capture.read_bytes())
    require(len(samples) == 2 * BACKEND.RATE * count, "Incorrect application capture length")
    require(all(BACKEND.math.isfinite(value) for value in samples), "Non-finite application audio")
    for channel in range(count):
        steady = samples[BACKEND.RATE * count + channel::count]
        rms = BACKEND.math.sqrt(sum(value * value for value in steady) / len(steady))
        require(0.25 < rms < 0.31, f"Application did not receive processed audio: RMS {rms}")


def verify_daemon_metadata(root, engine):
    h = BACKEND.BackendHarness(root)
    h.tools["engine"] = str(engine)
    try:
        server = h.config / "server.conf"
        server.write_text(server.read_text().rstrip()[:-1] + """
    { factory = metadata args = { metadata.name = default } }
]
""")
        h.start_server()
        # Only the fixtures inherit DSP auto-configuration. The engine must
        # choose its own configuration when daemon metadata has no policy owner.
        engine_config = root / "engine-config"
        engine_config.mkdir(mode=0o700)
        (engine_config / "client.conf").write_text(
            (h.config / "client.conf").read_text().split("stream.properties")[0])
        h.env["PIPEWIRE_CONFIG_DIR"] = str(engine_config)
        try:
            h.start_engine()
        finally:
            h.env["PIPEWIRE_CONFIG_DIR"] = str(h.config)
        h.apply(BACKEND.profile())
        BACKEND.assert_passes(h.audio("unmanaged-default-metadata", "cosmos.bus.voice.output", 0.4))
        BACKEND.assert_meter(h, 0.4)
        require(h.status()["discovery_error"] is None, "Unmanaged graph has a discovery failure")
        print("PASS: daemon-owned metadata does not require a session manager", flush=True)
    finally:
        h.close()


def verify(root, engine, channels, policy_first):
    h = BACKEND.BackendHarness(root)
    h.tools.update(engine=str(engine), wireplumber="/usr/bin/wireplumber", wpctl="/usr/bin/wpctl")
    # Do not inherit the DSP-only harness's global autoconnect/port overrides:
    # public streams must exercise the real session-manager configuration path.
    client = h.config / "client.conf"
    client.write_text(client.read_text().split("stream.properties")[0])
    h.env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={h.runtime}/no-session-bus"
    h.env["DBUS_SYSTEM_BUS_ADDRESS"] = f"unix:path={h.runtime}/no-system-bus"
    try:
        # Keep distro policy code/settings, but use only native private sockets.
        # The policy profile has no hardware monitors. Disable all D-Bus users,
        # including optional features, so neither desktop bus can be reached.
        config = root / "wireplumber.conf"
        config.write_text(Path("/usr/share/wireplumber/wireplumber.conf").read_text() + """
override.context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-metadata }
]
wireplumber.profiles = { policy = {
    support.dbus = disabled
    support.system-dbus = disabled
    support.logind = disabled
    support.portal-permissionstore = disabled
    script.client.access-portal = disabled
    hardware.audio = disabled
    hardware.bluetooth = disabled
    hardware.video-capture = disabled
} }
""")
        server = h.config / "server.conf"
        server.write_text(server.read_text().rstrip()[:-1] + """
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = fixture-sink-a node.description = "Synthetic output A"
        media.class = Audio/Sink audio.channels = 2 audio.position = [ FL FR ]
    } }
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = fixture-sink-b node.description = "Synthetic output B"
        media.class = Audio/Sink audio.channels = 2 audio.position = [ FL FR ]
    } }
]
""")
        h.start_server()
        policy = None
        if policy_first:
            policy = h.spawn("wireplumber", "wireplumber", "-c", config, "-p", "policy")
            h.wait_for(lambda: policy_ready(h, policy), "policy-first readiness", (policy,))
        h.start_engine()
        h.apply(BACKEND.profile(channels))
        count = 1 if channels == "mono" else 2
        source = root / "source.f32"
        source.write_bytes(BACKEND.array.array("f", (
            0.4 * BACKEND.math.sin(2 * BACKEND.math.pi * (1000 + channel * 1000) * frame / BACKEND.RATE)
            for frame in range(30 * BACKEND.RATE) for channel in range(count))).tobytes())
        h.spawn("source", "pw-cat", "--playback", "--raw", "--format", "f32",
                "--rate", BACKEND.RATE, "--channels", count, "--latency", "256",
                "--channel-map", "MONO" if count == 1 else "FL,FR", "--target", "0", "--properties",
                '{ node.name = fixture-source item.features.no-format = true '
                'adapter.auto-port-config = { mode = dsp } }', source)
        h.wait_for(lambda: meters_live(h), "processed audio and live meters before policy restart",
                   (h.engine,), timeout=10)
        if policy is None:
            policy = h.spawn("wireplumber", "wireplumber", "-c", config, "-p", "policy")

        # An unrelated application must link and follow both default changes.
        silence = root / "silence.f32"
        silence.write_bytes(bytes(30 * BACKEND.RATE * 2 * 4))
        player = h.spawn("desktop-player", "pw-cat", "--playback", "--raw", "--format", "f32",
                         "--rate", BACKEND.RATE, "--channels", 2, "--latency", "256", "--properties",
                         '{ node.name = fixture-player node.autoconnect = true }', silence)
        h.wait_for(lambda: policy_ready(h, policy), "desktop policy readiness", (policy,))
        for name in ("fixture-sink-b", "fixture-sink-a"):
            h.wait_for(lambda: name in h.nodes(), "synthetic output discovery", (policy,))
            h.command("wpctl", "set-default", h.nodes()[name]["id"])
            h.wait_for(lambda: linked_to(h, "fixture-player", name),
                       f"desktop playback following {name}", (policy, player, h.engine))
        # Endpoint reconfiguration can remove links; allow the engine's 5 s retry.
        h.wait_for(lambda: meters_live(h), "meter recovery after policy activation",
                   (policy, h.engine), timeout=10)
        capture_bus(h, count, "application-capture")
        # A session-manager restart must not take ownership of running ports.
        h.stop(policy)
        policy = h.spawn("wireplumber-restarted", "wireplumber", "-c", config, "-p", "policy")
        h.wait_for(lambda: policy_ready(h, policy), "restarted desktop policy readiness", (policy,))
        h.command("wpctl", "set-default", h.nodes()["fixture-sink-b"]["id"])
        h.wait_for(lambda: linked_to(h, "fixture-player", "fixture-sink-b"),
                   "desktop playback after policy restart", (policy, player, h.engine))
        capture_bus(h, count, "capture-after-policy-restart")
        # Exercise the public virtual sink too, without an engine-owned source route.
        desired = BACKEND.profile(channels)
        desired["routes"] = []
        h.apply(desired)
        direct = h.spawn("direct-playback", "pw-cat", "--playback", "--raw", "--format", "f32",
                         "--rate", BACKEND.RATE, "--channels", count, "--latency", "256",
                         "--target", "cosmos.bus.voice.input",
                         "--properties", "{ node.autoconnect = true }", source)
        capture_bus(h, count, "capture-direct-playback")
        require(direct.poll() is None, "Direct playback ended during recording")
        require(h.status()["discovery_error"] is None, "Policy change left a discovery failure")
        (root / "final-status.json").write_text(json.dumps(h.status(), indent=2))
        (root / "final-graph.json").write_text(json.dumps(h.snapshot(), indent=2))
        print(f"PASS: {channels}, {'policy' if policy_first else 'engine'} first: "
              "device switching, DSP, meters, policy restart", flush=True)
    except BaseException:
        (root / "failure-status.json").write_text(json.dumps(h.status(), indent=2))
        (root / "failure-graph.json").write_text(json.dumps(h.snapshot(), indent=2))
        raise
    finally:
        h.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, default=ROOT / "target/debug/cosmos-engine")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="cosmos-policy-", dir="/var/tmp"))
    passed = False
    try:
        unmanaged = root / "daemon-metadata"
        unmanaged.mkdir(mode=0o700)
        verify_daemon_metadata(unmanaged, args.engine.resolve())
        for channels in ("mono", "stereo"):
            for policy_first in (False, True):
                case = root / f"{channels}-{'policy' if policy_first else 'engine'}-first"
                case.mkdir(mode=0o700)
                verify(case, args.engine.resolve(), channels, policy_first)
        passed = True
    finally:
        if passed and not args.keep:
            shutil.rmtree(root)
        else:
            print(f"Private policy artifacts: {root}", file=sys.stderr)


if __name__ == "__main__":
    main()
