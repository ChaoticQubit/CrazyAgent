# DEC-069 follow-up proposal: contained Linux worker with the Rust engine on macOS

Date: 2026-10-18. Status: proposal only, not executed or authorized by this document. Task: remaining Task 2 qualification gap. Resource owner: the leader, exclusively. No source changes or reviews are proposed in this slice.

## Decision recommendation

Approve one 20-minute, synthetic-only containment experiment using the existing Docker Desktop and the already acquired immutable Python image. Run at most five disposable containers, strictly sequentially, with the actual existing Rust engine remaining on the host. Measure filesystem confinement, absence of host IPC/process exposure, network isolation and resource enforcement together. Retain each failed or unavailable result. Do not select a product runtime or claim full Task 2 admission from this experiment.

The retained leader-native worker/engine test passed and integrated make check passed 50 tests, build, then 50 tests at a88573d. Those results belong to the existing host-native profile and must remain separate from this proposed Docker experiment. This proposal does not reverify those results or combine different profiles into one qualified profile.

## Fixed prerequisites and authority

Use only local image python@sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9, platform linux/arm64. Record RepoDigests, resolved image ID and platform before creating anything. Require the digest already present; use --pull=never. A missing image, unavailable Docker Desktop or unsupported option ends the experiment as unavailable. No image pull, build, package installation, VM startup/configuration change or replacement image is included.

The leader alone may use /usr/local/bin/docker with the existing project-local configuration /Users/chaoticqubit/Projects/CrazyAgent/.local-state/tools/docker-config and explicit existing endpoint unix:///Users/chaoticqubit/.docker/run/docker.sock. This is host-controller access to Docker Desktop, never a socket or authority passed into the worker. Require the local Docker configuration to contain no auths, credential helpers, proxies or remote context selection; otherwise stop without reading credential values or changing configuration. Clear inherited environment and use absolute executable paths. Record versions and selected endpoint without inspecting other containers or real account stores.

Use the existing Rust engine binary associated with the leader's a88573d build evidence, recording its SHA-256 and that evidence binding. If that binding is missing, report it rather than rebuilding or assuming identity. No providers, accounts, harness launches, real repositories or credentials are included. Only the disposable engine and the named experiment containers may be controlled. No Docker inspection, exec, signals, stop, cleanup or resource changes may target unrelated workloads.

## Exact synthetic fixture layout and mounts

Choose a fresh run ID of 32 lowercase hexadecimal characters. Let R be /private/tmp/ca-q2-<run-id>. Create it and the following directories privately; all fixture data is generated for this run. Verify canonical paths, ownership and absence of pre-existing entries or symlinks before use. Never mount R or any ancestor. The short canonical root keeps the real macOS Unix socket path below its platform length limit. Retain the evidence path in the project ledger. If Docker Desktop cannot bind these private temporary fixtures under its existing file-sharing configuration, stop as unavailable; do not change file-sharing settings.

| Host path under R | Exact contents and use | Container destination |
|---|---|---|
| candidate/<case> | Empty directory for that one case, then at most eight synthetic files of at most 64 KiB each; worker and child write distinct marker files. No checkout, Git metadata, hooks or sockets. | /workspace, read/write, only this case directory |
| protected | sentinel.txt containing synthetic-protected:<run-id> followed by LF; config.json containing only run_id and policy=fixture-only. Hash and stat both before and after. These are canaries, not engine state or credentials. | /protected, read-only |
| payload | A planned standard-library-only probe.py and manifest.json, together at most 64 KiB. Manifest contains the run ID, selected case, expected canary hashes and unmounted synthetic engine path. No secrets or real PID values. Hash before launch. | /probe, read-only |
| host-engine | Actual engine.db, its SQLite sidecars, private engine.sock and synthetic-only engine configuration/state. Mode 0700 directory and normal private engine socket permissions. | No mount, symlink exposure, copy, relay, proxy, published endpoint or volume |
| evidence | Host-owned provenance, bounded stdout/stderr, inspect summaries, cgroup observations, host-client replies and cleanup receipts. | No mount |

The engine stays a normal host child: existing crazyagent engine R/host-engine R/host-engine/global.toml R/host-engine/project.toml engine.sock. The two TOML files are initially empty, selecting existing defaults. A host-only standard-library client sends the actual four-byte big-endian length-prefixed JSON WireRequest protocol. First send Snapshot with protocol_version=1, client_id=followup-host, request_id=host-before and expected_revision="0". Then send CreateProject for project_id=synthetic-project, spec={name: Synthetic containment fixture, repositories: []}, using the actual snapshot revision and request_id=host-create; require Applied. Repeat host-only Snapshot and inspect actual persisted projects/operation IDs after every case. The engine socket never enters a container. Do not replay a provider or fabricate an engine reply.

