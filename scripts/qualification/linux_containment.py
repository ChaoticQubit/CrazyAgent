import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import threading
import time
import uuid

from container_resources import CONFIG, DOCKER, ENDPOINT, IMAGE, MEMORY, OUTPUT_LIMIT, docker_timestamp_ns


CASES = ("boundary", "memory", "pids", "cpu", "deadline")
LABEL = "io.crazyagent.runtime-followup"
ENVIRONMENT = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C"}
TOTAL_LIMIT = 4 * 1024 * 1024
ROOT = Path(__file__).resolve().parents[2]

PAYLOAD = r'''
import errno
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import socket
import sys
import time

def invariant(condition, message):
    assert condition, message

def require(condition, message):
    if not condition:
        raise ValueError(message)

def observe_errno(operation):
    try:
        operation()
    except OSError as error:
        return error.errno
    return 0

def denied(operation, allowed):
    result = observe_errno(operation)
    require(result in allowed, "unexpected syscall errno: " + str(result))
    return result

def snapshot():
    base = Path("/sys/fs/cgroup")
    names = ("memory.max", "memory.swap.max", "memory.current", "memory.events", "pids.max", "pids.current", "pids.events", "cpu.max", "cpu.stat")
    return {name: (base / name).read_text()[:4096].strip() for name in names}

def counters(value):
    return {key: int(count) for key, count in (line.split() for line in value.splitlines())}

def emit(phase, **values):
    value = json.dumps({"phase": phase, **values}, sort_keys=True)
    require(len(value) < 16384, "payload record overflow")
    print(value, flush=True)

def reap(pid, deadline):
    invariant(pid > 0 and pid != os.getpid(), "invalid owned child")
    for attempt in range(1000):
        result, status = os.waitpid(pid, os.WNOHANG)
        if result == pid:
            return status
        if time.monotonic() >= deadline:
            os.kill(pid, signal.SIGKILL)
            result, status = os.waitpid(pid, 0)
            invariant(result == pid, "owned child not reaped")
            raise ValueError("child exceeded internal deadline")
        time.sleep(0.005)
    raise ValueError("child wait bound exhausted")

def touch(size):
    require(0 < size <= 96 * 1024 * 1024, "allocation bound")
    block = bytearray(size)
    for offset in range(0, size, 4096):
        block[offset] = 1
    invariant(block[0] == 1, "allocation not touched")
    return len(block)

def validate_topology(observation):
    require(observation["complete"], "incomplete topology observation")
    interfaces = observation["interfaces"]
    require([name for index, name in interfaces] == ["lo"], "non-loopback interface")
    routes, routes6 = observation["ipv4_routes"], observation["ipv6_routes"]
    require(not any(line.split()[1] == "00000000" and int(line.split()[3], 16) & 1 and not int(line.split()[3], 16) & 0x200 for line in routes), "default IPv4 route")
    require(not any(line.split()[0] == "0" * 32 and line.split()[1] == "00" and int(line.split()[8], 16) & 1 and not int(line.split()[8], 16) & 0x200 for line in routes6), "default IPv6 route")

def topology(role="worker", ipv4_path=Path("/proc/net/route"), ipv6_path=Path("/proc/net/ipv6_route")):
    invariant(role in ("worker", "child"), "unknown topology actor")
    interfaces = socket.if_nameindex()
    observation = {"interfaces": interfaces[:64], "complete": len(interfaces) <= 64, "read_errors": {}}
    for name, path, header in (("ipv4_routes", ipv4_path, 1), ("ipv6_routes", ipv6_path, 0)):
        try:
            with path.open("r") as source:
                text = source.read(4097)
            observation[name] = text[:4096].splitlines()[header:]
            observation["complete"] &= len(text) <= 4096
        except OSError as error:
            observation[name] = []
            observation["read_errors"][name] = error.errno
            observation["complete"] = False
    invariant(len(observation["interfaces"]) <= 64, "interface retention bound lost")
    emit("topology-" + role, **observation)
    validate_topology(observation)
    return observation

def exchange(address, family, nonce):
    with socket.socket(family, socket.SOCK_STREAM) as client:
        client.settimeout(0.25)
        client.connect(address)
        client.sendall(nonce)
        require(client.recv(64) == nonce, "local nonce exchange failed")

def filesystem_actor(manifest, role, port, unix_path):
    nonce = manifest["run_id"].encode()
    for name, expected in manifest["canaries"].items():
        require(hashlib.sha256((Path("/protected") / name).read_bytes()).hexdigest() == expected["sha256"], "canary read failed")
    (Path("/workspace") / (role + ".txt")).write_bytes(nonce + b":" + role.encode() + b"\n")
    (Path("/tmp") / (role + ".txt")).write_bytes(nonce)
    errors = {}
    allowed = (errno.EROFS, errno.EACCES, errno.EPERM)
    for name, path in (("direct", "/protected/sentinel.txt"), ("traversal", "/workspace/../protected/sentinel.txt"), ("symlink", "/workspace/escape"), ("root", "/etc/hostname"), ("payload", "/probe/probe.py")):
        errors[name] = denied(lambda p=path: Path(p).write_bytes(b"forbidden"), allowed)
    errors["host_path"] = denied(lambda: os.lstat(manifest["host_engine"]), (errno.ENOENT,))
    errors["host_symlink"] = denied(lambda: Path("/workspace/host-escape").read_bytes(), (errno.ENOENT,))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(0.25)
        errors["host_socket"] = denied(lambda: client.connect(manifest["host_engine"] + "/engine.sock"), (errno.ENOENT,))
    network = topology(role)
    exchange(("127.0.0.1", port), socket.AF_INET, nonce)
    exchange(unix_path, socket.AF_UNIX, nonce)
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as client:
        client.settimeout(0.25)
        errors["external_route"] = denied(lambda: client.connect(("192.0.2.1", 9)), (errno.ENETUNREACH, errno.EHOSTUNREACH))
    errors["cgroup_write"] = denied(lambda: Path("/sys/fs/cgroup/cpu.max").write_text(Path("/sys/fs/cgroup/cpu.max").read_text()), allowed)
    emit(role, errors=errors, topology=network, candidate=True, canary_reads=True, tcp=True, unix=True)

def boundary(manifest):
    deadline = time.monotonic() + 8
    nonce = manifest["run_id"].encode()
    Path("/workspace/escape").symlink_to("/protected/sentinel.txt")
    Path("/workspace/host-escape").symlink_to(manifest["host_engine"] + "/engine.db")
    mounts = Path("/proc/self/mountinfo").read_text()
    require(not any(line.split()[4] == "/dev/shm" for line in mounts.splitlines()), "shared memory mount exposed")
    for path in ("/var/run/docker.sock", "/run/docker.sock"):
        require(not os.path.lexists(path), "Docker socket exposed")
    unix_path = "/tmp/probe.sock"
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as tcp, socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as unix:
        tcp.bind(("127.0.0.1", 0))
        unix.bind(unix_path)
        tcp.listen(2)
        unix.listen(2)
        tcp.settimeout(1)
        unix.settimeout(1)
        listener = os.fork()
        if listener == 0:
            try:
                signal.alarm(6)
                for server in (tcp, unix, tcp, unix):
                    client, address = server.accept()
                    with client:
                        client.settimeout(0.25)
                        require(client.recv(64) == nonce, "listener challenge failed")
                        client.sendall(nonce)
                os._exit(0)
            except BaseException:
                os._exit(1)
        child = None
        try:
            filesystem_actor(manifest, "worker", tcp.getsockname()[1], unix_path)
            read_fd, write_fd = os.pipe()
            gate_read, gate_write = os.pipe()
            try:
                child = os.fork()
                if child == 0:
                    try:
                        signal.alarm(5)
                        os.close(read_fd)
                        os.close(gate_write)
                        require(os.read(gate_read, 2) == b"go", "child gate failed")
                        os.close(gate_read)
                        filesystem_actor(manifest, "child", tcp.getsockname()[1], unix_path)
                        os.write(write_fd, nonce)
                        os._exit(0)
                    except BaseException:
                        os._exit(1)
                os.close(write_fd)
                write_fd = None
                os.close(gate_read)
                gate_read = None
                os.kill(child, signal.SIGSTOP)
                waited, stopped = os.waitpid(child, os.WUNTRACED)
                require(waited == child and os.WIFSTOPPED(stopped), "child stop control failed")
                visible = sorted(int(path.name) for path in Path("/proc").iterdir() if path.name.isdecimal())
                allowed = {os.getpid(), listener, child}
                require(os.getpid() == 1 and set(visible) <= allowed, "unexpected process namespace")
                os.kill(child, signal.SIGCONT)
                os.write(gate_write, b"go")
                require(os.read(read_fd, 64) == nonce, "pipe nonce control failed")
                child_status = reap(child, deadline)
                child = None
                require(child_status == 0, "child positive control failed")
            finally:
                os.close(read_fd)
                if write_fd is not None:
                    os.close(write_fd)
                if gate_read is not None:
                    os.close(gate_read)
                os.close(gate_write)
            listener_status = reap(listener, deadline)
            listener = None
            require(listener_status == 0, "listener positive control failed")
            emit("boundary", visible=visible, recorded_descendants=sorted(allowed - {1}), pipe=True, stopped_continued_reaped=True, no_shm_mount=True)
        finally:
            for pid in (child, listener):
                if pid is not None:
                    os.kill(pid, signal.SIGKILL)
                    os.waitpid(pid, 0)
    require(time.monotonic() <= deadline, "combined boundary deadline exceeded")

def memory_case():
    before = snapshot()
    require(touch(8 * 1024 * 1024) == 8 * 1024 * 1024, "small allocation failed")
    control = snapshot()
    emit("memory-control", touched_bytes=8 * 1024 * 1024, before=before, after=control)
    emit("memory-attempt", requested_bytes=96 * 1024 * 1024)
    child = os.fork()
    if child == 0:
        signal.alarm(5)
        touch(96 * 1024 * 1024)
        os._exit(0)
    status = reap(child, time.monotonic() + 6)
    after = snapshot()
    require(os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGKILL, "memory child was not killed")
    require(counters(after["memory.events"])["oom_kill"] > counters(control["memory.events"])["oom_kill"], "OOM counter did not increase")
    emit("memory", child=child, child_status=status, before=control, after=after)

def pids_case():
    before = snapshot()
    controls = []
    for attempt in range(2):
        child = os.fork()
        if child == 0:
            os._exit(0)
        controls.append(child)
    for child in controls:
        require(reap(child, time.monotonic() + 1) == 0, "fork control failed")
    control = snapshot()
    emit("pids-control", children=controls, before=before, after=control)
    children, errors = [], []
    read_fd, write_fd = os.pipe()
    try:
        for attempt in range(18):
            try:
                child = os.fork()
            except OSError as error:
                errors.append(error.errno)
                continue
            if child == 0:
                signal.alarm(5)
                os.close(write_fd)
                os.read(read_fd, 1)
                os._exit(0)
            children.append(child)
        held = snapshot()
    finally:
        os.close(write_fd)
        os.close(read_fd)
        for child in children:
            require(reap(child, time.monotonic() + 1) == 0, "held PID child failed to reap")
    require(errors and all(error == errno.EAGAIN for error in errors), "expected EAGAIN not observed")
    require(counters(held["pids.events"])["max"] > counters(control["pids.events"])["max"], "PID events did not increase")
    invariant(len(children) + len(errors) == 18, "fork attempts lost")
    emit("pids", children=children, errnos=errors, before=control, held=held, after=snapshot(), all_reaped=True)

def cpu_case():
    before = snapshot()
    time.sleep(0.2)
    idle = snapshot()
    started = time.monotonic()
    progress = 0
    while time.monotonic() - started < 2:
        progress += 1
    after = snapshot()
    previous, current = counters(idle["cpu.stat"]), counters(after["cpu.stat"])
    delta = {key: current[key] - previous[key] for key in ("usage_usec", "nr_throttled", "throttled_usec")}
    require(progress > 0 and all(value > 0 for value in delta.values()), "CPU progress or throttling absent")
    denial = denied(lambda: Path("/sys/fs/cgroup/cpu.max").write_text(after["cpu.max"]), (errno.EROFS, errno.EACCES, errno.EPERM))
    emit("cpu", idle_before=before, idle_after=idle, after=after, delta=delta, iterations=progress, elapsed=time.monotonic() - started, cgroup_errno=denial)

def main():
    require(__debug__, "assertions disabled")
    require(len(sys.argv) == 2 and sys.argv[1] in ("boundary", "memory", "pids", "cpu", "deadline"), "invalid case")
    manifest = json.loads(Path("/probe/manifest.json").read_text())
    require(manifest["case"] == sys.argv[1], "manifest case mismatch")
    state = snapshot()
    require(state["memory.max"] == "67108864" and state["memory.swap.max"] == "0" and state["pids.max"] == "16", "cgroup limits mismatch")
    quota, period = state["cpu.max"].split()
    require(quota != "max" and 0 < int(quota) * 2 <= int(period), "CPU rate mismatch")
    status = Path("/proc/self/status").read_text()
    cgroup_mounts = [line for line in Path("/proc/self/mountinfo").read_text().splitlines() if " - cgroup2 " in line]
    require(len(cgroup_mounts) == 1 and "ro" in cgroup_mounts[0].split()[5].split(","), "writable or missing cgroup hierarchy")
    fields = dict(line.split(":", 1) for line in status.splitlines() if ":" in line)
    require(all(int(fields[key].strip(), 16) == 0 for key in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb")), "effective capabilities present")
    require(fields["NoNewPrivs"].strip() == "1", "no-new-privileges missing")
    require(os.getuid() == manifest["uid"] and os.getgid() == manifest["gid"], "UID/GID mismatch")
    emit("before", cgroup=state, status=status, cgroup_mounts=cgroup_mounts, uid=os.getuid(), gid=os.getgid(), run_id=manifest["run_id"])
    if sys.argv[1] == "boundary":
        signal.alarm(8)
        boundary(manifest)
    elif sys.argv[1] == "memory":
        signal.alarm(9)
        memory_case()
    elif sys.argv[1] == "pids":
        signal.alarm(9)
        pids_case()
    elif sys.argv[1] == "cpu":
        signal.alarm(6)
        cpu_case()
    else:
        child = os.fork()
        if child == 0:
            time.sleep(14)
            os._exit(0)
        emit("ready", nonce=manifest["run_id"], child=child)
        time.sleep(14.5)
        reap(child, time.monotonic() + 0.1)
    emit("after", cgroup=snapshot())

if __name__ == "__main__":
    main()
'''


