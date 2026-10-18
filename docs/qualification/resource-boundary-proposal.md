# Resource boundary replacement prototype

Date: October 17, 2026. Status: prototype authorized by the owner in DEC-069; permanent runtime not selected. Task 2 remains unqualified. Native provider/account calls remain outside this proposal.

## Evidence requiring a decision

The actual sandbox-runtime 0.0.80 probes passed candidate/child writes and denied tested trusted, traversal, symlink, synthetic credential and TCP loopback access. The leader's unchanged native `make check` exited 0: strict format/lint, 17 library plus 9 integration tests, build and the same suites again, with zero ignored tests. Passing observation tests do not qualify missing controls.

The tested Darwin memory setters returned EINVAL at 16 and 64 MiB. A per-user process limit of four prevented even the first child while the unrestricted control started six. A fixed Rust CPU role inherited a two-second per-process limit and terminated with SIGXCPU; this is not a task-wide CPU budget and was not applied to sandbox-runtime. The full profile remains unavailable.

Read-only discovery found an existing Docker Desktop daemon: client/server 29.8.2, Linux, cgroup v2, 12 CPUs and 16,745,562,112 bytes of VM memory. No daemon setting, container, image or unrelated project was changed. Reuse this existing Linux VM for a bounded alternative prototype before considering another VM installation.

## Concrete prototype

Acquire only the official Python Linux arm64 image by immutable manifest `python@sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9`. Docker Hub metadata for `3.14-alpine` reports approximately 21,105,475 compressed bytes and index digest `sha256:f6a589d43c42b9e7f7dc67a12d37132491f362859a5d750607710cc56da3bc72`. Record the inspected platform, manifest and resulting image identity before execution. Do not use a floating tag at runtime.

The leader owns one uniquely named disposable container at a time. Each probe has a 15-second external deadline, at most 128 MiB memory with swap capped at the same value, 0.5 CPU and 16 PIDs; lower tested limits are allowed within these caps. Use no network, no credentials, no provider endpoints, no privileged mode, no added capabilities, no host sockets, no host PID namespace and no daemon configuration changes. Drop capabilities, set no-new-privileges, use a read-only root filesystem and a bounded disposable tmpfs. Mount only dedicated nonsecret candidate/trusted fixtures if a specific filesystem test requires them. Never mount the Docker socket or arbitrary host directories.

Test actual memory exhaustion, PID exhaustion, CPU throttling and successful bounded controls; retain kernel/cgroup counters and observed outcomes. CPU rate throttling, total CPU-time budgets and wall deadlines are different controls and must be reported separately. The prototype cannot silently replace an aggregate CPU-time requirement with a CPU rate.

Remove only containers created and identity-checked by this prototype. Retain image/artifact provenance; do not prune images, stop other containers or change the existing Docker Desktop resource allocation. A failed launch or unavailable controller is a failed/unavailable experiment, not permission to broaden privileges.

## Decision and tradeoffs

Recommended decision: authorize this bounded replacement prototype without adopting Docker as the permanent launch runtime. Initial enforcement probes are estimated at 30-60 minutes; product integration and qualification are estimated at 2-4 engineering days. The native authentication, persistent control, recovery, filesystem/network boundary and packaging checks are additional work and remain unproven.

Benefit: an existing VM and cgroup v2 may supply task-scoped resource controls that the measured macOS setters did not. Cost: Docker Desktop becomes a prototype prerequisite, workers execute Linux binaries, account portability and repository mounts need fresh qualification, and VM memory/startup overhead affects the product. Any permanent runtime choice needs a separate recorded decision after evidence.

Alternative: continue investigating a macOS-native hard resource boundary. It preserves native process compatibility but currently has no demonstrated task-local memory/process mechanism and no reliable completion estimate. Ordinary scheduling/usage monitoring does not satisfy the existing hard-boundary requirement.

## Primary sources

[Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/) describes memory caps and cgroup CPU-rate controls. [Docker rootless constraints](https://docs.docker.com/engine/security/rootless/) distinguishes controller prerequisites. [Official Python image metadata](https://hub.docker.com/v2/repositories/library/python/tags/3.14-alpine) supplied the pinned platform digest and observed size. Documentation supports candidate selection; only the actual local probes can establish enforcement here.
