# Native execution boundary results

Date: October 18, 2026. Status: filesystem/network and actual worker/descendant engine-denial subsets verified; complete confinement unavailable. Task 2 is not fully complete.

## Current worker-to-engine evidence

Source worker `71e9ce7`, integrated as `a88573d`, adds a real disposable engine process and uses the existing protocol, private Unix socket and SQLite state. The leader-native focused run passed. Both unconfined worker and child successfully wrote candidate data, read and wrote synthetic protected state, sent SIGCONT to the owned engine, connected to its socket and submitted real project-control operations. The same operations under sandbox-runtime 0.0.80 retained candidate writes and returned EPERM for protected reads/writes, signals, socket contact and control requests in both worker and child. Host-side persisted project IDs contained only the host and two unconfined positive-control IDs. The fixture engine was reaped, with no unrelated process signaling.

The focused run retained raw observations and policy in `/private/tmp/crazyagent-web-LdBt4Z`. Its `engine-boundary-evidence/confined-evidence.json` SHA-256 is `bd761695fe77447b1b68431c4c7cb4efca86b2433da7cb0e45316f191a8d0cbd`. The leader log is `.worktrees/worker-engine-boundary/.local-state/native-leader-engine-boundary.log`. Earlier outer-sandbox startup failures remain explicit infrastructure failures.

The integrated native make check passed formatting, strict Clippy, 50 real tests, build and the same 50 tests again, with zero failures or ignored tests. Log `.local-state/integrated-engine-boundary-check.log` SHA-256 is `e6d7a23ce16dc4673a6022461d43ffced59cdf102b0cc3c780e376180184a27d`. IMP-046 through IMP-049 retain function decisions, fixture lifecycle and actual native results.

The tested engine-denial subset does not prove every host path, every process-control operation, an aggregate CPU budget, memory/PID enforcement or runtime default write exceptions. Complete native admission remains unavailable. The early rejected-admission checkpoint in [the forecast](forecast.md) remains historical and is not rewritten into a pass.

## Original filesystem and resource evidence

Date: October 17, 2026. Source: worker `808f108`, integrated as `381d6d6`. The original observations and failed attempts below remain retained.

## Tested runtime and scope

Actual sandbox-runtime 0.0.80, host and sandboxed child Node v26.12.0, macOS host, Rust 1.99.1. Disposable sibling candidate/trusted fixtures contain only synthetic nonsecret sentinels and a synthetic credential. No provider call, real credential, host setting or global package change occurred.

Allowed candidate writes, child candidate writes and an independent host loopback control succeeded. Trusted writes, traversal, symlink escape, child trusted writes, synthetic credential read and the tested forbidden TCP connection were denied. Denial checks combine permission-specific child errors with parent-observed unchanged bytes, absent credential copies and no accepted forbidden connection. These finite cases do not prove every filesystem path or network protocol is confined.

## Resource outcomes

| Control | Actual evidence | Qualification limit |
| --- | --- | --- |
| Two-second CPU | Fixed Rust child inherited soft/hard CPU limit and terminated with SIGXCPU after approximately 2.1 seconds | Per-process only; not applied to sandbox-runtime or proven as an aggregate worker-tree budget |
| 64 MiB memory | AS/RSS setters returned EINVAL; a bounded 24 MiB allocation/touch control succeeded | Required memory enforcement unavailable |
| Four processes | Limit inherited, but even the first child spawn failed; unrestricted control started six short-lived children | A per-user limit is not a usable isolated task quota |
| Wall watchdog | A real five-second sleep was terminated by the 40 ms watchdog test within two seconds | Probe supervision only; escaping descendants and long-lived runner control remain unqualified |

CPU, memory and process controls remain explicitly unenforced in boundary evidence. Engine endpoint denial awaits the actual endpoint; default writable exceptions remain unqualified. Complete observations with these gaps map to `Unavailable`, never admission.

## Verification and retained evidence

The worker's normal coding sandbox denied two loopback binds with EPERM. Tests were preserved. The leader reran the unchanged suite in the owner-authorized local-probe environment and received exit 0, then repeated it after source integration with exit 0. Each run passed formatting, strict Clippy, 17 library tests, 5 boundary tests, 3 real-binary CLI tests and 1 resource-observation test, followed by build and the same tests again. Zero tests were ignored.

Worker raw output is at `.worktrees/execution-boundary/.local-state/leader-native-check.log`; integrated raw output is at `.local-state/integrated-native-check.log`. Actual fixture/evidence directories are retained under each checkout's `.local-state/boundary-tests` and `.local-state/resource-tests`. The historical worker handoff and its exact earlier artifact paths remain at `.worktrees/execution-boundary/.local-state/task-02-report.md`. Only synthetic fixture data is involved. Review gates have not passed.

The implementation decision log contains IMP-021 through IMP-026 and a complete 22-function Rust coverage index, plus the fixed embedded JavaScript probe functions. Native observations are separate from trusted evidence admission and do not authenticate a worker-supplied report.

## Authorized next experiment

DEC-069 approves the [bounded Docker prototype](resource-boundary-proposal.md) using the existing Docker Desktop Linux VM and cgroup v2. This is an experiment with strict local resource caps. It does not adopt the permanent runtime, weaken launch requirements or authorize native account/budget testing. Independent Task 5 Rust core implementation proceeds while complete native admission remains blocked.