def invariant(condition, message):
    assert condition, message


def require(condition, message):
    if not condition:
        raise ValueError(message)


class Budget:
    def __init__(self):
        self.started = time.monotonic()

    def command_deadline(self, upper=None):
        deadline = min(self.started + 1200, time.monotonic() + 3, upper or float("inf"))
        require(deadline > time.monotonic(), "allocation or operation deadline exhausted")
        return deadline

    def admit_case(self):
        elapsed = time.monotonic() - self.started
        require(elapsed < 780 and elapsed + 15 + 180 < 1200, "case allocation exhausted; cleanup reserved")
        invariant(1200 - elapsed > 195, "cleanup reserve lost")


def capture(command, deadline, tick=None, process=None):
    require(command and os.path.isabs(command[0]) and deadline > time.monotonic(), "invalid bounded command")
    started = time.monotonic()
    process = process or subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, env=ENVIRONMENT)
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    counts = {name: 0 for name in streams}
    timed_out, overflow = False, False
    try:
        with selectors.DefaultSelector() as selector:
            for name in streams:
                stream = getattr(process, name)
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, name)
            while selector.get_map() or process.poll() is None:
                if tick:
                    tick()
                if time.monotonic() >= deadline:
                    timed_out = True
                    break
                for key, events in selector.select(min(0.02, max(0, deadline - time.monotonic()))):
                    value = os.read(key.fd, 65536)
                    if not value:
                        selector.unregister(key.fileobj)
                        continue
                    name = key.data
                    counts[name] += len(value)
                    streams[name].extend(value[:max(0, OUTPUT_LIMIT - len(streams[name]))])
                    overflow |= counts[name] > OUTPUT_LIMIT
                if overflow:
                    break
    finally:
        if process.poll() is None or timed_out or overflow:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.wait(timeout=1)
        for name in streams:
            getattr(process, name).close()
    invariant(process.returncode is not None, "subprocess not reaped")
    invariant(all(len(value) <= OUTPUT_LIMIT for value in streams.values()), "stream bound lost")
    return {"command": command, "pid": process.pid, "returncode": process.returncode, "elapsed": time.monotonic() - started,
            "timed_out": timed_out, "overflow": overflow, "counts": counts,
            **{name: value.decode("utf-8", errors="replace") for name, value in streams.items()}}


