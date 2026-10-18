import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import time
import uuid


ROOT = Path(__file__).resolve().parents[2]
DOCKER = "/usr/local/bin/docker"
CONFIG = Path("/Users/chaoticqubit/Projects/CrazyAgent/.local-state/tools/docker-config")
ENDPOINT = "unix:///Users/chaoticqubit/.docker/run/docker.sock"
DIGEST = "sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9"
IMAGE = "python@" + DIGEST
MEMORY = 64 * 1024 * 1024
OUTPUT_LIMIT = 32768
EXECUTION_SECONDS = 14
LABEL = "io.crazyagent.resource-prototype"
PROBES = ("memory_control", "memory_limit", "pids_control", "pids_limit", "cpu_limit", "deadline")
PAYLOAD = r'''
import errno
import json
import os
from pathlib import Path
import sys
import time

def snapshot():
    base = Path("/sys/fs/cgroup")
    names = ("memory.max", "memory.swap.max", "memory.current", "memory.events", "pids.max", "pids.current", "pids.events", "cpu.max", "cpu.stat")
    return {name: (base / name).read_text().strip() for name in names}

def emit(phase, **values):
    print(json.dumps({"phase": phase, **values}), flush=True)

name = sys.argv[1]
emit("before", cgroup=snapshot(), uid=os.getuid(), gid=os.getgid())
if name in ("memory_control", "memory_limit"):
    size = (8 if name == "memory_control" else 96) * 1024 * 1024
    emit("allocation", requested_bytes=size)
    block = bytearray(size)
    for offset in range(0, size, 4096):
        block[offset] = 1
    emit("result", touched_bytes=size)
elif name in ("pids_control", "pids_limit"):
    read_fd, write_fd = os.pipe()
    children = []
    failures = []
    try:
        for attempt in range(6):
            try:
                pid = os.fork()
            except OSError as error:
                if error.errno != errno.EAGAIN:
                    raise
                failures.append(error.errno)
                continue
            if pid == 0:
                os.close(write_fd)
                os.read(read_fd, 1)
                os._exit(0)
            children.append(pid)
        emit("result", attempts=6, children=len(children), failures=len(failures), errnos=failures, held=snapshot())
    finally:
        os.close(write_fd)
        os.close(read_fd)
        for pid in children:
            waited, status = os.waitpid(pid, 0)
            if waited != pid or status != 0:
                raise RuntimeError("child did not exit cleanly")
elif name == "cpu_limit":
    started = time.monotonic()
    iterations = 0
    while time.monotonic() - started < 3.5:
        iterations += 1
    emit("result", elapsed_seconds=time.monotonic() - started, iterations=iterations)
elif name == "deadline":
    time.sleep(30)
else:
    raise ValueError("unknown fixed probe")
emit("after", cgroup=snapshot())
'''


def invariant(condition, message):
    if not condition:
        raise AssertionError(message)