The controller uses the existing host Python standard library for supervision, IPC and SQLite observations. Probe/controller scripts are future experiment artifacts under R only; this proposal adds none. Host client frames and replies are bounded to the current 65536-byte protocol limit plus the four-byte prefix, with a one-second socket deadline. Host-engine readiness is bounded to five seconds. Failed positive controls stop further launches.

## Container configuration and verified limits

Every case uses 64 MiB memory, 64 MiB memory-plus-swap, 0.5 CPU, 16 PIDs maximum and an external lifetime deadline of at most 15 seconds. These are below or equal to DEC-069's 128 MiB / 0.5 CPU / 16 PIDs / 15 seconds ceilings. Do not raise limits to obtain a pass. Use the host fixture owner's nonzero UID/GID; if the controller is root, stop. No user-namespace, ownership, permission or Docker configuration changes are permitted to repair a mount failure.

The planned create arguments are shown below as an argument-vector template, not a command to run now. R, CASE, RUN_ID, HOST_UID and HOST_GID are verified fixture values, not interpolated untrusted shell input. Container name is ca-q2-<run-id>-<case>; cidfile is R/evidence/<case>.cid. The case set is exactly boundary, memory, pids, cpu, deadline.

```text
/usr/local/bin/docker
--config /Users/chaoticqubit/Projects/CrazyAgent/.local-state/tools/docker-config
--host unix:///Users/chaoticqubit/.docker/run/docker.sock
create
--name ca-q2-RUN_ID-CASE
--label io.crazyagent.runtime-followup=RUN_ID
--label io.crazyagent.runtime-followup.case=CASE
--cidfile R/evidence/CASE.cid
--pull=never
--platform=linux/arm64
--memory=64m
--memory-swap=64m
--cpus=0.5
--pids-limit=16
--network=none
--ipc=none
--cgroupns=private
--read-only
--cap-drop=ALL
--security-opt=no-new-privileges=true
--user HOST_UID:HOST_GID
--ulimit core=0:0
--ulimit fsize=65536:65536
--ulimit nofile=64:64
--tmpfs /tmp:rw,noexec,nosuid,nodev,size=8m,mode=1777
--mount type=bind,src=R/candidate/CASE,dst=/workspace
--mount type=bind,src=R/protected,dst=/protected,readonly
--mount type=bind,src=R/payload,dst=/probe,readonly
--workdir=/workspace
--log-driver=none
--entrypoint=/usr/local/bin/python3
python@sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9
-I
-S
-B
-u
/probe/probe.py
CASE
```

Keep Docker's default seccomp policy. Do not use privileged mode, added capabilities, devices, ports, host networking, host/shared PID or IPC namespaces, cgroup-parent changes, custom runtimes, extra mounts or writable container root. Do not pass host environment, SSH agents, account/configuration directories, engine/verifier directories or Docker/engine sockets. An image's minimal intrinsic environment is not imported account authority.

Before starting, inspect only the captured container ID. Require the exact run labels, name, resolved image ID, numeric user, three declared bind mounts, one bounded /tmp tmpfs, no published ports, network none, IPC none, private/default PID mode, private cgroup namespace, read-only root, empty effective capabilities and no-new-privileges. Require memory=67108864, memory-swap=67108864, NanoCpus=500000000 and PidsLimit=16. Reject a container that does not match; never start it or broaden authority. Inside a started case, also record cgroup v2 memory.max, memory.swap.max, pids.max, cpu.max, counters and /proc/self/status. Require memory.max=67108864, memory.swap.max=0, pids.max=16 and a finite cpu.max quota/period ratio no greater than 0.5. Missing or contradictory controls are unavailable/failed, not inferred from CLI arguments.

## Five sequential cases and honest positive/negative controls