def payload_hash():
    return hashlib.sha256(PAYLOAD.encode()).hexdigest()


def file_hash(path):
    require(path.is_file() and not path.is_symlink(), "hash input must be a regular file")
    require(path.stat().st_size <= 64 * 1024 * 1024, "hash input too large")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65536), b""):
            digest.update(block)
    return digest.hexdigest()


def fixture_size(root, validate=True):
    total, entries = 0, 0
    for directory, subdirs, names in os.walk(root, followlinks=False):
        entries += len(subdirs) + len(names)
        require(entries <= 256, "fixture entry bound exceeded")
        for name in names + [name for name in subdirs if (Path(directory) / name).is_symlink()]:
            path = Path(directory) / name
            info = path.lstat()
            require(stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode) or stat.S_ISSOCK(info.st_mode), "unexpected fixture entry")
            total += info.st_size
            if validate and "candidate" in path.relative_to(root).parts:
                require(info.st_size <= 65536, "candidate file overflow")
        relative = Path(directory).relative_to(root).parts
        if validate and len(relative) == 2 and relative[0] == "candidate":
            require(not subdirs and len(names) <= 8, "candidate shape overflow")
    require(total <= (TOTAL_LIMIT - 262144 if validate else TOTAL_LIMIT), "fixture/evidence allocation exhausted")
    return total


def save(root, name, value):
    require(re.fullmatch(r"[a-zA-Z0-9_.-]+", name), "invalid evidence name")
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    require(len(data) <= 131072, "evidence record overflow")
    size = fixture_size(root, validate=False)
    target = root / "evidence" / name
    require(not target.is_symlink(), "evidence path substituted")
    previous = target.stat().st_size if target.exists() else 0
    require(size - previous + len(data) <= TOTAL_LIMIT, "evidence total overflow")
    with target.open("wb") as output:
        output.write(data)


def validate_config(path):
    require(path.is_dir() and path.resolve() == path and not path.is_symlink(), "configuration path unavailable")
    names = []
    for index, item in enumerate(path.iterdir()):
        require(index < 2, "Docker configuration must be empty except config.json")
        names.append(item.name)
    names.sort()
    require(names == ["config.json"], "Docker configuration must be empty except config.json")
    config = path / "config.json"
    require(not config.is_symlink() and config.stat().st_size <= 1024, "invalid empty Docker configuration")
    require(json.loads(config.read_text()) in ({}, {"auths": {}}), "Docker configuration must be empty")


