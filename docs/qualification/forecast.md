# Initial V1 qualification forecast

Date: October 17, 2026. Status: early checkpoint; complete V1 remains unqualified. This is not a claim that sixteen measured working hours have elapsed. The repository does not contain a reliable cumulative working-time ledger for the earlier implementation sessions.

## Frozen inputs

The admission checkpoint uses source revision `7cfcb66b10cc415ff5d8e0452f2ed27c9c05ca6e`. Product source is the verified local-main checkpoint `448a52e`; the subsequent commit only records isolated worker scope. Engine source lineage includes `4919e1d` and `72d5b5d`; resource prototype source is `f285a2b`. Later implementation does not retroactively change these observations.

The integrated native Rust gate completed formatting, strict Clippy, 49 tests, a build and the same 49 tests again, with zero failures and zero ignored tests. Its log is `.local-state/integrated-engine-check-02.log`, SHA-256 `445110f4c9fcd0811ab877965e4375bfc1452a93cc09b1c9eb9d39d8504ab031`. The actual disconnected-peer regression, original process replay test and real SQLite/socket checks are part of this evidence. This establishes the tested durable core, not native model execution.

The integrated resource prototype completed five checks and six real sequential disposable containers in 21.054 seconds. Report `.local-state/container-probe-tests/cc982da2309c4153aac2745a4b02f292/report.json` has SHA-256 `ac32c2af3c0799fc38852f908c9318b1d1b6f314e2c3f36ba65dfdd0ff35`. Exact-container cleanup was confirmed. The report explicitly leaves native admission, an aggregate CPU-time budget and permanent runtime selection false. See [the measured resource results](container-resource-results.md).

## Actual admission results

The real CLI generated reports and attempted admission for three named launch-profile candidates. Each report command returned zero; each admission command returned one and listed all four missing mandatory capabilities. The report command records unavailable evidence and performs no provider execution. These are candidate identifiers, not configured or admitted accounts.

| Candidate profile | Confinement | Live recovery | Native authentication | Individual child control | Admission |
| --- | --- | --- | --- | --- | --- |
| `macos-native-claude-code` | Unavailable | Unavailable | Unavailable | Unavailable | Rejected |
| `macos-native-codex` | Unavailable | Unavailable | Unavailable | Unavailable | Rejected |
| `macos-rcli-leader` | Unavailable | Unavailable | Unavailable | Unavailable | Rejected |

Artifacts are retained in `.local-state/qualification-checkpoint-20261017/`. Report hashes are `afca3de4e7d2bf83ba991da5789528d971751fe90c9eae0797901c3e91fbb99f`, `08fe1cd985f7c19276b12a3a8a5d99aa2e72e4c79347a3183fac3fd2abde5c0f`, and `7778bd62f29fb9e0c362b6b11b05421b06f6d593ee76e1a5cd8579119e2fade0`, respectively. All three admission logs contain the same explicit rejection and have SHA-256 `635c3b3dffcc27ad8c36911628befa948dcb8686e7750ec15f4006e7d407d055`.

The admission invocation was `target/debug/crazyagent qualification admit --report PATH --source-revision 7cfcb66b10cc415ff5d8e0452f2ed27c9c05ca6e --profile-id PROFILE --evidence-dir .local-state/qualification-checkpoint-20261017 --require confinement --require live_recovery --require native_auth --require child_control`. No failed capability was removed from the required set.

## Capacity and forecast

The owner-approved capacity is 30 to 40 hours per week with a 20 percent reserve. October 17 to November 30 spans 44 calendar days, approximately 6.29 weeks. That corresponds to approximately 189 to 251 gross hours, or 151 to 201 planned hours after reserve, before subtracting actual time already spent during that interval. This is calendar arithmetic, not a measured remaining-effort estimate or an assumed work schedule.

The durable core is verified. Task worktree ownership and the actual worker-to-engine denial proof are in progress in separate visible native worktrees. Surviving native execution, profile authentication and child control still require real qualification. Coordination, knowledge, protected verification, both complete clients, end-to-end workflows, ten-session qualification and release packaging remain substantial downstream work. Code and security reviews are deferred by DEC-061 and must not appear as passed.

There is no evidence-supported date for complete V1 yet. November 30 remains a conditional target. Finishing implementation of control-plane modules cannot replace the missing runtime/profile proof. A revised delivery forecast must use the outcome of the next bounded qualification allocation and observed implementation throughput, while retaining the reserve and accepted scope.

## Next bounded allocation and decision boundary

Continue the already-authorized task worktree ownership slice and actual sandboxed worker-to-engine proof. These do not admit native execution or require provider spending. Preserve real red/green results, explicit infrastructure failures and function-level decisions before integration.

The concrete replacement candidate for the missing macOS resource boundary is a Linux worker backend inside the existing Docker Desktop VM, with the authoritative Rust engine outside the worker boundary, an immutable pinned image, narrowly selected project data, independent resource limits, and explicit lifecycle reconciliation. The existing prototype proves only the listed resource controls. It has not proved the complete filesystem, IPC, credentials, provider, recovery or child-control contract. A Linux CLI environment also changes the host-native compatibility assumption and introduces a Docker Desktop prerequisite.

Before extending DEC-069 or selecting that backend, prepare a reviewable follow-up proposal with exact mounts, networking, images, processes, cleanup, resource ceilings, elapsed-time bounds and compatibility effects. Obtain the owner's decision before permanent adoption, host privilege, larger prototype authority or external spending. Vercel Sandbox remains a future optional backend under DEC-070; this checkpoint does not pull it into V1.

Profile-connected tests separately require approved account/destination references and bounded usage. Do not read credentials or make a model call to discover that authority. Source/build/dependency preparation may proceed within DEC-068 while model-connected qualification remains unavailable.