| Case | Positive controls | Negative controls and required observations |
|---|---|---|
| boundary: filesystem | Worker and one child read both synthetic canaries, create distinct exact-content files in /workspace, and create a small /tmp file. Host verifies candidate contents and unchanged protected hashes. | Worker and child each attempt direct /protected/sentinel.txt overwrite, /workspace/../protected/sentinel.txt traversal and a /workspace/escape symlink targeting /protected/sentinel.txt. Require EROFS/EACCES/EPERM and unchanged canaries. Attempt writes to the read-only image and /probe payload; retain exact errno and distinguish filesystem read-only from UID permissions. The actual host-engine absolute path is unmounted: lstat must return ENOENT, and a candidate symlink to that unmounted path must not expose it. ENOENT is recorded only as non-exposure, never permission-denial proof. |
| boundary: network | A fixture-owned TCP listener on container loopback accepts worker and child challenges and returns the run nonce. Both socket creation and useful local traffic must work. | First require loopback-only interfaces and no default route; otherwise abort before any destination attempt. Worker and child each make one 250 ms TCP connect attempt to documentation-only address 192.0.2.1:9 and record the OS result. Require ENETUNREACH/EHOSTUNREACH for this planned topology; timeout is inconclusive and any connection is failure. No DNS, provider, real host listener, host.docker.internal lookup, bridge peer or internet endpoint is contacted. |
| boundary: IPC/process | A fixture-owned AF_UNIX socket in /tmp and an anonymous pipe carry worker/child nonce exchanges. The controller's host-only engine request still succeeds. One owned container child is stopped, continued and reaped using its fork-returned PID; bounded child creation works. | Require IPC none and no /dev/shm mount, no exposed engine/Docker socket paths and no writable cgroup hierarchy. Container /proc must contain only its PID 1 worker and recorded descendants at the sampling point; unexpected processes invalidate this fixture. A connect to the unmounted synthetic host engine path must yield ENOENT; this establishes non-exposure only. Never copy or signal a macOS host PID inside Linux: numeric PID collisions could target something else. Host-side queries confirm the disposable engine remains responsive and only expected host-generated project mutations exist. These are namespace/topology checks, not a demonstrated signal-denial syscall against macOS. |
| memory and pids: two separate containers | Memory case touches/releases 8 MiB before its negative test. PID case creates/reaps two known children before exhaustion. Capture applicable cgroup counters before and after each phase. | Memory case uses one child to touch up to 96 MiB under the unchanged 64 MiB cap; require an actual increased memory.events oom_kill counter with the owned child's termination, or verified container OOMKilled state if the observer also dies. A plain allocation exception/exit 137 alone is insufficient. PID case attempts at most 18 forks while holding only its own children; require EAGAIN and increased pids.events max, then release/reap every successful child. No root, fork bomb, other UID or other workload is used. |
| cpu and deadline: two separate containers | CPU case records a short idle sample, then two seconds of bounded busy work under the same cpu.max; require progress, actual usage delta and increased throttling counters. Deadline case prints a readiness nonce and starts one owned sleeping child. | CPU case attempts to open/write its own cgroup cpu.max unchanged value and requires denial; retain throttled_usec/nr_throttled deltas without claiming a total CPU budget. Deadline case deliberately exceeds the planned run time; host watchdog requests KILL at 12 seconds measured before the start request, requires verified non-running state within the remaining three seconds, and verifies owned descendants are gone. API latency, missing counter support or delayed kill is failure/unavailable; no timeout extension or second container is permitted. |

The three boundary rows are phases in one boundary container, not three containers. Memory, pids, cpu and deadline are four additional containers: five total maximum. Every case runs under the same bounded profile and mounts, with an empty per-case workspace. Each controlled child has an internal deadline shorter than its parent and is reaped; boundary operations have a combined internal deadline of eight seconds. The boundary case allocates no more than four simultaneously live processes, two listeners and eight small files. PID exhaustion is the only case permitted to reach the 16-PID ceiling.

Do not turn a failed positive control into a negative pass. Neither absent host paths nor missing /dev/shm prove a kernel access-denial policy. Do not create a second container, sidecar service or Linux VM process to manufacture a stronger cross-workload control. A failed boundary/control or resource result ends subsequent launches, retains observations and uses the remaining allocation for cleanup/reporting. No repeat-until-green or automatic retries are proposed.

## Timing, output limits and exact cleanup

The absolute allocation is 1200 monotonic seconds, including setup, execution, evidence and cleanup: minutes 0-3 for identity/configuration checks and host engine controls; minutes 3-8 for synthetic fixtures and the boundary case; minutes 8-13 for the four resource cases; minutes 13-17 for host-state/canary comparison and evidence accounting; minutes 17-20 reserved for final cleanup and the report. End early on failure. Do not start a container after minute 13, without at least 15 seconds of run/kill allowance and all reserved cleanup time remaining, or while an earlier container's non-running/removal state is unverified. Unfinished work at minute 20 is reported, not extended.

Each Docker API operation has a three-second host subprocess deadline bounded again by the overall deadline. Starting a container also starts its absolute lifetime watchdog before the start request; request kill at 12 seconds and verify termination by 15. Use attached output streams drained concurrently by the host controller with logging disabled, at most 32 KiB stdout and 32 KiB stderr per container; terminate the owned container on overflow. Apply the same 32 KiB per-stream cap to host-engine logs and per-command Docker metadata. Keep at most 4 MiB total retained fixture/evidence data, excluding the already-existing image/binary. The planned payload writes at most 512 KiB into each bind-mounted candidate; RLIMIT_FSIZE caps each regular file at 64 KiB and the host controller monitors the fixture total. This is a bounded synthetic workload, not a disk-quota qualification: bind mounts have no claimed aggregate disk quota. Record truncation/overflow as incomplete evidence, never success.