def create_arguments(root, run, case, uid, gid):
    require(re.fullmatch(r"[0-9a-f]{32}", run) and case in CASES, "invalid run or case")
    require(root == Path("/private/tmp/ca-q2-" + run), "noncanonical fixture root")
    require(uid > 0 and gid > 0, "nonzero host UID/GID required")
    args = ["create", "--name", "ca-q2-" + run + "-" + case,
            "--label", LABEL + "=" + run, "--label", LABEL + ".case=" + case,
            "--cidfile", str(root / "evidence" / (case + ".cid")), "--pull=never", "--platform=linux/arm64",
            "--memory=64m", "--memory-swap=64m", "--cpus=0.5", "--pids-limit=16", "--network=none", "--ipc=none",
            "--cgroupns=private", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges=true",
            "--user", str(uid) + ":" + str(gid), "--ulimit", "core=0:0", "--ulimit", "fsize=65536:65536", "--ulimit", "nofile=64:64",
            "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=8m,mode=1777"]
    for source, destination, readonly in ((root / "candidate" / case, "/workspace", False), (root / "protected", "/protected", True), (root / "payload", "/probe", True)):
        args += ["--mount", "type=bind,src=" + str(source) + ",dst=" + destination + (",readonly" if readonly else "")]
    return args + ["--workdir=/workspace", "--log-driver=none", "--entrypoint=/usr/local/bin/python3", IMAGE, "-I", "-B", "-u", "/probe/probe.py", case]


def validate_identity(data, run, case, image):
    require(isinstance(data, dict), "invalid inspected object")
    require(re.fullmatch(r"[0-9a-f]{64}", data.get("Id", "")), "invalid exact CID")
    require(data.get("Name") == "/ca-q2-" + run + "-" + case and data.get("Image") == image, "container identity mismatch; mutation forbidden")
    config = data.get("Config", {})
    require(config.get("Image") == IMAGE, "image reference mismatch")
    labels = config.get("Labels", {})
    require(labels.get(LABEL) == run and labels.get(LABEL + ".case") == case, "run/case identity mismatch")
    return data["Id"]


def validate_profile(data, root, run, case, image, uid, gid):
    validate_identity(data, run, case, image)
    config, host, state = data["Config"], data["HostConfig"], data["State"]
    require(state["Status"] == "created" and not state["Running"], "container was already started")
    require(config.get("User") == str(uid) + ":" + str(gid), "host UID/GID mismatch")
    require(config.get("Entrypoint") == ["/usr/local/bin/python3"] and config.get("Cmd") == ["-I", "-B", "-u", "/probe/probe.py", case], "payload argv mismatch")
    require(config.get("WorkingDir") == "/workspace" and not config.get("Volumes"), "unexpected image authority")
    expected = {"Memory": MEMORY, "MemorySwap": MEMORY, "NanoCpus": 500000000, "PidsLimit": 16,
                "NetworkMode": "none", "IpcMode": "none", "PidMode": "", "CgroupnsMode": "private", "ReadonlyRootfs": True,
                "Privileged": False, "AutoRemove": False, "CapDrop": ["ALL"]}
    for key, value in expected.items():
        require(host.get(key) == value, "profile mismatch: " + key)
    require(host.get("SecurityOpt") in (["no-new-privileges:true"], ["no-new-privileges=true"]), "no-new-privileges mismatch")
    require(host.get("RestartPolicy", {}).get("Name") == "no" and host.get("LogConfig", {}).get("Type") == "none", "restart/logging mismatch")
    for key in ("CapAdd", "Binds", "Devices", "DeviceRequests", "VolumesFrom", "PortBindings", "PublishAllPorts", "Init", "Links", "ExtraHosts", "GroupAdd", "CgroupParent", "UsernsMode"):
        require(not host.get(key), "forbidden profile field: " + key)
    require(host.get("Runtime") in (None, "", "runc"), "custom runtime forbidden")
    require(host.get("Tmpfs") == {"/tmp": "rw,noexec,nosuid,nodev,size=8m,mode=1777"}, "tmpfs mismatch")
    limits = {item["Name"]: (item["Soft"], item["Hard"]) for item in host.get("Ulimits", [])}
    require(limits == {"core": (0, 0), "fsize": (65536, 65536), "nofile": (64, 64)}, "ulimits mismatch")
    mounts = data.get("Mounts", [])
    tmpfs = [item for item in mounts if item.get("Type") == "tmpfs"]
    require(len(mounts) == 3 + len(tmpfs) and len(tmpfs) <= 1 and all(item.get("Destination") == "/tmp" and item.get("RW") for item in tmpfs), "mount count mismatch")
    actual = {(item.get("Source"), item.get("Destination"), item.get("RW")) for item in mounts if item.get("Type") == "bind" and item.get("Propagation") == "rprivate"}
    expected_mounts = {(str(root / "candidate" / case), "/workspace", True), (str(root / "protected"), "/protected", False), (str(root / "payload"), "/probe", False)}
    require(actual == expected_mounts, "mount whitelist mismatch")
    require(not data.get("NetworkSettings", {}).get("Ports"), "published ports forbidden")


class Controller:
    def __init__(self, root, run, budget):
        self.root, self.run, self.budget = root, run, budget
        self.sequence = 0
        self.image = None
        self.uid, self.gid = os.getuid(), os.getgid()

    def docker(self, arguments, required=True, upper=None):
        self.sequence += 1
        require(self.sequence <= 160, "Docker command count exhausted")
        result = capture([DOCKER, "--config", str(CONFIG), "--host", ENDPOINT, *arguments], self.budget.command_deadline(upper))
        save(self.root, "command-" + str(self.sequence).zfill(3) + ".json", result)
        require(not result["timed_out"] and not result["overflow"], "Docker command deadline/output unavailable")
        if required:
            require(result["returncode"] == 0, "Docker operation failed: " + arguments[0])
        return result

    def inspect(self, identity, case, upper=None):
        require(identity == "ca-q2-" + self.run + "-" + case or re.fullmatch(r"[0-9a-f]{64}", identity), "invalid inspection target")
        result = self.docker(["inspect", "--type", "container", identity], upper=upper)
        values = json.loads(result["stdout"])
        require(isinstance(values, list) and len(values) == 1, "ambiguous inspection")
        validate_identity(values[0], self.run, case, self.image)
        if re.fullmatch(r"[0-9a-f]{64}", identity):
            require(values[0]["Id"] == identity, "exact CID changed")
        return values[0]

    def stop(self, cid, case, upper=None):
        data = self.inspect(cid, case, upper)
        if data["State"]["Running"]:
            self.docker(["kill", "--signal", "KILL", cid], upper=upper)
            data = self.inspect(cid, case, upper)
        require(not data["State"]["Running"], "container exit unconfirmed")
        return data

    def cleanup(self, cid, case):
        data = self.stop(cid, case)
        save(self.root, case + "-stopped.json", data)
        self.docker(["rm", cid])
        result = self.docker(["inspect", "--type", "container", cid], required=False)
        require(result["returncode"] != 0 and result["stdout"].strip() in ("", "[]") and result["stderr"].strip() in (
            "Error: No such container: " + cid, "Error response from daemon: No such container: " + cid,
            "Error: No such object: " + cid, "Error response from daemon: No such object: " + cid), "exact removal absence unconfirmed")
        cidfile = self.root / "evidence" / (case + ".cid")
        if cidfile.exists():
            require(not cidfile.is_symlink() and cidfile.read_text().strip() == cid, "cidfile identity mismatch")
            cidfile.unlink()
        return {"confirmed": True, "cid": cid, "state": data["State"], "absent": True}


