# Bounded container resource results

Date: October 17, 2026. Authority: DEC-069. Status: bounded development prototype passed; no native profile or permanent runtime selected.

The leader used the installed Docker Desktop VM and official image python@sha256:aae362254cea6b75fe812f0b428f10b3b361a090fe5655fb1d03937f6d38cda9. Each container used 64 MiB RAM, no swap, 0.5 CPU, at most 16 PIDs, an unprivileged user, read-only root, no network, no added capabilities, no mounts and no host socket. Only one identified disposable container ran at a time.

| Check | Actual result |
| --- | --- |
| Memory | The 8 MiB control succeeded; 96 MiB allocation under 64 MiB ended with OOMKilled evidence. |
| Processes | Six children succeeded under 16 PIDs. Four PIDs allowed three children and denied three with EAGAIN and three kernel limit events. |
| CPU rate | The 3.5-second workload consumed 1,748,971 microseconds CPU with 35 throttled periods under cpu.max 50000 100000. This is not a total CPU-time budget. |
| Deadline | Actual running duration was 14.019154243 seconds, below 15 seconds. |
| Cleanup | All six exact identities were stopped, inspected, removed and confirmed absent. |

Five Python checks passed in 21.054 seconds. Evidence: .local-state/integrated-container-check.log and .local-state/container-probe-tests/cc982da2309c4153aac2745a4b02f292/report.json. Report SHA-256: ac32c2af3c0799fc38852f908c9318b1d1b6f314e2c3f36ba65dfdd0ff35. Source hashes and original parser red/green evidence are retained in IMP-038 through IMP-043.

Filesystem/credential exposure, allowed network destinations, engine endpoint denial, supervisor crash recovery, native CLI compatibility, authentication, child lifecycle, aggregate CPU-time budgets and packaging remain unqualified. Docker Desktop is a prototype prerequisite. These checks do not authorize permanent adoption or provider calls.
