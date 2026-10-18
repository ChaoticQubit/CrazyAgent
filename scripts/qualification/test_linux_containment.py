import copy
from contextlib import contextmanager, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest

import linux_containment as probe


class ContainmentTests(unittest.TestCase):
    def test_actual_host_topology_is_emitted_even_when_local_route_files_are_unavailable(self):
        namespace = {"__name__": "payload_helper_test"}
        exec(compile(probe.PAYLOAD, "probe.py", "exec"), namespace)
        output = io.StringIO()
        with redirect_stdout(output):
            try:
                namespace["topology"]()
            except (OSError, ValueError):
                pass
        self.assertTrue(output.getvalue(), "actual interfaces were lost before rejection")
        self.assertEqual(json.loads(output.getvalue())["interfaces"], [list(item) for item in socket.if_nameindex()])

    def test_topology_retains_real_interfaces_and_bounded_routes_before_rejection(self):
        namespace = {"__name__": "payload_helper_test"}
        exec(compile(probe.PAYLOAD, "probe.py", "exec"), namespace)
        with self.assertRaises(AssertionError):
            namespace["topology"]("unknown")
        with tempfile.TemporaryDirectory() as directory:
            ipv4, ipv6 = Path(directory) / "route", Path(directory) / "route6"
            ipv4.write_text("Iface Destination Gateway Flags\nlo 00000000 00000000 0001\n")
            ipv6.write_text("")
            output = io.StringIO()
            with redirect_stdout(output), self.assertRaises(ValueError):
                namespace["topology"]("worker", ipv4, ipv6)
            observation = json.loads(output.getvalue())
            self.assertEqual(observation["phase"], "topology-worker")
            self.assertEqual(observation["interfaces"], [list(item) for item in socket.if_nameindex()])
            self.assertEqual(observation["ipv4_routes"], ipv4.read_text().splitlines()[1:])
            self.assertEqual(observation["ipv6_routes"], [])
            ipv4.write_text("x" * 5000)
            output = io.StringIO()
            with redirect_stdout(output), self.assertRaisesRegex(ValueError, "incomplete topology"):
                namespace["topology"]("child", ipv4, ipv6)
            observation = json.loads(output.getvalue())
            self.assertFalse(observation["complete"])
            self.assertLessEqual(sum(map(len, observation["ipv4_routes"])), 4096)
            self.assertLess(len(output.getvalue()), 16384)
            ipv4.unlink()
            output = io.StringIO()
            with redirect_stdout(output), self.assertRaisesRegex(ValueError, "incomplete topology"):
                namespace["topology"]("worker", ipv4, ipv6)
            self.assertIn("ipv4_routes", json.loads(output.getvalue())["read_errors"])

    def test_topology_validator_keeps_exact_expectation_on_controlled_inputs(self):
        namespace = {"__name__": "payload_helper_test"}
        exec(compile(probe.PAYLOAD, "probe.py", "exec"), namespace)
        observation = {"interfaces": [(1, "lo")], "ipv4_routes": [], "ipv6_routes": [], "complete": True}
        namespace["validate_topology"](observation)
        for interfaces in ([], [(1, "renamed")], [(1, "lo"), (2, "eth0")]):
            with self.subTest(interfaces=interfaces), self.assertRaisesRegex(ValueError, "non-loopback interface"):
                namespace["validate_topology"]({**observation, "interfaces": interfaces})
        for key, route in (("ipv4_routes", "lo 00000000 00000000 0001"),
                           ("ipv6_routes", "0" * 32 + " 00 " + "0" * 32 + " 00 " + "0" * 32 + " 00000000 00000000 00000000 00000001 lo")):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "default IPv"):
                namespace["validate_topology"]({**observation, key: [route]})

    def test_failed_execution_traceback_precedes_missing_positive_control_file(self):
        result = probe.capture([sys.executable, "-c", "raise ValueError('primary topology rejection')"], time.monotonic() + 2)
        self.assertNotEqual(result["returncode"], 0)
        record = {"execution": result, "status": "unavailable", "cleanup": {"confirmed": True, "absent": True}}
        with tempfile.TemporaryDirectory() as directory:
            finalized = probe.finalize_case(Path(directory), "boundary", record, {}, "a" * 32, 501, 20)
        self.assertEqual(finalized["status"], "failed")
        self.assertIn("primary topology rejection", finalized["error"])
        self.assertIn("Traceback", finalized["error"])
        self.assertNotIn("child.txt", finalized["error"])
        self.assertEqual(finalized["execution"], result)
        record["error"] = "earlier controller failure"
        self.assertEqual(probe.finalize_case(Path("/unused"), "boundary", record, {}, "a" * 32, 501, 20)["error"], "earlier controller failure")

    def test_diagnostic_has_one_fixed_case_separate_exclusive_marker_and_no_qualification(self):
        self.assertEqual(probe.selected_cases(False), probe.CASES)
        self.assertEqual(probe.selected_cases(True), ("boundary",))
        with self.assertRaises(AssertionError):
            probe.selected_cases("cpu")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "linux-containment-native-attempt.json"
            original.write_bytes(b"immutable original attempt")
            provenance = {"run_id": "a" * 32, "source_sha256": probe.file_hash(Path(probe.__file__)), "engine_sha256": "b" * 64}
            marker = probe.claim_attempt(root, True, provenance)
            diagnostic = root / "linux-topology-diagnostic-attempt.json"
            self.assertEqual(marker, json.loads(diagnostic.read_text()))
            self.assertEqual(marker["mode"], "boundary-only-diagnostic")
            self.assertEqual(marker["cases"], ["boundary"])
            self.assertEqual(marker["payload_sha256"], probe.payload_hash())
            self.assertEqual(original.read_bytes(), b"immutable original attempt")
            for diagnostic_mode in (True, False):
                with self.assertRaises(FileExistsError):
                    probe.claim_attempt(root, diagnostic_mode, provenance)
            self.assertEqual(original.read_bytes(), b"immutable original attempt")
            self.assertEqual(json.loads(diagnostic.read_text()), marker)
        observed = {"case": "boundary", "status": "observed", "cleanup": {"confirmed": True, "absent": True}}
        self.assertEqual(probe.completion_status(True, [observed]), "diagnostic-observed")
        self.assertEqual(probe.completion_status(False, [observed]), "failed")
        self.assertEqual(probe.completion_status(True, [observed, observed]), "failed")
        self.assertEqual(probe.completion_status(True, [{**observed, "case": "cpu"}]), "failed")
        self.assertEqual(probe.completion_status(True, [{**observed, "cleanup": {"confirmed": False, "absent": False}}]), "failed")
        complete = [{**observed, "case": case} for case in probe.CASES]
        self.assertEqual(probe.completion_status(False, complete), "observed")

    def test_cli_exposes_only_explicit_diagnostic_selection_without_docker(self):
        result = probe.capture([sys.executable, str(Path(probe.__file__)), "--help"], time.monotonic() + 2)
        self.assertEqual(result["returncode"], 0)
        self.assertIn("--boundary-only-diagnostic", result["stdout"])
        for option in ("--case", "--marker", "--limit", "--ignore-failure"):
            self.assertNotIn(option, result["stdout"])

    @contextmanager
    def owned_socket_fixture(self):
        with tempfile.TemporaryDirectory(prefix="ca-lc-clean-", dir="/private/tmp") as directory:
            root = Path(directory).resolve() / "run"
            root.mkdir(mode=0o700)
            (root / "host-engine").mkdir(mode=0o700)
            path = root / "host-engine" / "engine.sock"
            code = "import os,socket,sys,time; os.umask(0o077); s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen(1); time.sleep(30)"
            process = subprocess.Popen([sys.executable, "-c", code, str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=probe.ENVIRONMENT)
            try:
                for attempt in range(100):
                    if path.exists():
                        break
                    if process.poll() is not None:
                        self.fail("real socket fixture exited: " + process.stderr.read(32768).decode())
                    time.sleep(0.01)
                self.assertTrue(path.exists(), "real Unix socket was not ready")
                yield root, process
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=1)
                process.stderr.close()

    def test_allocation_reserves_cleanup_and_refuses_late_cases(self):
        budget = probe.Budget()
        self.assertLessEqual(budget.command_deadline() - time.monotonic(), 3)
        budget.started -= 781
        with self.assertRaises(ValueError):
            budget.admit_case()
        budget.started -= 420
        with self.assertRaises(ValueError):
            budget.command_deadline()

    def test_capture_reaps_real_process_and_inherited_pipe_on_deadline(self):
        code = "import os,time; p=os.fork(); print(p,flush=True); time.sleep(30)"
        start = time.monotonic()
        result = probe.capture([sys.executable, "-c", code], start + 0.3)
        self.assertTrue(result["timed_out"])
        self.assertLess(time.monotonic() - start, 2)
        self.assertIsNotNone(result["returncode"])
        self.assertNotEqual(result["returncode"], 0)

    def test_capture_terminates_stream_overflow_and_bounds_both_streams(self):
        result = probe.capture(
            [sys.executable, "-c", "import os,time; os.write(1,b'x'*40000); os.write(2,b'y'*40000); time.sleep(30)"],
            time.monotonic() + 2,
        )
        self.assertTrue(result["overflow"])
        self.assertLessEqual(len(result["stdout"].encode()), 32768)
        self.assertLessEqual(len(result["stderr"].encode()), 32768)
        self.assertIsNotNone(result["returncode"])

    def test_capture_does_not_inherit_account_environment(self):
        result = probe.capture([sys.executable, "-c", "import os,json; print(json.dumps(sorted(os.environ)))"], time.monotonic() + 2)
        self.assertEqual(result["returncode"], 0)
        self.assertLessEqual(set(json.loads(result["stdout"])), {"PATH", "LANG", "LC_CTYPE", "__CF_USER_TEXT_ENCODING"})

    def test_profile_argv_has_exact_fixed_scope(self):
        root = Path("/private/tmp/ca-q2-" + "a" * 32)
        args = probe.create_arguments(root, "a" * 32, "boundary", 501, 20)
        self.assertEqual(args.count("--mount"), 3)
        self.assertIn("--pull=never", args)
        self.assertIn("--memory-swap=64m", args)
        self.assertIn("--pids-limit=16", args)
        self.assertIn("501:20", args)
        self.assertIn(probe.IMAGE, args)
        self.assertFalse(any("host-engine" in item for item in args))
        for case, uid, gid in (("extra", 501, 20), ("memory", 0, 20), ("cpu", 501, 0)):
            with self.assertRaises(ValueError):
                probe.create_arguments(root, "a" * 32, case, uid, gid)
        with self.assertRaises(ValueError):
            probe.create_arguments(Path("/tmp/other"), "a" * 32, "cpu", 501, 20)

    def test_identity_rejects_any_changed_authority_field(self):
        run, case, image = "a" * 32, "boundary", "sha256:" + "b" * 64
        data = {"Id": "c" * 64, "Name": "/ca-q2-" + run + "-" + case, "Image": image,
                "Config": {"Image": probe.IMAGE, "Labels": {probe.LABEL: run, probe.LABEL + ".case": case}}}
        self.assertEqual(probe.validate_identity(data, run, case, image), "c" * 64)
        for field in ("Id", "Name", "Image"):
            changed = copy.deepcopy(data)
            changed[field] = "other"
            with self.assertRaises(ValueError):
                probe.validate_identity(changed, run, case, image)
        changed = copy.deepcopy(data)
        changed["Config"]["Labels"][probe.LABEL + ".case"] = "cpu"
        with self.assertRaises(ValueError):
            probe.validate_identity(changed, run, case, image)

    def test_fixture_bounds_count_symlinks_without_following(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate = root / "candidate" / "boundary"
            candidate.mkdir(parents=True)
            (candidate / "outside").symlink_to("/does-not-exist")
            self.assertGreater(probe.fixture_size(root), 0)
            (candidate / "oversized").write_bytes(b"x" * 65537)
            with self.assertRaises(ValueError):
                probe.fixture_size(root)

    def test_config_rejects_authority_keys_without_echoing_values(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory).resolve()
            (config / "config.json").write_text('{"auths": {"sentinel": "DO-NOT-ECHO"}}')
            with self.assertRaisesRegex(ValueError, "empty") as error:
                probe.validate_config(config)
            self.assertNotIn("DO-NOT-ECHO", str(error.exception))
            (config / "config.json").write_text("{}")
            probe.validate_config(config)
            (config / "config.json").write_text('{"auths": {}}')
            probe.validate_config(config)
            (config / "contexts").mkdir()
            with self.assertRaises(ValueError):
                probe.validate_config(config)

    def test_frame_limits_and_truncated_real_socket(self):
        with self.assertRaises(ValueError):
            probe.frame({"value": "x" * 65536})
        left, right = socket.socketpair()
        try:
            left.settimeout(0.1)
            right.sendall(b"ab")
            right.close()
            with self.assertRaises(ValueError):
                probe.receive_exact(left, 4)
        finally:
            left.close()
            right.close()

    def test_payload_is_bounded_and_has_no_host_pid_or_import_side_effect(self):
        namespace = {"__name__": "payload_helper_test"}
        exec(compile(probe.PAYLOAD, "probe.py", "exec"), namespace)
        self.assertLess(len(probe.PAYLOAD.encode()), 60000)
        with self.assertRaises(AssertionError):
            namespace["invariant"](False, "trip")
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing"
            observed = namespace["observe_errno"](lambda: missing.read_bytes())
            self.assertEqual(observed, 2)
        self.assertEqual(hashlib.sha256(probe.PAYLOAD.encode()).hexdigest(), probe.payload_hash())

    def test_internal_assertion_is_enabled_and_trippable(self):
        with self.assertRaises(AssertionError):
            probe.invariant(False, "trip")

    def test_owned_socket_cleanup_removes_only_original_after_reap(self):
        with self.owned_socket_fixture() as (root, process):
            receipt = probe.engine_socket_receipt(root, process)
            process.terminate()
            process.wait(timeout=1)
            result = probe.cleanup_engine_socket(root, process, receipt)
            self.assertTrue(result["removed"])
            self.assertTrue(result["socket_absent"])
            self.assertFalse(os.path.lexists(root / "host-engine" / "engine.sock"))
            self.assertFalse(probe.cleanup_engine_socket(root, process, receipt)["removed"])

    def test_owned_socket_cleanup_refuses_live_process(self):
        with self.owned_socket_fixture() as (root, process):
            receipt = probe.engine_socket_receipt(root, process)
            with self.assertRaises(ValueError):
                probe.cleanup_engine_socket(root, process, receipt)
            self.assertTrue(stat.S_ISSOCK((root / "host-engine" / "engine.sock").lstat().st_mode))

    def test_owned_socket_cleanup_refuses_replaced_socket_file_and_symlink(self):
        for replacement in ("socket", "file", "symlink"):
            with self.subTest(replacement=replacement), self.owned_socket_fixture() as (root, process):
                receipt = probe.engine_socket_receipt(root, process)
                process.terminate()
                process.wait(timeout=1)
                path = root / "host-engine" / "engine.sock"
                original = path.with_name("original.sock")
                path.rename(original)
                with socket.socket(socket.AF_UNIX) as changed:
                    if replacement == "socket":
                        changed.bind(str(path))
                        path.chmod(0o700)
                    elif replacement == "file":
                        path.write_text("replacement must survive")
                    else:
                        path.symlink_to(original)
                    identity = path.lstat()
                    with self.assertRaises(ValueError):
                        probe.cleanup_engine_socket(root, process, receipt)
                    self.assertEqual(path.lstat(), identity)
                    self.assertTrue(original.exists())

    def test_owned_socket_cleanup_refuses_replaced_root_or_parent(self):
        for component in ("root", "parent"):
            with self.subTest(component=component), self.owned_socket_fixture() as (root, process):
                receipt = probe.engine_socket_receipt(root, process)
                process.terminate()
                process.wait(timeout=1)
                target = root if component == "root" else root / "host-engine"
                target.rename(target.with_name(target.name + "-original"))
                target.mkdir(mode=0o700)
                if component == "root":
                    (root / "host-engine").mkdir(mode=0o700)
                path = root / "host-engine" / "engine.sock"
                with socket.socket(socket.AF_UNIX) as changed:
                    changed.bind(str(path))
                    path.chmod(0o700)
                    identity = path.lstat()
                    with self.assertRaises(ValueError):
                        probe.cleanup_engine_socket(root, process, receipt)
                    self.assertEqual(path.lstat(), identity)

    def test_local_preflight_rejects_unbound_binary_without_docker(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            binary = root / "engine"
            binary.write_bytes(b"synthetic input, never executed")
            binary.chmod(0o700)
            binding = root / "binding.json"
            binding.write_text("{}")
            with self.assertRaises(ValueError):
                probe.local_preflight(binary, hashlib.sha256(binary.read_bytes()).hexdigest(), binding)

    def test_profile_rejects_relaxed_limits_and_extra_mounts_from_controlled_data(self):
        run, case, image = "a" * 32, "boundary", "sha256:" + "b" * 64
        root = Path("/private/tmp/ca-q2-" + run)
        data = {"Id": "c" * 64, "Name": "/ca-q2-" + run + "-" + case, "Image": image,
                "State": {"Status": "created", "Running": False},
                "Config": {"Image": probe.IMAGE, "Labels": {probe.LABEL: run, probe.LABEL + ".case": case},
                           "User": "501:20", "Entrypoint": ["/usr/local/bin/python3"],
                           "Cmd": ["-I", "-B", "-u", "/probe/probe.py", case], "WorkingDir": "/workspace"},
                "HostConfig": {"Memory": 67108864, "MemorySwap": 67108864, "NanoCpus": 500000000,
                               "PidsLimit": 16, "NetworkMode": "none", "IpcMode": "none", "PidMode": "",
                               "CgroupnsMode": "private", "ReadonlyRootfs": True, "Privileged": False,
                               "AutoRemove": False, "CapDrop": ["ALL"], "SecurityOpt": ["no-new-privileges:true"],
                               "RestartPolicy": {"Name": "no"}, "LogConfig": {"Type": "none"},
                               "Tmpfs": {"/tmp": "rw,noexec,nosuid,nodev,size=8m,mode=1777"},
                               "Ulimits": [{"Name": name, "Soft": size, "Hard": size} for name, size in (("core", 0), ("fsize", 65536), ("nofile", 64))]},
                "Mounts": [{"Type": "bind", "Source": str(root / source), "Destination": destination, "RW": rw, "Propagation": "rprivate"}
                           for source, destination, rw in (("candidate/boundary", "/workspace", True), ("protected", "/protected", False), ("payload", "/probe", False))]}
        probe.validate_profile(data, root, run, case, image, 501, 20)
        for key, value in (("MemorySwap", 134217728), ("PidsLimit", 17), ("NetworkMode", "host"), ("ReadonlyRootfs", False), ("PidMode", "host"), ("IpcMode", "host"), ("CapAdd", ["SYS_ADMIN"]), ("SecurityOpt", [])):
            changed = copy.deepcopy(data)
            changed["HostConfig"][key] = value
            with self.assertRaises(ValueError):
                probe.validate_profile(changed, root, run, case, image, 501, 20)
        changed = copy.deepcopy(data)
        changed["Mounts"].append({"Type": "bind", "Source": "/", "Destination": "/host", "RW": True, "Propagation": "rprivate"})
        with self.assertRaises(ValueError):
            probe.validate_profile(changed, root, run, case, image, 501, 20)

    def test_real_host_engine_protocol_sqlite_and_mutation_detection(self):
        binary = Path(__file__).resolve().parents[2] / "target/debug/crazyagent"
        self.assertTrue(binary.is_file(), "run make build before this real local engine check")
        with tempfile.TemporaryDirectory(prefix="ca-lc-unit-", dir="/private/tmp") as directory:
            root = Path(directory)
            engine_dir = root / "host-engine"
            engine_dir.mkdir(mode=0o700)
            (root / "evidence").mkdir(mode=0o700)
            for name in ("global.toml", "project.toml"):
                (engine_dir / name).write_text("")
            process = subprocess.Popen([str(binary), "engine", str(engine_dir), str(engine_dir / "global.toml"), str(engine_dir / "project.toml"), "engine.sock"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=probe.ENVIRONMENT)
            socket_receipt = None
            try:
                for attempt in range(100):
                    if (engine_dir / "engine.sock").exists():
                        break
                    if process.poll() is not None:
                        self.fail("real engine exited before readiness: " + process.stderr.read(32768).decode())
                    time.sleep(0.01)
                before = probe.engine_request(root, "host-before", "0", {"kind": "Snapshot"})
                self.assertEqual(before["revision"], "0")
                socket_receipt = probe.engine_socket_receipt(root, process)
                created = probe.engine_request(root, "host-create", before["revision"], {"kind": "CreateProject", "project_id": "synthetic-project", "spec": {"name": "Synthetic containment fixture", "repositories": []}})
                self.assertEqual(created["outcome"], {"kind": "Applied"})
                probe.host_state(root, "host-control", ["host-before", "host-create"])
                changed = probe.engine_request(root, "unexpected-mutation", "1", {"kind": "CreateProject", "project_id": "unexpected-project", "spec": {"name": "Synthetic extra", "repositories": []}})
                self.assertEqual(changed["outcome"], {"kind": "Applied"})
                with self.assertRaises(ValueError):
                    probe.host_state(root, "host-after", ["host-before", "host-create", "host-control", "unexpected-mutation"])
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=1)
                stdout, stderr = process.communicate(timeout=1)
                self.assertLessEqual(len(stdout), 32768)
                self.assertLessEqual(len(stderr), 32768)
                if socket_receipt is not None:
                    probe.cleanup_engine_socket(root, process, socket_receipt)
            self.assertFalse((engine_dir / "engine.sock").exists())


if __name__ == "__main__":
    unittest.main()