Cleanup follows these bounded actions for each case, including failure, interruption or watchdog expiration:

1. Take the exact CID only from this run's create result/cidfile. With the host Docker prefix above, inspect that CID and verify both run/case labels, exact name and expected image ID against the pre-start receipt. Never select by a broad name or label match alone. Identity mismatch stops mutation and is reported.
2. For that verified running CID only, issue docker kill --signal KILL CID. If it is already stopped, do not signal it. Inspect the same CID and retain bounded exit/OOM/finish state. Verify non-running state; if Docker is unresponsive, launch nothing else and report the residual CID and unverified deadline. Do not claim cleanup succeeded.
3. Issue docker rm CID only after verified non-running state. Require a subsequent inspect of that exact CID to return the specific no-such-container result. Authentication/daemon errors are not absence. Remove its cidfile only after this receipt. Never use prune, rm -f, wildcard deletion, image removal or commands against other containers/networks/volumes.
4. After the final case or an early stop, perform the host-only final engine Snapshot and persisted-state comparison where available. Close host clients. Terminate only the still-owned engine Child handle, allow one second, then KILL that same owned live child if necessary and wait/reap. Record identity, exit and disappearance of its socket after reap; retain a stale fixture socket as evidence if forced termination left one. Never signal by process-name search or touch another engine.
5. Retain the private R fixture tree, raw artifacts and hashes within the 4 MiB ceiling. Record all owned CIDs as absent or explicitly unresolved, engine reap status, observed descendants, host persisted-state results, unchanged canaries, monotonic elapsed time and every pass/fail/unavailable result. No automatic host directory deletion or unrelated Docker cleanup is authorized.

If an API operation times out after an uncertain create, use only the exact prechosen name and run labels to resolve and identity-check that one possible experiment container; no new container may start until it is accounted for. If the create identity cannot be established safely, stop with an explicit residual-state gap. A controller interruption/daemon outage can defeat a host watchdog; this is a failure to establish the 15-second guarantee, not evidence that Docker enforces wall time itself.

## What this experiment cannot prove

It cannot prove an aggregate task CPU-time budget. cpu.max/--cpus limits CPU rate, while the external watchdog limits elapsed time; neither is a kernel-enforced total CPU-seconds budget across the task's lifetime or restarts. Even a finite rate times a planned deadline is not independent cumulative CPU accounting, and a delayed/unavailable host controller weakens that estimate. Record observed usage only; total CPU-budget enforcement remains unavailable.

It cannot prove native Claude Code, Codex or RCLI compatibility, macOS-specific execution, native authentication, approvals, protected account storage, child controls, live-worker recovery or TUI/desktop completeness. The pinned Linux Python image deliberately contains no native harness or credentials. No providers are contacted. Docker Desktop resource use, file sharing and Linux semantics remain product dependency/compatibility questions.

The host Rust engine is real and checked independently, but intentionally unreachable from the container. This does not exercise an authorized worker-engine bridge, permission revocation over such a bridge, same-UID macOS signal denial, a mounted host Unix socket, or a real container-originated engine mutation attempt over an exposed endpoint. The canary read-only mount is not a mounted live engine database. Passing non-exposure checks does not qualify any future transport or justify exposing the Docker socket. Keeping engine policy on the host does not by itself establish the full sandbox contract.

It also cannot prove arbitrary-code escape resistance, hypervisor/kernel security, cross-container isolation against other workloads, VM-wide performance, aggregate disk/IO budgets, concurrency, release packaging, credential safety or independent verification integrity. It is a finite synthetic probe under one immutable image/profile; inherited descendants must be observed but a finite pass does not establish all possible attacks. Review remains deferred, not passed.

The outcome is a profile-bound evidence ledger with exact image/binary/payload hashes, settings, counters, timings, cleanup receipts and explicit gaps. Any successful subset remains separate from the native profile's reported passes. Permanent Docker adoption and full Task 2 admission require their own later evidence-based owner decision.

## Reference basis

Local authority: docs/agent-decisions.md DEC-069, docs/qualification/resource-boundary-proposal.md and scripts/qualification/container_resources.py. The image identity above is taken from that accepted local record, not a fresh pull or image inspection in this turn.

Docker documents that [network none supplies only loopback](https://docs.docker.com/engine/network/drivers/none/), and that [IPC none supplies a private IPC namespace without /dev/shm; run options also define read-only root and mount controls](https://docs.docker.com/reference/cli/docker/container/run/). Its [resource documentation distinguishes memory configuration and CPU rate controls](https://docs.docker.com/engine/containers/resource_constraints/). Those documented settings guide this proposal; actual local enforcement remains to be measured.