def run_command(command, deadline):
    started = time.monotonic()
    if deadline <= started:
        raise ValueError("deadline must be in the future")
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    counts = {"stdout": 0, "stderr": 0}
    timed_out = False
    interrupted = False
    process = subprocess.Popen(
        command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        start_new_session=True, env={"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C"},
    )
    with selectors.DefaultSelector() as selector:
        try:
            for name in streams:
                stream = getattr(process, name)
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, name)
            while selector.get_map() or process.poll() is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    timed_out = True
                    break
                for key, _ in selector.select(min(remaining, 0.05)):
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    name = key.data
                    counts[name] += len(data)
                    streams[name].extend(data[:max(0, OUTPUT_LIMIT - len(streams[name]))])
        except KeyboardInterrupt:
            interrupted = True
        finally:
            if timed_out or interrupted or process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.wait(timeout=2)
            for name in streams:
                stream = getattr(process, name)
                for attempt in range(16):
                    try:
                        data = os.read(stream.fileno(), 65536)
                    except BlockingIOError:
                        break
                    if not data:
                        break
                    counts[name] += len(data)
                    streams[name].extend(data[:max(0, OUTPUT_LIMIT - len(streams[name]))])
                stream.close()
    invariant(all(len(value) <= OUTPUT_LIMIT for value in streams.values()), "capture exceeded bound")
    invariant(all(counts[name] >= len(streams[name]) for name in streams), "capture count lost bytes")
    invariant(process.returncode is not None, "client was not reaped")
    return {
        "command": command, "returncode": process.returncode,
        "timed_out": timed_out, "interrupted": interrupted,
        "elapsed_seconds": time.monotonic() - started,
        "truncated": any(counts[name] > OUTPUT_LIMIT for name in streams),
        **{name: value.decode("utf-8", errors="replace") for name, value in streams.items()},
        **{name + "_bytes": value for name, value in counts.items()},
    }


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def docker(context, arguments, deadline=None, required=True):
    previous = context["sequence"]
    invariant(previous >= 0, "command sequence became negative")
    result = run_command(
        [DOCKER, "--config", str(CONFIG), "--host", ENDPOINT, *arguments],
        deadline if deadline is not None else time.monotonic() + 5,
    )
    context["sequence"] += 1
    invariant(context["sequence"] == previous + 1, "command sequence lost ordering")
    save(context["artifacts"] / "commands" / (str(context["sequence"]).zfill(4) + ".json"), result)
    if required and (result["returncode"] != 0 or result["timed_out"] or result["interrupted"] or result["truncated"]):
        raise RuntimeError("Docker command unavailable: " + arguments[0] + ": " + result["stderr"])
    return result


def require(condition, message):
    if not condition:
        raise ValueError(message)


def inspect_container(context, identity):
    data = json.loads(docker(context, ["inspect", "--type", "container", identity])["stdout"])
    require(isinstance(data, list) and len(data) == 1, "expected one inspected container")
    return data[0]


def verify_identity(data, token, name):
    require(re.fullmatch(r"[0-9a-f]{64}", data.get("Id", "")), "invalid full container ID")
    require(data.get("Name") == "/" + name, "container name mismatch; no mutation permitted")
    require(data.get("Image") == DIGEST, "container image ID mismatch; no mutation permitted")
    config = data.get("Config", {})
    require(config.get("Image") == IMAGE, "container image reference mismatch")
    require(config.get("Labels", {}).get(LABEL) == token, "ownership label mismatch; no mutation permitted")


def verify_stopped(data, token, name, probe):
    verify_identity(data, token, name)
    config = data["Config"]
    host = data["HostConfig"]
    require(data["State"]["Status"] == "created" and not data["State"]["Running"], "container must be stopped before verification")
    require(config.get("User") == "65534:65534", "unprivileged identity missing")
    require(config.get("Cmd") == ["python3", "-I", "-B", "-u", "-c", PAYLOAD, probe], "fixed workload mismatch")
    require(not config.get("Entrypoint"), "unexpected image entrypoint")
    expected = {
        "Memory": MEMORY, "MemorySwap": MEMORY, "NanoCpus": 500000000,
        "PidsLimit": 4 if probe == "pids_limit" else 16,
        "NetworkMode": "none", "ReadonlyRootfs": True, "Privileged": False,
        "PidMode": "", "IpcMode": "none", "CgroupnsMode": "private", "AutoRemove": False,
    }
    invariant(0 < expected["Memory"] <= 128 * 1024 * 1024, "internal memory plan exceeds authorization")
    invariant(0 < expected["PidsLimit"] <= 16, "internal PID plan exceeds authorization")
    for key, value in expected.items():
        require(host.get(key) == value, "HostConfig mismatch: " + key)
    require(host.get("CapDrop") == ["ALL"], "capabilities were not all dropped")
    require(host.get("SecurityOpt") == ["no-new-privileges:true"], "no-new-privileges missing")
    require(host.get("RestartPolicy", {}).get("Name") == "no", "restart policy mismatch")
    require(host.get("LogConfig", {}).get("Type") == "none", "daemon logging must be disabled")
    for key in ("CapAdd", "Binds", "Tmpfs", "Devices", "DeviceRequests", "VolumesFrom", "PortBindings", "PublishAllPorts", "Init"):
        require(not host.get(key), "forbidden HostConfig value: " + key)
    require(not data.get("Mounts"), "host or image mounts are forbidden")


