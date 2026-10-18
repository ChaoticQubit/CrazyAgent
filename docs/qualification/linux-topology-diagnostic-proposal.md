# One-container topology diagnostic proposal

Date: October 18, 2026. Status: owner explicitly approved the one-container execution in DEC-072, Q-057 A. IMP-080 and IMP-082 through IMP-086 cover source preparation. DEC-071's failed original attempt remains preserved as a separate result.

## Why this attempt exists

The original boundary payload rejected its loopback-only expectation before saving actual interface names/routes. Its evaluator then encountered a missing child control file. The prepared change preserves bounded topology before the same strict validation and retains the primary traceback. It does not relax network policy, add connectivity attempts or replace the original result.

## Exact allocation

One invocation and at most one new boundary container, with no automatic retry. Use the existing Docker Desktop endpoint and empty credential-free Docker config, pinned local python@sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9 image, pull never, 64 MiB RAM and no extra swap, 0.5 CPU, sixteen PIDs, fifteen seconds maximum container lifetime and 1200 seconds total allocation. Keep the existing kill target at 11.9 seconds, twelve-second ceiling and cleanup reserve. No later memory/PID/CPU/deadline case is selected.

The complete profile and three fresh synthetic mounts are unchanged: one writable candidate, read-only protected canaries and read-only payload. No network access, credentials, provider/model calls, host sockets, privileges, devices, host namespace or unrelated workload access. The actual Rust engine/database/socket stay in the unmounted private host fixture. Keep read-only root, dropped capabilities, no-new-privileges, private namespace settings and all file/output/storage limits. Do not change Docker settings or file-sharing permissions.

Every mutation checks the exact CID, chosen name, run/case labels and image. Observe stop, exact removal and absence before reporting cleanup. Reap the owned host engine and safely remove only its verified original socket. Stop after any failure, unavailable observation, overflow, deadline or unconfirmed cleanup. Retain every raw record and both old and new attempt markers. Permanent runtime adoption, full containment qualification and native admission remain separate decisions.

## Frozen source and local proof

Worktree .worktrees/linux-topology, branch feat/linux-topology-diagnostic, frozen source commit 8cfbf15 and unchanged Rust source 41cd3b6. Python source SHA-256 0543d35f16c1819ad5e3eb5b14fd241de4e570349997cfe73c1a5eb181bdf49a; test source 04be8741969e89304067e76fc8a9f74ce5df09a82dd0171f2d6d759ba9394976; payload 74d7ffc5ba3ffc81957ccf03d46244e3d082569773a0cae22764b7fda6d4569e. The actual local engine hash is ef2172ec51ee327fd8f88166810c027a4cf137fb05be1848c19c47dc3b0a85d1, built offline from this worktree's unchanged Rust tree with recorded Rust/Cargo 1.99.1 and source manifest.

All twenty-four leader-native helper methods passed, zero failures/skips, in 1.243 seconds. They use real processes, sockets, files, the actual engine protocol and SQLite; pure controlled-input validator checks are not called Docker evidence. Log .worktrees/linux-topology/.local-state/linux-topology-leader-helper-tests.log, SHA-256 5ad86ce08064effbe85e5e63397cdd01149fa7e0b352ec0f8f351f4e438dff7e. Build binding .local-state/linux-topology-build-binding.json SHA-256 bb296e636f7cc352d87856e961e13850dbf15c0a3fb8385fb47e6eedf9f180c5. No Docker command has run from this worktree.

## Proposed command, only after approval

```sh
/usr/bin/env -i PATH=/usr/bin:/bin LANG=C /Applications/Xcode.app/Contents/Developer/usr/bin/python3 -B /Users/chaoticqubit/Projects/CrazyAgent/.worktrees/linux-topology/scripts/qualification/linux_containment.py \
  --engine /Users/chaoticqubit/Projects/CrazyAgent/.worktrees/linux-topology/target/debug/crazyagent \
  --engine-sha256 ef2172ec51ee327fd8f88166810c027a4cf137fb05be1848c19c47dc3b0a85d1 \
  --build-evidence /Users/chaoticqubit/Projects/CrazyAgent/.worktrees/linux-topology/.local-state/linux-topology-build-binding.json \
  --boundary-only-diagnostic
```

This exclusively creates linux-topology-diagnostic-attempt.json. Original mode retains its separate original marker. Even diagnostic-observed leaves combined_qualification_passed, native_admitted and permanent_runtime_selected false. Interface names/routes alone do not prove external reachability or its denial. Reviews remain deferred under DEC-061.