def frame(value):
    data = json.dumps(value).encode()
    require(0 < len(data) <= 65536, "protocol frame overflow")
    return len(data).to_bytes(4, "big") + data


def receive_exact(client, size, deadline=None):
    require(0 < size <= 65536, "invalid frame length")
    deadline = deadline or time.monotonic() + 1
    data = bytearray()
    while len(data) < size:
        remaining = deadline - time.monotonic()
        require(remaining > 0, "frame receive deadline exceeded")
        client.settimeout(min(1, remaining))
        block = client.recv(size - len(data))
        require(bool(block), "truncated protocol frame")
        data.extend(block)
    invariant(len(data) == size, "frame shape lost")
    return bytes(data)


def engine_request(root, request_id, revision, command):
    deadline = time.monotonic() + 1
    request = {"protocol_version": 1, "client_id": "followup-host", "request_id": request_id, "expected_revision": revision, "command": command}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(1)
        client.connect(str(root / "host-engine" / "engine.sock"))
        client.sendall(frame(request))
        size = int.from_bytes(receive_exact(client, 4, deadline), "big")
        reply = json.loads(receive_exact(client, size, deadline))
    require(reply.get("request_id") == request_id, "engine reply identity mismatch")
    save(root, request_id + ".json", {"request": request, "reply": reply})
    return reply


def host_state(root, request_id, expected_operations):
    reply = engine_request(root, request_id, "1", {"kind": "Snapshot"})
    expected_projects = [{"project_id": "synthetic-project", "spec": {"name": "Synthetic containment fixture", "repositories": []}}]
    require(reply["revision"] == "1" and reply["outcome"] == {"kind": "Snapshot", "projects": expected_projects, "tasks": []}, "host engine state changed")
    with sqlite3.connect((root / "host-engine" / "engine.db").as_uri() + "?mode=ro", uri=True, timeout=0.25) as database:
        database.set_progress_handler(lambda: 1, 100000)
        projects = database.execute("SELECT id FROM projects ORDER BY id LIMIT 3").fetchall()
        operations = database.execute("SELECT client,id FROM operations ORDER BY id LIMIT 32").fetchall()
        events = database.execute("SELECT body FROM events ORDER BY cursor LIMIT 3").fetchall()
        revision = database.execute("SELECT revision FROM state WHERE id=1").fetchall()
    require(projects == [("synthetic-project",)] and revision == [(1,)], "persisted project/revision mismatch")
    require(operations == sorted(("followup-host", identity) for identity in expected_operations + [request_id]), "unexpected persisted operations")
    require(len(events) == 1 and json.loads(events[0][0])["request_id"] == "host-create", "unexpected mutation event")
    save(root, request_id + "-sqlite.json", {"projects": projects, "operations": operations, "events": events, "revision": revision})


def canaries(root):
    result = {}
    for name in ("sentinel.txt", "config.json"):
        path = root / "protected" / name
        info = path.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1 and info.st_size <= 4096, "canary shape mismatch")
        result[name] = {"sha256": file_hash(path), "mode": info.st_mode, "uid": info.st_uid, "gid": info.st_gid, "size": info.st_size, "inode": info.st_ino, "mtime_ns": info.st_mtime_ns}
    return result


def fixture_identity(info, kind):
    require(kind in ("directory", "socket"), "unexpected fixture identity kind")
    predicate = stat.S_ISDIR if kind == "directory" else stat.S_ISSOCK
    require(predicate(info.st_mode) and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0, "fixture kind, owner or privacy mismatch")
    result = {"device": info.st_dev, "inode": info.st_ino, "uid": info.st_uid, "gid": info.st_gid, "mode": info.st_mode}
    if kind == "socket":
        require(info.st_nlink == 1, "socket link identity mismatch")
        result.update(ctime_ns=info.st_ctime_ns, links=info.st_nlink)
    return result


def engine_socket_receipt(root, process):
    require(isinstance(process, subprocess.Popen) and process.poll() is None, "live owned engine handle required")
    require(root.is_absolute() and root.resolve() == root, "socket fixture root is not canonical")
    parent = root / "host-engine"
    require(parent.resolve() == parent, "socket fixture parent is not canonical")
    path = parent / "engine.sock"
    return {"engine_pid": process.pid, "path": str(path), "root": fixture_identity(root.lstat(), "directory"),
            "parent": fixture_identity(parent.lstat(), "directory"), "socket": fixture_identity(path.lstat(), "socket")}


def cleanup_engine_socket(root, process, receipt):
    require(isinstance(process, subprocess.Popen) and process.returncode is not None, "engine must exit and be reaped before socket cleanup")
    require(process.wait(timeout=0) == process.returncode and receipt.get("engine_pid") == process.pid, "owned engine handle mismatch")
    require(root.is_absolute() and root.resolve() == root and receipt.get("path") == str(root / "host-engine" / "engine.sock"), "socket fixture path mismatch")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    root_fd = os.open(root, flags)
    removed = False
    try:
        require(fixture_identity(os.fstat(root_fd), "directory") == receipt["root"] and fixture_identity(root.lstat(), "directory") == receipt["root"], "socket fixture root replaced")
        parent_fd = os.open("host-engine", flags, dir_fd=root_fd)
        try:
            require(fixture_identity(os.fstat(parent_fd), "directory") == receipt["parent"], "socket fixture parent replaced")
            require(fixture_identity(os.stat("host-engine", dir_fd=root_fd, follow_symlinks=False), "directory") == receipt["parent"], "socket fixture parent path replaced")
            try:
                info = os.stat("engine.sock", dir_fd=parent_fd, follow_symlinks=False)
            except FileNotFoundError:
                info = None
            if info is not None:
                require(fixture_identity(info, "socket") == receipt["socket"], "owned engine socket replaced")
                require(fixture_identity(root.lstat(), "directory") == receipt["root"] and fixture_identity(os.stat("host-engine", dir_fd=root_fd, follow_symlinks=False), "directory") == receipt["parent"], "socket fixture path changed before removal")
                os.unlink("engine.sock", dir_fd=parent_fd)
                removed = True
            try:
                os.stat("engine.sock", dir_fd=parent_fd, follow_symlinks=False)
            except FileNotFoundError:
                absent = True
            else:
                absent = False
            require(absent, "unexpected socket entry survived cleanup")
            require(fixture_identity(root.lstat(), "directory") == receipt["root"] and fixture_identity(os.stat("host-engine", dir_fd=root_fd, follow_symlinks=False), "directory") == receipt["parent"], "socket fixture path changed after removal")
            require(not os.path.lexists(root / "host-engine" / "engine.sock"), "socket pathname is not absent")
            invariant(absent and process.returncode is not None, "socket cleanup postcondition lost")
            return {"engine_pid": process.pid, "removed": removed, "socket_absent": absent, "original_socket": receipt["socket"]}
        finally:
            os.close(parent_fd)
    finally:
        os.close(root_fd)


