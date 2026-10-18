from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import linux_containment as probe


def payload_namespace():
    namespace = {"__name__": "offline_contract_unit_inputs"}
    exec(compile(probe.PAYLOAD, "probe.py", "exec"), namespace)
    return namespace


def controlled_observation():
    return {
        "complete": True,
        "read_errors": {},
        "interfaces": [[1, "lo"], [2, "tunl0"], [3, "gre0"]],
        "links": {
            "lo": {"flags": 0x10049, "operstate": "unknown"},
            "tunl0": {"flags": 0x80, "operstate": "down"},
            "gre0": {"flags": 0x80, "operstate": "down"},
        },
        "ipv4_routes": [],
        "ipv6_routes": [],
    }


class OfflineTopologyTests(unittest.TestCase):
    def test_original_strict_validator_still_rejects_extra_down_devices(self):
        namespace = payload_namespace()
        with self.assertRaisesRegex(ValueError, "non-loopback interface"):
            namespace["validate_topology"](controlled_observation())

    def test_distinct_offline_validator_accepts_only_complete_down_fallbacks(self):
        namespace = payload_namespace()
        observation = controlled_observation()
        self.assertIsNone(namespace["validate_offline_topology"](observation))
        self.assertEqual(observation, controlled_observation())

    def test_all_observed_fallback_names_have_explicit_down_only_admission(self):
        namespace = payload_namespace()
        observation = controlled_observation()
        names = ("tunl0", "gre0", "gretap0", "erspan0", "ip_vti0", "ip6_vti0", "sit0", "ip6tnl0", "ip6gre0")
        observation["interfaces"] = [[1, "lo"]] + [[index + 2, name] for index, name in enumerate(names)]
        observation["links"] = {"lo": {"flags": 0x10049, "operstate": "unknown"}}
        observation["links"].update({name: {"flags": 0x80, "operstate": "down"} for name in names})
        self.assertIsNone(namespace["validate_offline_topology"](observation))
        observation["interfaces"].append([11, "eth0"])
        observation["links"]["eth0"] = {"flags": 0, "operstate": "down"}
        with self.assertRaises(ValueError):
            namespace["validate_offline_topology"](observation)

    def test_up_running_lower_up_and_unknown_operstate_each_fail(self):
        namespace = payload_namespace()
        for flags, state in ((0x81, "down"), (0xC0, "down"), (0x10080, "down"), (0x80, "unknown"), (0x80, "up"), (-1, "down"), (1 << 32, "down")):
            with self.subTest(flags=flags, state=state):
                observation = controlled_observation()
                observation["links"]["gre0"] = {"flags": flags, "operstate": state}
                with self.assertRaises(ValueError):
                    namespace["validate_offline_topology"](observation)

    def test_missing_conflicting_incomplete_and_error_observations_fail(self):
        namespace = payload_namespace()
        variations = []
        for key in ("complete", "links", "read_errors", "ipv4_routes", "ipv6_routes"):
            observation = controlled_observation()
            del observation[key]
            variations.append(observation)
        observation = controlled_observation()
        del observation["links"]["gre0"]
        variations.append(observation)
        observation = controlled_observation()
        observation["complete"] = False
        variations.append(observation)
        observation = controlled_observation()
        observation["read_errors"] = {"gre0": 13}
        variations.append(observation)
        observation = controlled_observation()
        observation["interfaces"].append([2, "gre0"])
        variations.append(observation)
        observation = controlled_observation()
        observation["interfaces"] = [[2, "tunl0"]]
        variations.append(observation)
        for observation in variations:
            with self.subTest(observation=observation):
                with self.assertRaises(ValueError):
                    namespace["validate_offline_topology"](observation)

    def test_usable_default_routes_and_routes_on_extra_devices_fail(self):
        namespace = payload_namespace()
        route6 = "0" * 32 + " 00 " + "0" * 32 + " 00 " + "0" * 32 + " 00000001 00000000 00000000 00000001 lo"
        for key, row in (("ipv4_routes", "lo 00000000 00000000 0001"), ("ipv4_routes", "tunl0 0100007F 00000000 0001"), ("ipv6_routes", route6), ("ipv6_routes", route6[:-2] + "gre0"), ("ipv6_routes", "malformed")):
            with self.subTest(key=key, row=row):
                observation = controlled_observation()
                observation[key] = [row]
                with self.assertRaises(ValueError):
                    namespace["validate_offline_topology"](observation)

    def test_explicit_loopback_reject_route_is_not_usable_default_route(self):
        namespace = payload_namespace()
        observation = controlled_observation()
        observation["ipv6_routes"] = ["0" * 32 + " 00 " + "0" * 32 + " 00 " + "0" * 32 + " ffffffff 00000001 00000000 00200200 lo"]
        self.assertIsNone(namespace["validate_offline_topology"](observation))

    def test_real_file_collector_is_bounded_and_retains_missing_state(self):
        namespace = payload_namespace()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("lo", "gre0"):
                (root / name).mkdir()
                (root / name / "flags").write_text("0x10049\n" if name == "lo" else "0x80\n")
                (root / name / "operstate").write_text("unknown\n" if name == "lo" else "down\n")
            observed = namespace["read_link_states"]([[1, "lo"], [2, "gre0"]], root)
            self.assertTrue(observed["complete"])
            self.assertEqual(observed["states"]["gre0"]["flags"], 0x80)
            (root / "gre0" / "flags").write_text("0" * 65)
            overflow = namespace["read_link_states"]([[1, "lo"], [2, "gre0"]], root)
            self.assertFalse(overflow["complete"])
            self.assertIn("gre0", overflow["read_errors"])
            (root / "gre0" / "flags").unlink()
            missing = namespace["read_link_states"]([[1, "lo"], [2, "gre0"]], root)
            self.assertFalse(missing["complete"])
            self.assertIn("gre0", missing["read_errors"])

    def test_collector_refuses_path_escape_and_has_a_tripable_input_invariant(self):
        namespace = payload_namespace()
        with tempfile.TemporaryDirectory() as directory:
            observed = namespace["read_link_states"]([[1, "../escape"]], Path(directory))
            self.assertFalse(observed["complete"])
            self.assertFalse(observed["states"])
            with self.assertRaises(AssertionError):
                namespace["read_link_states"]([[index, "lo"] for index in range(65)], Path(directory))

    def test_versioned_marker_is_exclusive_and_does_not_consume_old_markers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "linux-containment-native-attempt.json"
            diagnostic = root / "linux-topology-diagnostic-attempt.json"
            old.write_text("original")
            diagnostic.write_text("diagnostic")
            record = probe.claim_attempt(root, False, {"source_sha256": "controlled-unit-input"}, network_contract="offline-network-v2")
            self.assertEqual(record["network_contract"], "offline-network-v2")
            self.assertTrue((root / "linux-offline-network-v2-attempt.json").is_file())
            self.assertEqual(old.read_text(), "original")
            self.assertEqual(diagnostic.read_text(), "diagnostic")
            with self.assertRaises(FileExistsError):
                probe.claim_attempt(root, False, {}, network_contract="offline-network-v2")

    def test_help_and_mutually_exclusive_selection_make_no_experiment(self):
        script = Path(probe.__file__)
        help_result = subprocess.run([sys.executable, "-B", str(script), "--help"], capture_output=True, text=True, timeout=5)
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("--offline-network-contract", help_result.stdout)
        invalid = subprocess.run([sys.executable, "-B", str(script), "--engine", "/nonexistent", "--engine-sha256", "0" * 64, "--build-evidence", "/nonexistent", "--boundary-only-diagnostic", "--offline-network-contract"], capture_output=True, text=True, timeout=5)
        self.assertEqual(invalid.returncode, 2)
        self.assertIn("not allowed", invalid.stderr)


if __name__ == "__main__":
    unittest.main()
