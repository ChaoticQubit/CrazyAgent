import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time
import unittest
import uuid


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("container_resources.py")
ROOT = Path(__file__).resolve().parents[2]


class ContainerResourceChecks(unittest.TestCase):
    def load_probe(self):
        spec = importlib.util.spec_from_file_location("container_resources", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_real_process_output_is_bounded(self):
        probe = self.load_probe()
        result = probe.run_command(
            [sys.executable, "-c", "import os; os.write(1,b'x'*100000); os.write(2,b'y'*100000)"],
            time.monotonic() + 3,
        )
        self.assertEqual(result["returncode"], 0)
        self.assertEqual(result["stdout_bytes"], 100000)
        self.assertEqual(result["stderr_bytes"], 100000)
        self.assertEqual(len(result["stdout"].encode()), probe.OUTPUT_LIMIT)
        self.assertEqual(len(result["stderr"].encode()), probe.OUTPUT_LIMIT)
        self.assertTrue(result["truncated"])

    def test_real_process_deadline_terminates_client(self):
        probe = self.load_probe()
        started = time.monotonic()
        result = probe.run_command(
            [sys.executable, "-c", "import time; print('started',flush=True); time.sleep(5)"],
            started + 0.3,
        )
        self.assertTrue(result["timed_out"])
        self.assertNotEqual(result["returncode"], 0)
        self.assertIn("started", result["stdout"])
        self.assertLess(time.monotonic() - started, 2)
        with self.assertRaisesRegex(ValueError, "deadline must be in the future"):
            probe.run_command([sys.executable, "-c", "raise RuntimeError('must never start')"], time.monotonic() - 1)

    def test_internal_invariant_cannot_be_disabled(self):
        probe = self.load_probe()
        probe.invariant(True, "valid")
        with self.assertRaisesRegex(AssertionError, "tripped"):
            probe.invariant(False, "tripped")

    def test_observed_nanosecond_timestamps(self):
        """Replay CPU evidence from native run 74ad4c934ea44520a53c5f80430f5794.
        Inputs retain the observed timestamps, cgroup stream and outcome.
        Outputs verify production evaluation and exact nanosecond duration.
        """
        probe = self.load_probe()
        evidence = {
            "cleanup": {"confirmed": True},
            "state": {
                "StartedAt": "2026-10-18T02:53:23.385560418Z",
                "FinishedAt": "2026-10-18T02:53:27.137527211Z",
                "ExitCode": 0,
                "OOMKilled": False,
            },
            "execution": {
                "truncated": False, "interrupted": False, "timed_out": False,
                "elapsed_seconds": 3.8176821249999997,
                "stdout": r'''{"phase": "before", "cgroup": {"memory.max": "67108864", "memory.swap.max": "0", "memory.current": "8507392", "memory.events": "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\nsock_throttled 0", "pids.max": "16", "pids.current": "1", "pids.events": "max 0", "cpu.max": "50000 100000", "cpu.stat": "usage_usec 119253\nuser_usec 83086\nsystem_usec 36167\nnice_usec 0\nnr_periods 2\nnr_throttled 2\nthrottled_usec 92263\nnr_bursts 0\nburst_usec 0"}, "uid": 65534, "gid": 65534}
{"phase": "result", "elapsed_seconds": 3.5000042100145947, "iterations": 15531898}
{"phase": "after", "cgroup": {"memory.max": "67108864", "memory.swap.max": "0", "memory.current": "7557120", "memory.events": "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\nsock_throttled 0", "pids.max": "16", "pids.current": "1", "pids.events": "max 0", "cpu.max": "50000 100000", "cpu.stat": "usage_usec 1877440\nuser_usec 1840783\nsystem_usec 36657\nnice_usec 0\nnr_periods 37\nnr_throttled 37\nthrottled_usec 1828436\nnr_bursts 0\nburst_usec 0"}}
''',
            },
        }
        probe.evaluate("cpu_limit", evidence)
        self.assertEqual(evidence["running_nanoseconds"], 3751966793)
        self.assertEqual(evidence["running_seconds"], 3.751966793)
        self.assertEqual(evidence["cpu_delta"]["usage_usec"], 1758187)
        for finished in ("2026-10-18T02:53:38.385560419Z", "2026-10-18T02:53:23.385560417Z"):
            invalid_duration = {**evidence, "state": {**evidence["state"], "FinishedAt": finished}}
            with self.subTest(finished=finished):
                with self.assertRaisesRegex(ValueError, "actual container runtime exceeded 15 seconds"):
                    probe.evaluate("cpu_limit", invalid_duration)
        observed = probe.docker_timestamp_ns("2026-10-18T02:53:23.385560418+00:00")
        self.assertEqual(observed, probe.docker_timestamp_ns(evidence["state"]["StartedAt"]))
        self.assertEqual(observed, probe.docker_timestamp_ns("2026-10-17T22:53:23.385560418-04:00"))
        for fraction in ("", ".1", ".123456", ".1234567", ".46518975", ".385560418"):
            with self.subTest(fraction=fraction):
                base = probe.docker_timestamp_ns("2026-10-18T02:53:23Z")
                expected = int(fraction[1:].ljust(9, "0")) if fraction else 0
                self.assertEqual(probe.docker_timestamp_ns("2026-10-18T02:53:23" + fraction + "Z") - base, expected)
        for invalid in ("2026-10-18T02:53:23", "2026-10-18T02:53:23.1234567890Z", "2026-13-18T02:53:23Z"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(ValueError):
                    probe.docker_timestamp_ns(invalid)

    def test_real_docker_resource_controls(self):
        artifacts = ROOT / ".local-state" / "container-probe-tests" / uuid.uuid4().hex
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--artifacts", str(artifacts)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=180,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads((artifacts / "report.json").read_text())
        self.assertFalse(report["permanent_runtime_selected"])
        self.assertFalse(report["native_admitted"])
        self.assertEqual(report["status"], "proven")
        self.assertEqual(len(report["probes"]), 6)
        self.assertTrue(report["clean_container_confirmation"])
        for name, evidence in report["probes"].items():
            with self.subTest(probe=name):
                self.assertEqual(evidence["status"], "proven", evidence)
                self.assertTrue(evidence["cleanup"]["confirmed"])
                self.assertEqual(evidence["execution"]["timed_out"], name == "deadline")
                self.assertLess(evidence["execution"]["elapsed_seconds"], 15)
                self.assertTrue((artifacts / name / "before.json").is_file())
                self.assertTrue((artifacts / name / "after.json").is_file())
                self.assertEqual(evidence["before"]["memory.max"], "67108864")
                self.assertEqual(evidence["before"]["memory.swap.max"], "0")
                self.assertEqual(evidence["before"]["cpu.max"], "50000 100000")
                self.assertEqual(evidence["before"]["pids.max"], "4" if name == "pids_limit" else "16")
        self.assertFalse(report["probes"]["deadline"]["state"]["Running"])
        self.assertLessEqual(report["probes"]["deadline"]["running_seconds"], 15)
        memory = report["probes"]["memory_limit"]
        self.assertTrue(memory["state"]["OOMKilled"])
        self.assertEqual(memory["state"]["ExitCode"], 137)
        self.assertEqual(report["probes"]["memory_control"]["result"]["touched_bytes"], 8 * 1024 * 1024)
        high = report["probes"]["pids_control"]["result"]
        low = report["probes"]["pids_limit"]["result"]
        self.assertEqual(high["children"], 6)
        self.assertEqual(high["failures"], 0)
        self.assertGreater(low["children"], 0)
        self.assertGreater(low["failures"], 0)
        self.assertEqual(high["attempts"], low["attempts"])
        self.assertGreater(report["probes"]["pids_limit"]["pids_max_events_delta"], 0)
        cpu = report["probes"]["cpu_limit"]
        self.assertGreater(cpu["cpu_delta"]["nr_throttled"], 0)
        self.assertGreater(cpu["cpu_delta"]["throttled_usec"], 0)
        self.assertGreater(cpu["cpu_delta"]["usage_usec"], 0)
        self.assertFalse(report["aggregate_cpu_time_budget_proven"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