def setup_fixture(root, run):
    require(re.fullmatch(r"[0-9a-f]{32}", run), "invalid fixture run ID")
    require(root == Path("/private/tmp/ca-q2-" + run) and root.parent.resolve() == root.parent, "fixture canonical path mismatch")
    root.mkdir(mode=0o700, exist_ok=False)
    for name in ("candidate", "protected", "payload", "host-engine", "evidence"):
        (root / name).mkdir(mode=0o700)
    for case in CASES:
        (root / "candidate" / case).mkdir(mode=0o700)
    (root / "protected" / "sentinel.txt").write_text("synthetic-protected:" + run + "\n")
    (root / "protected" / "config.json").write_text(json.dumps({"run_id": run, "policy": "fixture-only"}))
    (root / "payload" / "probe.py").write_text(PAYLOAD)
    for name in ("global.toml", "project.toml"):
        (root / "host-engine" / name).write_text("")
    for path in [root, *(root / name for name in ("candidate", "protected", "payload", "host-engine", "evidence")), *(root / "candidate" / case for case in CASES)]:
        info = path.lstat()
        require(path.resolve() == path and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0, "fixture ownership/privacy mismatch")
    fixture_size(root)


def local_preflight(engine, expected_hash, binding):
    require(sys.platform == "darwin" and os.uname().machine == "arm64", "native macOS arm64 prerequisite unavailable")
    require(os.getuid() > 0 and os.getgid() > 0, "nonzero host UID/GID required")
    validate_config(CONFIG)
    require(engine == ROOT / "target/debug/crazyagent" and engine.resolve() == engine and os.access(engine, os.X_OK), "existing canonical worktree engine binary unavailable")
    require(re.fullmatch(r"[0-9a-f]{64}", expected_hash) and file_hash(engine) == expected_hash, "engine binary evidence binding mismatch")
    require(binding.is_file() and binding.stat().st_size <= 16384, "existing build evidence unavailable")
    bound = json.loads(binding.read_text())
    require(bound.get("engine") == str(engine) and bound.get("engine_sha256") == expected_hash and bound.get("command") == "make build" and bound.get("exit_code") == 0, "build evidence does not bind this binary")
    log = Path(bound.get("build_log", ""))
    require(log.is_absolute() and file_hash(log) == bound.get("build_log_sha256"), "build evidence log hash mismatch")
    require(re.fullmatch(r"[0-9a-f]{40}", bound.get("source_revision", "")), "source revision binding missing")
    compile(PAYLOAD, "probe.py", "exec")
    invariant(len(PAYLOAD.encode()) <= 60000, "payload exceeds allocation")
    evidence = {"engine": str(engine), "engine_sha256": expected_hash, "binding": str(binding), "binding_sha256": file_hash(binding),
                "source_sha256": file_hash(Path(__file__)), "resource_helper_sha256": file_hash(Path(__file__).with_name("container_resources.py")), "payload_sha256": payload_hash(),
                "config_sha256": file_hash(CONFIG / "config.json"), "endpoint": ENDPOINT, "uid": os.getuid(), "gid": os.getgid(), "build": bound}
    return evidence


def preflight(controller, engine, expected_hash, binding):
    evidence = local_preflight(engine, expected_hash, binding)
    evidence["docker_version"] = controller.docker(["version", "--format", "{{json .}}"])["stdout"]
    image = json.loads(controller.docker(["image", "inspect", IMAGE])["stdout"])
    require(isinstance(image, list) and len(image) == 1, "image inspection ambiguous")
    image = image[0]
    require(IMAGE in image.get("RepoDigests", []) and image.get("Architecture") == "arm64" and image.get("Os") == "linux", "pinned local image unavailable")
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", image.get("Id", "")), "resolved image ID unavailable")
    controller.image = image["Id"]
    evidence["image"] = {key: image.get(key) for key in ("Id", "RepoDigests", "Architecture", "Os")}
    save(controller.root, "provenance.json", evidence)
    return evidence


def evaluate(case, result, receipt, run, uid, gid):
    require(receipt["confirmed"] and receipt["absent"], "evaluation before cleanup confirmation")
    require(not result["overflow"] and not result["timed_out"], "incomplete attached stream")
    if case == "boundary":
        require(result["returncode"] == 0, "boundary payload failed: " + result["stderr"])
    records = [json.loads(line) for line in result["stdout"].splitlines()]
    require(len(records) <= 12, "unexpected payload record count")
    phases = {record["phase"]: record for record in records}
    require(len(phases) == len(records) and "before" in phases, "missing/duplicate pre-control")
    before = phases["before"]
    require(before["run_id"] == run and before["uid"] == uid and before["gid"] == gid, "executed identity mismatch")
    state = receipt["state"]
    elapsed = docker_timestamp_ns(state["FinishedAt"]) - docker_timestamp_ns(state["StartedAt"])
    require(0 <= elapsed <= 15000000000, "actual lifetime exceeds fifteen seconds")
    if case == "deadline":
        require(phases.get("ready", {}).get("nonce") == run and "after" not in phases, "deadline readiness/completion mismatch")
        require(state["ExitCode"] == 137 and not state["OOMKilled"], "deadline kill not observed")
    elif case == "memory" and state["OOMKilled"]:
        require("memory-control" in phases and phases.get("memory-attempt", {}).get("requested_bytes") == 96 * 1024 * 1024, "memory positive control missing before OOM")
    else:
        require(state["ExitCode"] == 0 and result["returncode"] == 0 and "after" in phases, "case positive/workload failed")
        require(case in phases, "case evidence missing")
        if case == "boundary":
            require("worker" in phases and "child" in phases, "worker/child controls incomplete")
    return {"records": records, "actual_lifetime_ns": elapsed, "status": "observed"}


def finalize_case(root, case, record, baseline, run, uid, gid):
    if "error" in record:
        record["status"] = "failed"
        return record
    try:
        evaluated = evaluate(case, record["execution"], record["cleanup"], run, uid, gid)
        require(canaries(root) == baseline, "protected canary changed")
        if case == "boundary":
            for role in ("worker", "child"):
                require((root / "candidate" / case / (role + ".txt")).read_text() == run + ":" + role + "\n", "candidate positive control failed")
        record.update(evaluated)
    except (OSError, ValueError, KeyError) as error:
        record["status"], record["error"] = "failed", str(error)
    return record


