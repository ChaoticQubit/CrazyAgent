# Bounded Linux containment result

Date: October 18, 2026. Authority: DEC-071 and the accepted linux-containment-proposal.md. Development-only source: worker e2c295a, base 57499cb. Permanent runtime adoption and native profile admission remain unavailable.

## Result

The single approved experiment failed at its first boundary case and stopped. One container was created, started, observed stopped and removed with exact identity checks. The memory, PID, CPU and deadline cases did not run. This is not a passing combined containment result.

The payload rejected its network topology because socket.if_nameindex did not satisfy the exact list containing only lo. The payload raised non-loopback interface before retaining the actual interface list or routes. Docker inspection reported NetworkMode none. The evidence does not distinguish an empty list, renamed interface or additional interface, and does not establish usable external connectivity. Do not weaken the expectation, infer an escape or classify this as a network-denial pass.

The host evaluator subsequently encountered the missing child.txt positive-control artifact. That is a consequence of the earlier payload failure, not proof of a separate filesystem denial. The raw container traceback is retained.

## Observed bounds and controls

The actual inspected profile used the existing pinned linux/arm64 Python image, 64 MiB memory, zero extra swap, 0.5 CPU, sixteen PIDs, network none, IPC none, private cgroup namespace, nonzero host UID/GID, read-only root, dropped capabilities and no-new-privileges. Only the three approved fresh synthetic bind mounts were present. The real engine, database and socket remained host-only and unmounted.

Before the failed case, cgroup observations reported memory.max 67108864, memory.swap.max 0, pids.max 16 and cpu.max 50000 100000. These are observed configuration values, not successful resource-pressure cases. Container runtime was approximately 0.521 seconds and the complete experiment took 1.235260166 monotonic seconds. Output remained within its stream bounds; retained fixture bytes were 293564.

Container 7317c7ae35647165aa16894cdb69f858901a922fa734bc2ddcd5d977c0348789 exited with status 1. Cleanup confirmed exact-container absence before the supervisor stopped. Synthetic protected canary hashes and metadata were unchanged. The actual host engine passed protocol and SQLite controls before and after the case and at final inspection, with no unexpected project/state mutation. The owned engine was reaped; exact original socket cleanup completed and pathname absence was confirmed. Other workloads were not inspected or changed.

## Evidence

Private fixture: /private/tmp/ca-q2-0065d51e2f85416f99a13a0f59b7d6e3. Report: evidence/report.json, SHA-256 e2906f2b7ae95e5e442b58c99b1b87d36b8f1d486dde7adbb2f126d2bfc63856. Preserve all command, profile, stopped-state, execution, host-protocol, SQLite, canary, provenance and cleanup records in that directory.

The exclusive attempt record remains .worktrees/linux-containment/.local-state/linux-containment-native-attempt.json. Source SHA-256 6f23372d93ffc6d552fc2718ed240da81dd82cac17b859ea0c46f2ae31ca4bc9; payload d85b2cc2f8b6e284b5f2fff54ed52125a53e0d1a0843b6f9454b1c90cd71998a; actual engine binary 6b0ff07c81814d8d70dd98081e70986d5ef71c67bcfe13a07a56bbc417001104, bound to the worker's successful make build at source 57499cb. This does not claim the later coordination core was the binary used in this experiment.

Eighteen leader-native helper tests passed before the experiment, including real process deadlines/output, engine protocol/SQLite and exact owned-socket cleanup/replacement refusal. Log: .worktrees/linux-containment/.local-state/linux-containment-leader-helper-tests-02.log, SHA-256 8cbd510a4ed6c6c5455cf7cf7f95f90e7e89f09ffa343e63e9a8a4748a282b0f. The original cleanup failure and subsequent red/green evidence remain retained. Helper checks are not Docker containment proof.

## Next boundary

No automatic rerun or remaining case launch is authorized by this failed attempt. Preserve the source and receipts. A follow-up must first retain topology observations before validation, present the exact bounded diagnostic and obtain any additional experiment authority required by DEC-071. Do not change Docker settings, file sharing, network policy or resource limits to obtain a pass. Core implementation can continue independently.

Aggregate CPU-time budgets, disk quota, full Task 2 admission, native harness compatibility/authentication, individual native child controls, live-worker recovery, any worker-engine bridge and arbitrary escape resistance remain unqualified. Reviews are deferred under DEC-061.