def cleanup(context, token, name, known_id, directory):
    result = {"confirmed": False, "ids": []}
    listed = docker(context, ["ps", "--all", "--quiet", "--no-trunc", "--filter", "label=" + LABEL + "=" + token])
    ids = listed["stdout"].split()
    require(len(ids) <= 1, "multiple exact-label matches; cleanup identity uncertain")
    require(bool(ids), "no exact-label match after create attempt; cleanup uncertain")
    require(not known_id or ids == [known_id], "created ID differs from reconciled ID")
    data = inspect_container(context, ids[0])
    verify_identity(data, token, name)
    result["ids"] = ids
    if data["State"]["Running"]:
        docker(context, ["kill", data["Id"]])
        data = inspect_container(context, data["Id"])
        verify_identity(data, token, name)
    require(not data["State"]["Running"], "owned container is still running")
    save(directory / "after.json", data)
    result["state"] = data["State"]
    docker(context, ["rm", data["Id"]])
    listed = docker(context, ["ps", "--all", "--quiet", "--no-trunc", "--filter", "label=" + LABEL + "=" + token])
    require(not listed["stdout"].strip(), "owned container remains after removal")
    result["confirmed"] = True
    invariant(len(result["ids"]) == 1, "cleanup must remove exactly one owned ID")
    invariant(not result["state"]["Running"], "cleanup confirmation lost stopped state")
    return result


def counters(value):
    return {key: int(count) for key, count in (line.split() for line in value.splitlines())}


def docker_timestamp_ns(value):
    """Convert a Docker RFC3339 timestamp to integer UTC epoch nanoseconds.
    Accept a timezone and up to nine fractional digits as input.
    Preserve fractional precision without Python 3.9 datetime truncation.
    """
    require(isinstance(value, str), "Docker timestamp must be a string")
    match = re.fullmatch(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)", value)
    require(match is not None, "invalid Docker RFC3339 timestamp")
    whole = datetime.fromisoformat(match[1] + match[3].replace("Z", "+00:00"))
    delta = whole - datetime(1970, 1, 1, tzinfo=timezone.utc)
    nanoseconds = int((match[2] or "").ljust(9, "0"))
    invariant(whole.microsecond == 0, "whole-second parser retained a fraction")
    invariant(0 <= nanoseconds < 1000000000, "fraction exceeded one second")
    return (delta.days * 86400 + delta.seconds) * 1000000000 + nanoseconds


