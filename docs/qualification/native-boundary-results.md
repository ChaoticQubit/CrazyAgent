# Native execution boundary results

Date: October 17, 2026. Status: probe implementation verified; complete confinement unavailable. Task 2 is not fully complete. Source: worker `808f108`, integrated as `381d6d6`.

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
