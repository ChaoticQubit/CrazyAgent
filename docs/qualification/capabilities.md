# Qualification and admission

Status: implementation in progress. No native execution profile is admitted by the repository foundation or by an admission unit test.

## Required evidence

A qualification report identifies its source revision, execution profile, observed tool versions, per-capability outcomes, retained artifact references and explicit gaps. Admission must validate the expected source and profile, required proven outcomes and intact evidence. An unavailable result is not a pass.

Artifact paths must stay inside the designated evidence directory after resolving filesystem paths, including symlinks. Artifact bytes must match their recorded SHA-256. Missing, changed or escaping evidence prevents admission. A report supplied by an untrusted coding worker does not become an authoritative verification receipt merely because its JSON and hashes are internally consistent.

## Native capability status

| Capability | Required proof | Current status |
|---|---|---|
| Confinement | Permitted candidate writes succeed; forbidden host/trusted writes, symlink escapes and network actions fail; child restrictions and resource limits are measured | Unavailable; real runtime probes have not run |
| Live recovery | The original live worker survives an engine-only crash and reattaches without duplicate dispatch | Unavailable; runner implementation and native evidence are pending |
| Native authentication | The chosen account/API/local-endpoint mode works inside the admitted profile without exposing credentials | Unavailable; no provider authentication qualification has run |
| Native child control | Stable child identity, state, targeted cancellation and recovery work through the chosen native harness | Unavailable; adapter implementation and native evidence are pending |

These capabilities are qualified per source revision, profile, operating system, harness and relevant authentication mode. An installed executable, help response, successful build or other profile's result cannot establish them.

## Evidence boundaries

Keep raw local evidence under private ignored storage. Publish sanitized summaries with enough provenance to locate and verify the authorized local evidence. Do not commit credentials, raw provider transcripts or private host paths as test artifacts.

Unit tests exercise real admission functions using controlled inputs and actual temporary files. CLI tests execute the actual application binary. Those checks establish admission behavior; they do not prove native confinement, provider behavior, crash recovery or child control. Native checks remain explicitly unavailable until the corresponding real probes succeed.

The current owner-directed development phase defers code and security reviews. No review result or native capability is implied by that deferral. The accepted product verification requirements remain in force.

## Delivery checkpoints

The repository foundation supports offline formatting, linting and test/build/test through make check. The empty scaffold had zero application tests. Task 1 adds the real qualification/admission behavior and CLI; subsequent tasks must provide the runtime evidence that can justify an admitted workflow.