def evaluate(probe, evidence):
    invariant(probe in PROBES, "evaluation received an unplanned workload")
    invariant(evidence["cleanup"]["confirmed"], "evaluation preceded cleanup confirmation")
    records = [json.loads(line) for line in evidence["execution"]["stdout"].splitlines()]
    phases = {record["phase"]: record for record in records}
    require("before" in phases, "container produced no pre-execution cgroup record")
    before = phases["before"]["cgroup"]
    evidence["before"] = before
    require(phases["before"]["uid"] == 65534 and phases["before"]["gid"] == 65534, "actual container UID/GID mismatch")
    expected = {"memory.max": str(MEMORY), "memory.swap.max": "0", "pids.max": "4" if probe == "pids_limit" else "16", "cpu.max": "50000 100000"}
    for key, value in expected.items():
        require(before.get(key) == value, "actual cgroup mismatch: " + key)
    execution = evidence["execution"]
    state = evidence["state"]
    require(not execution["truncated"] and not execution["interrupted"], "incomplete execution stream")
    require(execution["elapsed_seconds"] < 15, "external execution exceeded 15 seconds")
    started = docker_timestamp_ns(state["StartedAt"])
    finished = docker_timestamp_ns(state["FinishedAt"])
    evidence["running_nanoseconds"] = finished - started
    require(0 <= evidence["running_nanoseconds"] <= 15000000000, "actual container runtime exceeded 15 seconds")
    evidence["running_seconds"] = evidence["running_nanoseconds"] / 1000000000
    require(0 <= evidence["running_seconds"] <= 15, "actual container runtime exceeded 15 seconds")
    if probe == "deadline":
        require(execution["timed_out"] and state["ExitCode"] == 137 and not state["OOMKilled"], "watchdog did not stop actual container")
        require("after" not in phases, "deadline workload completed unexpectedly")
        return
    require(not execution["timed_out"], "unexpected execution deadline")
    if probe == "memory_limit":
        require(phases.get("allocation", {}).get("requested_bytes") == 96 * 1024 * 1024, "missing bounded allocation attempt")
        require(state["OOMKilled"] and state["ExitCode"] == 137, "cgroup OOM enforcement was not observed")
        evidence["post_cgroup"] = "unavailable after OOM; pre-counters and Docker State retained"
        return
    require(state["ExitCode"] == 0 and not state["OOMKilled"], "positive/workload execution failed")
    result = phases["result"]
    evidence["result"] = result
    evidence["after"] = phases["after"]["cgroup"]
    if probe == "memory_control":
        require(result["touched_bytes"] == 8 * 1024 * 1024, "small allocation did not complete")
    elif probe in ("pids_control", "pids_limit"):
        require(result["attempts"] == 6 and result["children"] + result["failures"] == 6, "fork attempts unaccounted for")
        delta = counters(result["held"]["pids.events"])["max"] - counters(before["pids.events"])["max"]
        evidence["pids_max_events_delta"] = delta
        if probe == "pids_control":
            require(result["children"] == 6 and result["failures"] == 0, "six-child control failed")
        else:
            require(0 < result["children"] <= 3 and result["failures"] > 0, "PID cap did not distinguish successful and denied children")
            require(result["errnos"] == [11] * result["failures"] and delta > 0, "kernel EAGAIN/PID-event evidence missing")
    elif probe == "cpu_limit":
        previous = counters(before["cpu.stat"])
        current = counters(evidence["after"]["cpu.stat"])
        delta = {key: current[key] - previous[key] for key in ("usage_usec", "nr_throttled", "throttled_usec")}
        evidence["cpu_delta"] = delta
        elapsed = result["elapsed_seconds"]
        require(3.5 <= elapsed <= 5, "CPU workload elapsed time outside measurement window")
        require(delta["nr_throttled"] > 0 and delta["throttled_usec"] > 0, "CPU throttling was not observed")
        require(elapsed * 100000 <= delta["usage_usec"] <= elapsed * 650000 + 100000, "CPU usage outside 0.10-0.65 CPU plus 100ms tolerance")


def execute_probe(context, probe):
    invariant(probe in PROBES, "execution received an unplanned workload")
    token = context["run"] + "." + probe
    name = "crazyagent-resource-" + token.replace("_", "-").replace(".", "-")
    directory = context["artifacts"] / probe
    directory.mkdir()
    evidence = {"status": "unavailable", "cleanup": {"confirmed": False}, "token": token, "name": name}
    identity = None
    verified = False
    invariant(not verified, "new probe must not inherit another container's verification")
    try:
        created = docker(context, [
            "create", "--pull=never", "--name", name, "--label", LABEL + "=" + token,
            "--memory", str(MEMORY), "--memory-swap", str(MEMORY), "--cpus", "0.5",
            "--pids-limit", "4" if probe == "pids_limit" else "16", "--network", "none",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true", "--user", "65534:65534",
            "--read-only", "--restart", "no", "--log-driver", "none", "--ipc", "none", "--cgroupns", "private",
            IMAGE, "python3", "-I", "-B", "-u", "-c", PAYLOAD, probe,
        ])
        identity = created["stdout"].strip()
        require(re.fullmatch(r"[0-9a-f]{64}", identity), "create did not return a full container ID")
        before = inspect_container(context, identity)
        save(directory / "before.json", before)
        verify_stopped(before, token, name, probe)
        verified = True
        execution = docker(context, ["start", "--attach", identity], time.monotonic() + EXECUTION_SECONDS, required=False)
        evidence["execution"] = execution
        save(directory / "execution.json", execution)
        if execution["timed_out"] or execution["interrupted"]:
            docker(context, ["kill", identity])
    except (OSError, ValueError, RuntimeError, KeyError, TypeError, KeyboardInterrupt) as error:
        evidence["error"] = str(error) or "interrupted"
    finally:
        try:
            evidence["cleanup"] = cleanup(context, token, name, identity, directory)
            evidence["state"] = evidence["cleanup"]["state"]
        except (OSError, ValueError, RuntimeError, KeyboardInterrupt) as error:
            evidence["cleanup"]["error"] = str(error) or "cleanup interrupted"
        if verified and "execution" in evidence and evidence["cleanup"]["confirmed"] and "error" not in evidence:
            try:
                evaluate(probe, evidence)
                evidence["status"] = "proven"
            except (ValueError, KeyError, TypeError) as error:
                evidence["status"] = "failed"
                evidence["error"] = str(error)
        save(directory / "evidence.json", evidence)
    return evidence