def execute_case(controller, case, baseline):
    controller.budget.admit_case()
    root, run = controller.root, controller.run
    manifest = {"run_id": run, "case": case, "canaries": baseline, "host_engine": str(root / "host-engine"), "uid": controller.uid, "gid": controller.gid}
    manifest_path = root / "payload" / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, sort_keys=True))
    require((root / "payload" / "probe.py").stat().st_size + manifest_path.stat().st_size <= 65536, "payload fixture overflow")
    arguments = create_arguments(root, run, case, controller.uid, controller.gid)
    record = {"case": case, "status": "unavailable", "chosen_name": "ca-q2-" + run + "-" + case, "cleanup": {"confirmed": False},
              "manifest_sha256": file_hash(manifest_path), "create_argv_sha256": hashlib.sha256(json.dumps(arguments).encode()).hexdigest()}
    cid, attach, timer, collector = None, None, None, None
    watchdog = {"kill_ceiling_seconds": 12, "kill_target_seconds": 11.9}
    cancel_watchdog = threading.Event()
    started = None
    try:
        created = controller.docker(arguments)
        cid = created["stdout"].strip()
        require(re.fullmatch(r"[0-9a-f]{64}", cid), "create returned invalid CID")
        require((root / "evidence" / (case + ".cid")).read_text().strip() == cid, "create/cidfile mismatch")
        data = controller.inspect(cid, case)
        validate_profile(data, root, run, case, controller.image, controller.uid, controller.gid)
        save(root, case + "-profile.json", data)
        command = [DOCKER, "--config", str(CONFIG), "--host", ENDPOINT, "start", "--attach", cid]
        controller.budget.admit_case()
        started = time.monotonic()

        def deadline_watchdog():
            try:
                current = controller.inspect(cid, case, started + 11)
                if cancel_watchdog.wait(max(0, started + 11.9 - time.monotonic())):
                    return
                if current["State"]["Running"]:
                    watchdog["requested_at"] = time.monotonic() - started
                    controller.docker(["kill", "--signal", "KILL", cid], upper=started + 14)
                current = controller.inspect(cid, case, started + 15)
                require(not current["State"]["Running"], "watchdog exit unconfirmed")
                watchdog["verified_at"] = time.monotonic() - started
            except BaseException as error:
                watchdog["error"] = str(error)

        timer = threading.Timer(10, deadline_watchdog)
        timer.start()
        attach = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, env=ENVIRONMENT)

        def collect_attached():
            try:
                record["execution"] = capture(command, min(started + 15, controller.budget.started + 1200), tick=lambda: fixture_size(root), process=attach)
            except BaseException as error:
                record["stream_error"] = str(error)

        collector = threading.Thread(target=collect_attached)
        collector.start()
        for attempt in range(60):
            data = controller.inspect(cid, case, started + 3)
            if data["State"]["Status"] != "created":
                break
            require(time.monotonic() < started + 3, "start request not confirmed within three seconds")
            time.sleep(0.05)
        require(data["State"]["Status"] in ("running", "exited"), "container start unconfirmed")
        record["start_confirmed_at"] = time.monotonic() - started
        require(record["start_confirmed_at"] <= 3, "start request exceeded three seconds")
        collector.join(timeout=max(0.01, started + 15 - time.monotonic()))
        require(not collector.is_alive() and not record.get("stream_error"), "attached stream unavailable")
        cancel_watchdog.set()
        timer.cancel()
        timer.join(timeout=max(0.01, started + 15 - time.monotonic()))
        require(not timer.is_alive(), "watchdog termination unconfirmed")
        record["observed_exit_at"] = time.monotonic() - started
        require(record["observed_exit_at"] <= 15, "container exit not confirmed within lifetime")
        require(not watchdog.get("error"), "watchdog failed: " + str(watchdog.get("error")))
        if case == "deadline":
            require(11.9 <= watchdog.get("requested_at", 0) <= 12 and watchdog.get("verified_at", 16) <= 15, "deadline watchdog proof missing")
    except (OSError, ValueError, RuntimeError, KeyboardInterrupt, subprocess.SubprocessError) as error:
        record["error"] = str(error) or "interrupted"
    finally:
        if timer:
            cancel_watchdog.set()
            timer.cancel()
            timer.join(timeout=3.1)
        if attach is not None:
            if attach.poll() is None:
                try:
                    os.killpg(attach.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if collector:
                collector.join(timeout=1)
            attach.wait(timeout=1)
            if not collector:
                attach.stdout.close()
                attach.stderr.close()
        record["watchdog"] = watchdog
        try:
            if timer and timer.is_alive():
                raise ValueError("watchdog still active; concurrent cleanup forbidden")
            if cid is None or not re.fullmatch(r"[0-9a-f]{64}", cid):
                resolved = controller.inspect("ca-q2-" + run + "-" + case, case)
                cid = resolved["Id"]
            record["cid"] = cid
            record["cleanup"] = controller.cleanup(cid, case)
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
            record["cleanup"]["error"] = str(error)
        record["started_monotonic"] = started
        save(root, case + "-result.json", record)
    finalize_case(root, case, record, baseline, run, controller.uid, controller.gid)
    save(root, case + "-result.json", record)
    return record


def interrupted(signum, current_frame):
    raise KeyboardInterrupt("controller signal " + str(signum))


def selected_cases(diagnostic):
    invariant(type(diagnostic) is bool, "diagnostic selection must be boolean")
    return ("boundary",) if diagnostic else CASES


def claim_attempt(directory, diagnostic, provenance):
    cases = selected_cases(diagnostic)
    name = "linux-topology-diagnostic-attempt.json" if diagnostic else "linux-containment-native-attempt.json"
    record = {**provenance, "mode": "boundary-only-diagnostic" if diagnostic else "five-case-qualification",
              "cases": list(cases), "payload_sha256": payload_hash()}
    with (directory / name).open("x") as attempt:
        json.dump(record, attempt, sort_keys=True)
    return record


def completion_status(diagnostic, results):
    if tuple(item["case"] for item in results) != selected_cases(diagnostic):
        return "failed"
    if not all(item["status"] == "observed" and item["cleanup"]["confirmed"] and item["cleanup"]["absent"] for item in results):
        return "failed"
    return "diagnostic-observed" if diagnostic else "observed"


def main():
    parser = argparse.ArgumentParser(description="DEC-071 leader-only native experiment; help makes no Docker calls")
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--engine-sha256", required=True)
    parser.add_argument("--build-evidence", type=Path, required=True)
    parser.add_argument("--check-local", action="store_true", help="validate existing local inputs without Docker or experiment setup")
    parser.add_argument("--boundary-only-diagnostic", action="store_true", help="one separately authorized boundary diagnostic; never five-case qualification")
    args = parser.parse_args()
    require(__debug__, "assertions disabled")
    if args.check_local:
        evidence = local_preflight(args.engine, args.engine_sha256, args.build_evidence)
        print(json.dumps({"status": "local-inputs-only", "engine_sha256": evidence["engine_sha256"], "payload_sha256": evidence["payload_sha256"], "docker_executed": False}))
        return 0
    budget = Budget()
    run = uuid.uuid4().hex
    root = Path("/private/tmp/ca-q2-" + run)
    os.umask(0o077)
    signal.signal(signal.SIGTERM, interrupted)
    setup_fixture(root, run)
    controller = Controller(root, run, budget)
    report = {"run_id": run, "root": str(root), "status": "unavailable", "cases": [], "native_admitted": False,
              "mode": "boundary-only-diagnostic" if args.boundary_only_diagnostic else "five-case-qualification",
              "selected_cases": list(selected_cases(args.boundary_only_diagnostic)), "combined_qualification_passed": False,
              "permanent_runtime_selected": False, "aggregate_cpu_budget_proven": False, "review": "deferred",
              "gaps": ["full Task 2 admission", "aggregate CPU-time and disk budgets", "native harness/auth/child control/live recovery", "worker-engine bridge and arbitrary escape resistance"]}
    engine, engine_thread, engine_log, engine_socket = None, None, {}, None
    operations = ["host-before", "host-create"]
    try:
        report["attempt"] = claim_attempt(ROOT / ".local-state", args.boundary_only_diagnostic,
                                          {"run_id": run, "root": str(root), "source_sha256": file_hash(Path(__file__)), "engine_sha256": args.engine_sha256})
        with (root / "evidence" / "allocation.lock").open("x") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            report["provenance"] = preflight(controller, args.engine, args.engine_sha256, args.build_evidence)
            command = [str(args.engine), "engine", str(root / "host-engine"), str(root / "host-engine" / "global.toml"), str(root / "host-engine" / "project.toml"), "engine.sock"]
            engine = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, env=ENVIRONMENT)

            def collect_engine():
                try:
                    engine_log.update(capture(command, budget.started + 1200, process=engine))
                except BaseException as error:
                    engine_log["error"] = str(error)

            engine_thread = threading.Thread(target=collect_engine)
            engine_thread.start()
            ready_by = min(time.monotonic() + 5, budget.started + 1200)
            for attempt in range(100):
                if (root / "host-engine" / "engine.sock").exists():
                    break
                require(engine.poll() is None and time.monotonic() < ready_by, "host engine readiness unavailable")
                time.sleep(0.05)
            require((root / "host-engine" / "engine.sock").exists() and time.monotonic() <= ready_by, "host engine readiness unavailable within five seconds")
            engine_socket = engine_socket_receipt(root, engine)
            report["engine_socket_identity"] = engine_socket
            before = engine_request(root, "host-before", "0", {"kind": "Snapshot"})
            require(before["revision"] == "0" and before["outcome"] == {"kind": "Snapshot", "projects": [], "tasks": []}, "initial engine control failed")
            created = engine_request(root, "host-create", before["revision"], {"kind": "CreateProject", "project_id": "synthetic-project", "spec": {"name": "Synthetic containment fixture", "repositories": []}})
            require(created["outcome"] == {"kind": "Applied"} and created["revision"] == "1", "real host mutation failed")
            baseline = canaries(root)
            report["canaries_before"] = baseline
            host_state(root, "host-control", operations)
            operations.append("host-control")
            for case in selected_cases(args.boundary_only_diagnostic):
                require(engine.poll() is None and not engine_log.get("overflow"), "host engine control/log unavailable")
                result = execute_case(controller, case, baseline)
                report["cases"].append(result)
                host_state(root, "host-after-" + case, operations)
                operations.append("host-after-" + case)
                save(root, "report.json", report)
                if result["status"] != "observed" or not result["cleanup"]["confirmed"]:
                    break
            report["status"] = completion_status(args.boundary_only_diagnostic, report["cases"])
    except (OSError, ValueError, RuntimeError, KeyboardInterrupt, subprocess.SubprocessError, sqlite3.Error) as error:
        report["error"] = str(error) or "interrupted"
    finally:
        if engine is not None:
            try:
                if engine.poll() is None:
                    host_state(root, "host-final", operations)
                    report["final_host_state"] = "unchanged"
            except (OSError, ValueError, sqlite3.Error) as error:
                report["final_host_state"] = str(error)
                report["status"] = "failed"
            try:
                if engine.poll() is None:
                    engine.terminate()
                try:
                    engine.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    engine.kill()
                    engine.wait(timeout=1)
                if engine_thread:
                    engine_thread.join(timeout=1)
                report["engine_cleanup"] = {"pid": engine.pid, "returncode": engine.returncode, "reaped": engine.returncode is not None,
                                            "socket_absent": not (root / "host-engine" / "engine.sock").exists()}
                save(root, "engine-log.json", engine_log)
                require(engine_socket is not None, "engine socket identity was not captured; no socket removal authorized")
                report["engine_cleanup"]["socket_cleanup"] = cleanup_engine_socket(root, engine, engine_socket)
                report["engine_cleanup"]["socket_absent"] = report["engine_cleanup"]["socket_cleanup"]["socket_absent"]
                require(not engine_log.get("overflow") and not engine_log.get("error"), "engine log incomplete")
                require(report["engine_cleanup"]["socket_absent"], "stale owned engine socket retained")
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                report["engine_cleanup_error"], report["status"] = str(error), "failed"
        report["elapsed_seconds"] = time.monotonic() - budget.started
        if report["elapsed_seconds"] > 1200:
            report["status"], report["allocation_error"] = "failed", "total allocation exceeded"
        try:
            report["fixture_bytes"] = fixture_size(root)
            if "canaries_before" in report:
                report["canaries_unchanged"] = canaries(root) == report["canaries_before"]
                require(report["canaries_unchanged"], "canaries changed")
        except (OSError, ValueError) as error:
            report["fixture_error"], report["status"] = str(error), "failed"
        report["combined_qualification_passed"] = not args.boundary_only_diagnostic and report["status"] == "observed"
        save(root, "report.json", report)
    print(json.dumps({"status": report["status"], "evidence": str(root), "elapsed_seconds": report["elapsed_seconds"]}))
    return 0 if report["status"] in ("observed", "diagnostic-observed") else 1


if __name__ == "__main__":
    raise SystemExit(main())