def preflight(context):
    require(json.loads((CONFIG / "config.json").read_text()) == {"auths": {}}, "Docker config must contain only empty auths")
    require(json.loads(docker(context, ["info", "--format", "{{json .CgroupVersion}}"])["stdout"]) == "2", "cgroup v2 required")
    images = json.loads(docker(context, ["image", "inspect", IMAGE])["stdout"])
    require(len(images) == 1, "expected one acquired image")
    image = images[0]
    require(image["Id"] == DIGEST and IMAGE in image["RepoDigests"], "acquired image identity mismatch")
    require(image["Os"] == "linux" and image["Architecture"] == "arm64", "acquired image platform mismatch")
    require(image["Size"] == 85931133, "acquired image size mismatch")
    save(context["artifacts"] / "image.json", image)
    return {key: image[key] for key in ("Id", "RepoDigests", "Os", "Architecture", "Size")}


def interrupted(signum, frame):
    raise KeyboardInterrupt("signal " + str(signum))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", type=Path)
    arguments = parser.parse_args()
    run = uuid.uuid4().hex
    artifacts = arguments.artifacts or ROOT / ".local-state" / "container-probe" / run
    artifacts = artifacts.resolve()
    require(ROOT / ".local-state" in artifacts.parents, "artifacts must remain in this checkout's .local-state")
    artifacts.mkdir(parents=True, exist_ok=False)
    context = {"artifacts": artifacts, "run": run, "sequence": 0}
    report = {
        "status": "unavailable", "run": run, "artifacts": str(artifacts), "probes": {},
        "permanent_runtime_selected": False, "native_admitted": False,
        "aggregate_cpu_time_budget_proven": False, "clean_container_confirmation": False,
        "limits": {"memory_bytes": MEMORY, "memory_swap_bytes": MEMORY, "cpus": 0.5, "pids": [4, 16], "external_deadline_seconds": EXECUTION_SECONDS, "maximum_running_seconds": 15, "output_bytes_per_stream": OUTPUT_LIMIT},
        "gaps": ["aggregate CPU-time budget", "worker filesystem/network/credential policy", "native auth/control/recovery", "Docker control-plane access and crash recovery", "production packaging"],
    }
    signal.signal(signal.SIGTERM, interrupted)
    try:
        with (ROOT / ".local-state" / "container-probe.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            report["image"] = preflight(context)
            for probe in PROBES:
                evidence = execute_probe(context, probe)
                report["probes"][probe] = evidence
                save(artifacts / "report.json", report)
                if evidence["status"] == "unavailable" or not evidence["cleanup"]["confirmed"]:
                    break
            report["clean_container_confirmation"] = bool(report["probes"]) and all(item["cleanup"]["confirmed"] for item in report["probes"].values())
            statuses = [item["status"] for item in report["probes"].values()]
            if len(statuses) == len(PROBES) and all(status == "proven" for status in statuses):
                report["status"] = "proven"
            elif "failed" in statuses:
                report["status"] = "failed"
    except (OSError, ValueError, RuntimeError, KeyboardInterrupt) as error:
        report["error"] = str(error) or "interrupted"
    finally:
        save(artifacts / "report.json", report)
    print(json.dumps({"status": report["status"], "artifacts": str(artifacts), "error": report.get("error")}))
    return 0 if report["status"] == "proven" else 1


if __name__ == "__main__":
    raise SystemExit(main())
