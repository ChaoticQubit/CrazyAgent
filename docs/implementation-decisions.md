# Implementation decisions

This log records architecture and function-level implementation rationale. The owner/product authority register is docs/agent-decisions.md. Stable IMP IDs survive revisions; superseded decisions name their replacements. Each record distinguishes observed evidence from retrospective inference.

## Record contract

Each meaningful decision records date/status and task, affected files/symbols, the problem and constraints, the chosen approach and rationale, alternatives and their pros/cons/costs, verification evidence, risks and revisit conditions. A symbol coverage index maps every production function to relevant decisions. One decision may cover several functions when they share the same rationale.

Workers record new choices as they make them in their own .local-state/implementation-decisions.md. The leader consolidates those records here before integrating each task. The documentation worker initially owns this file for Task 1; the leader resumes ownership when that worker finishes.

## IMP-001: Separate product authority from implementation rationale

Date: October 17, 2026. Status: accepted implementation workflow under DEC-067. Task: continued V1 implementation. Scope: docs/agent-decisions.md, this file and worker briefs.

Problem: Product approvals and detailed function design have different evidence and change frequencies. A single undifferentiated chat history loses both.

Decision and rationale: Retain numbered DEC records for owner/product authority and maintain IMP records here for technical choices. Preserve cross-links, stable identifiers and a production-symbol coverage index. Label reconstructed Task 1 rationale as retrospective wherever contemporaneous intent was not recorded.

Alternatives and tradeoffs: One combined register has fewer files but makes detailed implementation changes obscure product authority. Code comments place rationale near statements but violate the owner's no-comment rule and distribute context across files. Unstructured chat is cheap initially but expensive to recover and cannot prove a decision's current status. The chosen split costs maintenance of links and the coverage index.

Verification and revisit: The leader checks the record and symbol map before task integration. Revisit layout if the log becomes difficult to navigate, preserving IDs and history rather than deleting records.

## IMP-002: Keep unqualified native workflows unavailable while building independent code

Date: October 17, 2026. Status: implementation boundary under DEC-059, DEC-062 and DEC-066. Task: native qualification. Scope: qualification admission, execution boundary and subsequent independent engine work.

Decision and rationale: Continue real source work and tests using existing tools while resolving missing runtime acquisition and account authority. Do not turn missing dependencies, synthetic capability claims or skipped native tests into passes. Run actual native probes after their prerequisites and authority exist.

Alternatives and tradeoffs: Stopping all source work wastes independent development capacity. Treating a mocked runtime as qualified gives faster green output but loses the accepted security and recovery guarantee. The chosen approach requires explicit separation of implemented behavior, native evidence and remaining gaps, and may expose a schedule change when qualification fails.

Verification and revisit: Existing Task 1 tests establish admission behavior only; native capability outcomes remain unavailable. Revisit the runtime choice on concrete failed boundary evidence rather than weakening launch requirements silently.

## Task 1 scope and evidence provenance

Date: October 17, 2026. Task: implementation plan Task 1, qualification admission and evidence contract. Documentation baseline: `ccbad41f3034471acfda685ce7660637ec55e59e` on `docs/implementation-decisions`.

The production files and tests described below are byte-identical to source commit `37ead0f`, integrated into trunk as `d1aa990`. The source baseline is independent of later documentation or runtime acquisition. DEC-065 supplies the technical authority, DEC-067 requires this record, and DEC-061 defers code/security reviews. These records document existing behavior; they are not reviews or newly approved product decisions.

Evidence labels used throughout:

| Label | Meaning and source |
| --- | --- |
| Authority | Accepted DEC records and the adopted architecture/implementation plan. |
| Worker record | Contemporaneous `.local-state/qualification-report.md` in the `qualification-admission` worktree. This is local handoff evidence, not a portable tracked artifact. |
| Source observation | Direct inspection of the pinned production implementation and its real tests after querying the shared graph. |
| Retrospective inference | A tradeoff or motivation reconstructed during this documentation task, not an owner statement or historical claim of intent. |
| Historical execution | Commands and outcomes retained in the worker record and DEC-065. No Cargo command, build, native probe or provider call was run by the documentation worker. |

The historical worker record contains command/result summaries, not separate raw command-output files in its `.local-state` directory. DEC-065 independently records the leader's final `make check` result. This task checks source coverage and document consistency; it does not claim a fresh test run or recovery of missing raw transcripts.

The owner's later approval of project-local sandbox-runtime 0.0.80, locked V1 dependencies and disposable local boundary probes permits leader-owned acquisition. It does not qualify any native capability. The documentation worker neither acquires the runtime nor requires its path to finish this record. Earlier acquisition gaps in IMP-002 and DEC-065 describe their historical context; later verified acquisition and probe evidence must be recorded by their owners.

## IMP-003: Keep the qualification contract in one reusable Rust module

Date: October 17, 2026. Status: observed implementation; layering rationale partly retrospective. Task: Task 1. Scope: `Cargo.toml`, `crates/crazyagent/Cargo.toml`, `crates/crazyagent/src/lib.rs`, `qualification.rs` and `src/bin/crazyagent.rs` under that crate.

Problem and constraints: The future engine and the present CLI need the same admission rules. DEC-065 permits established serialization, cryptography and filesystem dependencies. No service, interface framework, provider integration or custom unsafe code is required for this slice.

Decision and rationale: Export `pub mod qualification` from the library and keep CLI invocation/output in the binary. Both crate roots forbid unsafe code. The workspace has one member, resolver 3 and edition 2024; package version, Apache-2.0 license and `publish = false` are inherited. Pin Serde 1.0.229, serde_json 1.0.154, sha2 0.10.9 and rustix 1.1.5; tempfile 3.27.0 is test-only. The worker record directly supports cached dependencies and minimum code; the maintainability benefit of one shared policy implementation is retrospective inference.

Alternatives and costs: Duplicating admission in the CLI initially avoids a module but costs consistency and direct unit access when the engine arrives. A separate service adds deployment, transport and lifecycle costs without a current caller requirement. Handwritten JSON or SHA-256 removes dependencies but creates parser/cryptographic correctness obligations. Depending on native filesystem calls directly would add unsafe integration; rustix provides the needed operations through the accepted dependency. Exact pins and the generated lockfile constrain resolution but require deliberate future updates.

Verification: Historical offline metadata resolved 33 packages without downloads; final `make check` built and tested the workspace. Source observation confirms the sole production module export and both unsafe prohibitions. No production function is defined in `src/lib.rs`; it is accounted for as an export rather than invented as a function.

Risks and revisit: Cargo 1.71.2 cannot parse resolver 3. The worker used installed Cargo/rustc 1.99.1 through a task-local PATH. That successful macOS build does not establish another target's portability or support policy. Revisit module boundaries when an actual engine caller or target requires them, and dependency pins through approved acquisition, never by editing generated Cargo.lock manually.

## IMP-004: Represent capability outcomes explicitly and serialize stable names

Date: October 17, 2026. Status: observed implementation; representation tradeoffs retrospective. Task: Task 1. Scope: `qualification.rs::{Capability, Capability::ALL, Capability::as_str, Outcome, Artifact, QualificationReport}`.

Problem and constraints: Missing evidence, a failed probe and an unavailable prerequisite must not become successful qualification. CLI names and JSON keys must refer to the same capabilities. Report contents remain data, not their own authority.

Decision and rationale: Use four capability variants for confinement, live recovery, native authentication and child control, with snake_case serialization and explicit `as_str` spellings. `ALL` defines deterministic enumeration. `Outcome` distinguishes `Proven`, `Failed` and `Unavailable`; absence from the results map is also nonproof. Store versions/results in ordered maps, artifacts as relative-path/hash records and gaps as a vector. Structured records deny unknown fields. The worker record directly supports the capability map and unavailable distinction; choosing ordered maps for predictable iteration/output is retrospective inference from source.

Alternatives and costs: A boolean loses the difference between failed and unavailable. Free-form capability strings simplify extension but transfer spelling and validity checks to every consumer. An unordered map offers average constant-time access but gives up ordered iteration, with no measured advantage at these small bounds. A serialized admission boolean would be simpler to consume but would let a report claim authority. Strict fields and enums cost explicit schema migration when extending the contract.

Verification: `required_capabilities_fail_closed_individually` tests every capability with absent, failed and unavailable outcomes. The real-binary report test deserializes JSON and checks every `ALL` entry is unavailable. CLI parsing uses `as_str` over `ALL`, connecting invocation spellings to this representation. These are historical passing tests; there is no standalone exhaustive serialization-name test.

Risks and revisit: The enum is the current four-capability contract, not proof that all V1 evidence types exist. Adding a capability requires updating names, requirements, serialization consumers and tests together. A declared `Proven` value alone neither authenticates its producer nor proves the native outcome.

## IMP-005: Bind trusted invocation context separately from report data

Date: October 17, 2026. Status: directly recorded worker decision and DEC-065; constructor details observed. Task: Task 1. Scope: `qualification.rs::{AdmissionContext, AdmissionContext::new, QualificationReport::new, QualificationReport::context}`.

Problem and constraints: The required `admit(report, required)` API must compare report identity against expectations obtained independently from trusted policy. Deserialization must not let report authors supply those expectations themselves.

Decision and rationale: `AdmissionContext::new` owns copies of expected source revision, profile and evidence-root path in private fields. `QualificationReport::new` creates identity plus empty collections and no context. Context is private and skipped by Serde; `load_report` attaches it after parsing. Constructors allocate data and leave validation to the admission boundary, so callers can construct an unavailable/empty report without asserting it is admissible. This is directly supported by the worker design rulings, not inferred product approval.

Alternatives and costs: Expected identity in JSON is cheap but self-authorizing. Passing context as a third admission argument is explicit but changes the approved Task 1 signature. Global context preserves that signature but adds hidden mutable state and cross-profile coupling. A fully validated constructor could prevent empty states but would conflate recording unavailable evidence with admitting it. The selected design costs a load/bind step and requires callers to distinguish parsed data from trusted evidence.

Verification: `serialized_report_cannot_supply_admission_authority` serializes a bound report, deserializes it directly, then observes admission rejection and absence of `evidence_root` in JSON. Other admission fixtures use real serialization and `load_report` to bind context. `QualificationReport::new` is also exercised by the actual report command.

Risks and revisit: The context does not authenticate the human, engine or evidence producer. In-process callers remain responsible for obtaining expectations from trusted policy. Public report fields remain mutable; admission rechecks them. Revisit integration with protected engine authority rather than weakening the unbound-report rejection.

## IMP-006: Bound report loading before attaching caller authority

Date: October 17, 2026. Status: observed implementation supported by worker details; operation-order rationale retrospective. Task: Task 1. Scope: `qualification.rs::load_report` and `REPORT_BYTES_MAX`.

Problem and constraints: An untrusted report path may identify a nonregular file, a symlink, oversized JSON or malformed data. Loading must consume bounded input and preserve ordinary error reporting.

Decision and rationale: Open read-only with close-on-exec, no-follow and nonblocking flags. Require a regular file of at most 1 MiB by metadata, then independently read at most the limit plus one byte and reject excess. Parse via Serde and attach the caller-provided context only after successful parsing. The second size check covers growth after metadata inspection; this explanation follows the source rather than a contemporaneous design quote. Return `InvalidReport` for open/read/parse failures.

Alternatives and costs: Unbounded `read_to_end` is shorter but removes the allocation ceiling. Metadata alone is cheaper to write but does not bound a later read if bytes grow. A streaming parser reduces the whole-file buffer but adds complexity unnecessary for the current 1 MiB policy. Following the report's final symlink is convenient but admits another pathname indirection. The chosen approach adds explicit OS flags and a bounded buffer, plus a dependency/platform qualification obligation.

Verification: `nested_evidence_is_supported_and_oversized_files_fail` rejects a real oversized report file. `malformed_unknown_and_duplicate_json_never_loads` writes actual malformed/unknown/duplicate-key JSON variants and rejects them. Real-binary tests exercise file loading. Dedicated report symlink, FIFO and concurrent-growth tests were not present in the pinned test set; do not infer their execution from the flags.

Risks and revisit: No-follow here applies to the final report-path component, not a proof about every ancestor of that path. A bounded successful parse is not an admission result. Revisit the bound and file-opening policy when protected engine storage or another platform requires a changed contract.

## IMP-007: Admit only a nonempty required subset with matching identity and intact artifacts

Date: October 17, 2026. Status: directly recorded worker decision and DEC-065; evaluation order observed. Task: Task 1. Scope: `qualification.rs::admit`.

Problem and constraints: Admission must reject missing context, wrong source/profile, missing or nonproven requirements and invalid evidence. Optional unavailable capabilities must not block a different proven requirement. Every listed artifact must be checked.

Decision and rationale: Reject unbound reports first. Require one to four distinct capabilities, validate both trusted and reported identifiers, compare source then profile, and collect every required capability whose outcome is not `Proven`. Only after those checks validate report shape, open the evidence root as a no-follow directory and hash each listed artifact against its stored digest. Return success only after the complete loop. This ordering yields named missing capabilities before missing-artifact errors for the intentionally unavailable CLI report. Subset semantics are directly recorded; this diagnostic-order explanation is retrospective inference.

Alternatives and costs: Requiring all four capabilities for every call is simpler policy but blocks independently qualified subsets. Accepting an empty list makes success vacuous. Stopping at the first missing capability shortens collection work but conceals other unmet requirements. Checking only a selected artifact would reduce I/O but leave other listed evidence unchecked. The current policy costs bounded reads of all artifacts and returns the first structural/evidence failure rather than an exhaustive diagnostic report.

Verification: `missing_evidence_never_admits` covers empty/unavailable reports, wrong identities, tampering and parent traversal. `complete_intact_evidence_admits_required_capabilities` accepts all four synthetic declarations with intact real-file evidence. `required_capabilities_fail_closed_individually` and `optional_unavailable_result_does_not_block_proven_requirement` establish required versus optional behavior. Actual-binary tests repeat matching/mismatching identity and changed-byte behavior. The duplicate-required API branch has no dedicated unit case in this baseline.

Risks and revisit: Admission establishes identity consistency and byte integrity, not producer authentication, artifact-to-capability semantic sufficiency or live native qualification. It does not freeze a file after checking it. Later protected verification must control producers, evidence storage and current-contract acceptance. Revisit those boundaries with engine integration; never relabel synthetic outcomes as native evidence.

## IMP-008: Apply explicit report-shape and text ceilings at the trust boundary

Date: October 17, 2026. Status: DEC-065 resource policy with source-observed details; exact numeric tradeoffs retrospective. Task: Task 1. Scope: `qualification.rs::{valid_text, validate_shape}`, `ARTIFACTS_MAX` and report field collections.

Problem and constraints: Valid JSON can still contain empty identities, duplicate artifact references, malformed hashes or impractical collections. These are untrusted-input failures, so validation returns errors rather than asserting.

Decision and rationale: `valid_text` requires nonblank text, byte length within a caller-selected ceiling and no control characters; it validates without rewriting the value. `validate_shape` requires nonempty version inventory, at most 128 versions/gaps and at most four results; bounds version keys to 256 bytes, values to 1024 and gaps to 4096. Require one to sixteen artifacts, unique relative paths of at most 1024 OS-string bytes and 32 parsed normal components, and exactly 64 lowercase ASCII hexadecimal digest characters. Use a set for duplicate paths. DEC-065 authorizes these ceilings; no benchmark justifies their exact values.

Alternatives and costs: Trusting parser success removes code but accepts meaningless and excessive records. Silent trimming or lowercase conversion makes more inputs acceptable but changes supplied identity/digest representation. A schema-validation dependency centralizes rules but adds machinery for this small typed contract. The chosen rules add branches and a bounded component/set allocation, and require explicit migration if valid future records need larger limits.

Verification: `absent_invalid_or_duplicate_artifacts_and_versions_fail` covers missing versions/artifacts, malformed hash, absent file, duplicate paths, too many artifacts and an empty identifier. Path tests cover absolute/parent paths, symlinks and directories; the nested/oversized test covers a legitimate nested path. The suite does not independently test every numeric ceiling or Unicode/control-character branch. Component rules operate on Rust's parsed path components, not a claim of byte-for-byte lexical normalization.

Risks and revisit: Syntax does not establish truthful version inventory or artifact meaning. Limits constrain resource use but are not measured throughput guarantees. Revisit with representative real evidence and explicit policy changes, preserving rejection behavior and adding the corresponding boundary checks.

## IMP-009: Resolve evidence beneath an opened directory using no-follow descriptors

Date: October 17, 2026. Status: directly described by worker record; alternatives retrospective. Task: Task 1. Scope: `qualification.rs::open_artifact` and evidence-root opening in `admit`.

Problem and constraints: Artifact references are untrusted relative paths. A lexical path check alone does not prevent a symlink from redirecting a later open outside the evidence tree. Nested legitimate evidence must still work without unsafe Rust.

Decision and rationale: Clone the open root handle, iterate at most 32 normal components and open each through rustix `openat` relative to the current directory descriptor. Use read-only, close-on-exec, no-follow and nonblocking flags; require directory semantics for intermediate components. Return the final file handle for regular-file/type/size verification in `hash_file`. This ties later component resolution to opened directories and rejects symlink components rather than canonicalizing and reopening a pathname.

Alternatives and costs: Joining a path to the root is shorter but follows symlinks. Canonicalize-then-open gives a readable containment check but reopens through a pathname after the check. Rejecting all nested paths simplifies resolution but removes a tested legitimate case. Descriptor-relative traversal costs one open per component, descriptor management and platform-specific dependency qualification, bounded by depth rather than an unbounded walk.

Verification: `absolute_parent_symlink_and_directory_artifacts_fail` creates actual outside fixtures, intermediate/final symlinks and a directory; each candidate is rejected. `nested_evidence_is_supported_and_oversized_files_fail` accepts an actual nested regular file. The worker record describes nonblocking FIFO handling, but the pinned tests contain no dedicated FIFO or rename-race case. Do not represent those claims as executed probes.

Risks and revisit: This evidence-file policy is not an execution sandbox or a proof against hard links, ancestor changes before opening the trusted root, or hostile mutation after hashing. The caller must supply a trustworthy evidence-root location. Revisit platform support and protected evidence ownership with real qualification, keeping the current containment checks intact.

## IMP-010: Hash bounded regular files with standard SHA-256 and active invariants

Date: October 17, 2026. Status: directly recorded bounded standard hashing; loop/buffer tradeoffs retrospective. Task: Task 1. Scope: `qualification.rs::hash_file`, `ARTIFACT_BYTES_MAX`.

Problem and constraints: Evidence integrity requires comparing actual bytes, without reading arbitrary files or allocating an entire artifact. Internal caller mistakes must be distinct from untrusted file failures.

Decision and rationale: Assert the internal `bytes_max` argument is positive and no greater than 16 MiB. Reject nonregular files and oversized metadata with ordinary errors. Stream SHA-256 through a fixed 65,536-byte stack buffer, track bytes actually read, reject excess and bound read iterations to the policy-derived maximum plus the EOF opportunity. On EOF assert the byte ceiling and return lowercase hexadecimal digest. Report I/O/limit failures through `AdmissionError::Evidence`. The worker explicitly selected standard cryptography, bounded reads and assertions; the constant-buffer allocation benefit is retrospective inference, not a measured performance result.

Alternatives and costs: Loading a whole artifact is shorter but uses memory proportional to file size. A weak checksum reduces dependency/code cost but does not meet the accepted integrity approach. A custom SHA implementation creates unnecessary correctness obligations. An unbounded read loop handles arbitrary short-read patterns but loses the explicit iteration ceiling. The selected bounded loop may reject a file whose reads repeatedly return short chunks; there is no claim of universal I/O-pattern support or an optimized buffer size.

Verification: Positive admission uses the known SHA-256 digest of real `abc` bytes. Unit and binary tests rewrite those bytes and observe rejection; a real file expanded past 16 MiB is rejected. `internal_hash_limit_cannot_be_zero` and `internal_hash_limit_cannot_exceed_policy_bound` call the real private function and intentionally trip both argument assertions. The EOF postcondition is not independently tripped by a test; the record does not count it as separate demonstrated invariant coverage.

Risks and revisit: A matching digest is an integrity check against supplied data, not proof of trusted origin or permanent immutability. Revisit limits, read behavior and buffer size only with a real workload or target requirement, preserving strict resource ceilings and standard cryptography.

## IMP-011: Reject duplicate JSON map entries during deserialization

Date: October 17, 2026. Status: observed implementation supported by worker details; visitor factoring retrospective. Task: Task 1. Scope: `qualification.rs::unique_map` and its local `UniqueMap<K,V>` Serde visitor methods `expecting` and `visit_map`; `versions` and `results` fields.

Problem and constraints: A JSON object can repeat a key. Ordinary map deserialization can silently select a later value, concealing conflicting outcomes or runtime versions before admission sees the map.

Decision and rationale: Reuse one generic Serde visitor for ordered, deserializable keys/values. `expecting` supplies a bounded-map diagnostic. `visit_map` reads at most the bounded sequence needed to detect a 129th entry, rejects duplicate keys or an entry beyond 128 and returns only a completed unique map. An unreachable assertion marks the structurally impossible fallthrough after the bounded loop. `unique_map` is the deserializer adapter attached to both fields. The decision to reject duplicate keys is contemporaneously recorded; the shared adapter's maintenance benefit is retrospective inference.

Alternatives and costs: Standard map decoding is simpler but loses duplicate provenance. Pre-scanning raw JSON duplicates parser work and risks disagreement with Serde. Separate visitors for capabilities and versions repeat behavior. The generic visitor adds local type/trait machinery and a 128-entry policy ceiling, while avoiding another parser or a general validation framework. On a duplicate, the map may be internally updated before returning the error, but that failed map is never returned to the caller.

Verification: `malformed_unknown_and_duplicate_json_never_loads` writes conflicting duplicate capability keys and conflicting duplicate version keys, then observes load failure. Successful report loading exercises empty-map completion and normal entries. There is no dedicated 128/129-entry test, formatter assertion test or fallthrough-panic test; that panic is structurally unreachable in the current loop.

Risks and revisit: The results enum limits distinct valid capability keys below the generic visitor ceiling; `validate_shape` still enforces the field-specific count. Revisit the shared adapter only if a real schema change needs another map policy; preserve explicit duplicate rejection.

## IMP-012: Preserve typed admission errors and readable required-capability names

Date: October 17, 2026. Status: observed implementation with worker-recorded CLI behavior; presentation tradeoffs retrospective. Task: Task 1. Scope: `qualification.rs::{AdmissionError, AdmissionError::fmt, impl std::error::Error}` and `Capability::as_str`.

Problem and constraints: Library callers need distinguishable failure categories, and CLI users need actionable mismatch or missing-capability messages. Missing native evidence must produce an error, not a success-like log message.

Decision and rationale: Use typed variants for unbound report, source mismatch, profile mismatch, invalid report, missing capabilities and invalid evidence. `fmt` emits fixed text for identity/context cases, includes bounded missing-capability names via `as_str` and prefixes detailed validation/I/O reasons. Implement the standard Error trait without an extra method or dependency. The worker directly records exit semantics and named failures; the API maintenance rationale is inferred.

Alternatives and costs: Strings everywhere reduce type definitions but force consumers to parse presentation. A catch-all error dependency eases aggregation but is unnecessary for this small closed set. Returning all validation failures at once adds collection and reporting complexity. The selected enum adds variant maintenance and returns one failing category at a time; only missing capabilities are aggregated.

Verification: Unit tests compare exact `MissingCapabilities` values. Actual-binary tests check named missing recovery, source/profile mismatch and SHA-256 mismatch on stderr with exit 1. They do not exhaustively assert every formatted variant. The empty standard Error implementation adds no production function to the coverage count.

Risks and revisit: OS error strings can include local filesystem details; this is a local CLI, not a sanitized external publishing channel. Revisit redaction and structured protocol errors before exposing these messages across a different trust boundary, without silently reducing useful local failure evidence.

## IMP-013: Parse the two CLI operations with a bounded explicit grammar

Date: October 17, 2026. Status: observed implementation; minimum-parser rationale supported by worker record, detailed alternatives retrospective. Task: Task 1. Scope: `src/bin/crazyagent.rs::{Arguments, parse, ARGUMENTS_MAX}`.

Problem and constraints: The CLI currently has only `qualification report` and `qualification admit`. It must reject ambiguous options, preserve OS-native path arguments and require explicit admission requirements.

Decision and rationale: Accept at most 32 arguments after the executable name and only the two operation names. Parse remaining arguments as flag/value pairs with `as_chunks::<2>()`; reject a remainder. Keep option values as `OsString` so filesystem paths need not be UTF-8. Permit only source/profile for report, adding report/evidence paths for admit. Reject empty, unknown or duplicate ordinary options. Repeated `--require` is allowed only for admit, each value must match `Capability::ALL`, duplicates fail, and at least one is required alongside both paths. Validate source/profile before returning `Arguments`, establishing the lookup preconditions used by `execute`.

Alternatives and costs: A parser dependency offers richer help and conventions but adds a dependency for a small grammar. Permissive unknown/duplicate flags are easier to accept but hide mistakes or implicit last-value selection. Converting all arguments to strings simplifies types but rejects valid native paths. The chosen parser costs hand-maintained usage/validation and requires extension when positional arguments, more operations or another option style is actually needed.

Verification: `binary_requires_trusted_source_profile_and_nonempty_requirements` exercises missing options, missing profile, duplicate source, unknown option and an empty requirement list through real processes, expecting exit 2. Successful binary admission covers the normal parser path. Excess argument count, duplicate requirements, non-UTF-8 arguments and every pair/remainder branch lack dedicated baseline tests. Historical Clippy rejected `chunks_exact(2)`; the worker changed it to `as_chunks::<2>()` and reran the strict check without allowances.

Risks and revisit: The operation-name unwrap follows a prior successful pattern check; option indexing follows parse's required-key checks. Refactoring that order must preserve those invariants. Revisit grammar/dependency choice when CLI growth makes this smaller explicit parser harder to maintain than an established alternative.

## IMP-014: Separate process exit handling from invocation and admission

Date: October 17, 2026. Status: observed implementation with contemporaneously recorded exit contract; separation rationale retrospective. Task: Task 1. Scope: `src/bin/crazyagent.rs::{main, execute, USAGE}`.

Problem and constraints: Real shell callers need reliable status codes and separate stdout/stderr. CLI input must bind the same library policy rather than implement another admission path.

Decision and rationale: `main` collects at most 33 OS arguments so `parse` can detect excess beyond 32, handles sole `--help`/`--version` requests, then converts `execute` results into `ExitCode`. `execute` parses, obtains validated text identities, dispatches report generation or constructs `AdmissionContext`, loads the report and calls the real library `admit`. Successful admission writes named requirements to locked stdout. Parsing/text invocation errors use exit 2; evidence/admission/output errors use exit 1; success uses exit 0 and failures print to stderr. Collection bounds argument count, not individual OS argument size.

Alternatives and costs: Calling `process::exit` throughout shortens return plumbing but couples every path to process termination. Panicking on rejected evidence gives less usable operational failures. Reimplementing policy in the binary creates drift. The selected split adds result conversion and small temporary name vectors while keeping business behavior in the library and making real-process tests straightforward. No framework or provider adapter is added.

Verification: The three actual-binary tests execute this path, observe successful JSON/admission output and distinguish exit 1 from exit 2 on real failures. Dedicated help/version and broken-pipe tests are absent. Locked fallible writes propagate errors for report/admission output; the help/version printing macros use their standard behavior, so the record does not promise uniform broken-pipe handling.

Risks and revisit: Human-readable success text is not a protected engine authorization token. Future engine/protocol callers should use the typed API and trusted policy. Revisit command dispatch when new commands exist; preserve status/error semantics or migrate consumers explicitly.

## IMP-015: Validate textual CLI identity separately from native path values

Date: October 17, 2026. Status: observed implementation; duplicate-boundary rationale retrospective. Task: Task 1. Scope: `src/bin/crazyagent.rs::text_option` and its callers `parse`/`execute`; library `valid_text`.

Problem and constraints: Source and profile are textual identities, while report/evidence paths are OS-native values. Invalid identities should be invocation errors before accessing evidence, and direct library consumers must retain their own validation.

Decision and rationale: Look up the requested option, require UTF-8, nonblank text of at most 256 bytes and no control characters, and return a borrowed string without normalizing it. `parse` checks both required identifiers; `execute` retrieves them through the same helper. The library independently validates trusted and reported identities during admission. This modest duplication preserves distinct CLI and library error boundaries without making the library depend on the binary parser.

Alternatives and costs: One exported generic validation API reduces repeated checks but expands the library surface for a small helper. Validating only in the CLI leaves direct library callers unchecked. Validating only in admission allows report mode to emit malformed identities and changes error categorization. The selected checks cost small bounded text scans and repeated validation; no performance claim is made for that cost.

Verification: Real-binary missing-profile/missing-argument cases exercise the helper's failure path, and normal report/admission cases exercise borrowed valid identities. Source observation confirms the other character/byte checks; the baseline has no dedicated whitespace, control-character, maximum-length or non-UTF-8 identity tests. The unit empty-identifier variant exercises library validation separately.

Risks and revisit: Leading/trailing non-control whitespace is retained when the identity is otherwise nonblank; identities are exact-match strings rather than normalized or validated Git object IDs. Revisit only with a concrete identity format contract, not an inferred requirement to normalize existing inputs.

## IMP-016: Generate an explicitly unavailable report without executing native tools

Date: October 17, 2026. Status: directly recorded worker decision and DEC-065. Task: Task 1. Scope: `src/bin/crazyagent.rs::write_report`, `QualificationReport::new`, `Capability::ALL`.

Problem and constraints: Task 1 needs a real serializable report command before native runtime/profile qualification. It must not spend against accounts, claim unavailable outcomes or fabricate a runtime inventory.

Decision and rationale: Construct report data from caller identity, record only the compiled CrazyAgent package version, mark all four capabilities unavailable and give each a native/runtime-evidence-not-collected gap. Leave artifacts empty. Pretty-print JSON to locked stdout and terminate with a newline, propagating serialization/write errors. No runtime discovery, account access or provider call occurs. The unavailable report is deliberately nonadmissible, while still being a valid record of missing evidence.

Alternatives and costs: Inferring success from installed tools is cheap but does not prove behavior. Synthetic proven outcomes would misrepresent qualification. Refusing to emit any report until native readiness removes a useful machine-readable gap record. Running discovery automatically would enlarge authority and latency for a reporting command. The selected approach costs a later real evidence collector and does not fulfill native qualification by itself.

Verification: `report_serializes_unavailable_capabilities_and_admit_names_them` runs the real binary, parses stdout, checks source/profile and empty artifacts, checks every capability unavailable, then writes that stdout to a real file and observes admission exit 1 naming recovery. DEC-065 also records the leader's direct unavailable report observation. Neither is native runtime evidence.

Risks and revisit: The version entry is the application version only, not a runtime version inventory. Approved sandbox-runtime acquisition is a separate subsequent task. Extend reporting only with actual authorized probes and provenance; do not turn this skeleton into a passing report by changing outcome labels alone.

## IMP-017: Establish admission behavior with real files, real processes and explicit historical TDD evidence

Date: October 17, 2026. Status: directly recorded historical execution, inspected test source; evidence limitations explicit. Task: Task 1. Scope: the test-only module in `qualification.rs`, `tests/qualification_cli.rs`, root `Makefile` and worker handoff. Test helpers are excluded from the production-symbol count.

Problem and constraints: DEC-062 forbids mocks, skipped failures and fabricated runtime replies. Admission can be tested before native capability qualification, but synthetic report inputs must never be counted as native proof. Assertions must remain active and testable where used for internal arguments.

Decision and rationale: Unit tests call production loading/admission/hash functions with actual tempfile directories, real JSON and real evidence bytes, including symlinks and file-size changes. Synthetic identity/version/outcome fields are input data only. Binary tests use `Command` with Cargo's actual built executable, real files, stdout/stderr and exit codes. Use the known SHA-256 of `abc` rather than another fake engine reply. Two `should_panic` tests intentionally trip private hash-limit assertions; they are executed tests, not ignored tests. The design follows the direct worker report and DEC-062; the value of keeping the file fixtures small is retrospective inference.

Alternatives and costs: Mocked files/processes make tests easier to isolate but violate authority and fail to exercise OS behavior. Requiring native runtime/provider success for this logic would block an independent contract and introduce unapproved execution. Only testing serialized strings would miss path and byte-integrity behavior. Real fixtures/processes cost filesystem work, subprocess startup and platform prerequisites; no benchmark quantifies that cost.

Historical red/green record, using the worker's installed modern toolchain PATH:

| Check | Recorded red | Recorded green and limits |
| --- | --- | --- |
| `cargo test --offline -p crazyagent --lib qualification::tests::missing_evidence_never_admits` | Missing API implementation first; after typed scaffolding, one executed test failed at the empty-report rejection assertion, exit 101. | Included in the later passing unit suite. The first Cargo 1.71.2 resolver-3 failure happened before compilation and is infrastructure failure, not behavior red. |
| `cargo test --offline -p crazyagent --lib` | Expanded suite: 2 passed, 7 failed, none ignored, covering invalid/unavailable evidence and escaping input. | Initial implementation: 9 passed. Final suite adds two assertion tests: 11 passed, none ignored. |
| `cargo test --offline -p crazyagent --test qualification_cli` | 0 passed, 3 failed, none ignored: empty output failed JSON/text checks and mismatched identity incorrectly succeeded. | 3 real-binary tests passed, exit 0, none ignored. |
| `PATH=/Users/chaoticqubit/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH make check` | First full check failed strict Clippy on `chunks_exact_to_as_chunks`, exit 2; the parser was corrected without an allowance. | Final worker check exited 0: format, strict Clippy, 11 unit plus 3 binary tests, build, then those tests again. DEC-065 separately records the same integrated leader totals. Zero-test binary/doc-test targets are not behavior evidence. |

Verification: These outcomes are reported historical execution, not commands rerun by the documentation worker. The pinned source has exactly eleven unit test declarations and three binary integration test declarations, no ignored tests and no mocking layer. All production and test source files match `37ead0f`. Source inspection plus worker/leader records support this attribution; separate raw command-output files were not available in the worker handoff directory.

Risks and revisit: These tests prove admission behavior on the exercised local profile, not confinement, live-worker recovery, authentication, child controls, end-to-end V1 operation or another operating system. No native engine, desktop or TUI acceptance result exists in this slice. Reviews are deferred, not passed. Later tasks must retain actual native execution evidence and their own red/green records without using this historical suite as substitute proof.

## Task 1 production-symbol coverage index

The count is **18 of 18 production function/method definitions**: thirteen in `qualification.rs`, five in `src/bin/crazyagent.rs`, zero in `src/lib.rs`. It includes both local Serde visitor methods and both distinct constructors. It excludes derived/generated methods, the empty Error implementation, constants, type declarations, closures and all test-only helpers/tests. Nonfunction exports/types/build choices are covered by IMP-003 through IMP-005 and the related records above.

All qualification symbols below resolve in `crates/crazyagent/src/qualification.rs`; all CLI symbols resolve in `crates/crazyagent/src/bin/crazyagent.rs`. Use names with their enclosing type or local visitor, not line numbers, as stable references.

| Production symbol | Decision records | Role |
| --- | --- | --- |
| `Capability::as_str` | IMP-004, IMP-012 | Canonical CLI/error spellings corresponding to serialized capability names. |
| `AdmissionContext::new` | IMP-005 | Own caller-provided expectations separately from report JSON. |
| `QualificationReport::new` | IMP-004, IMP-005, IMP-016 | Construct unbound report data with empty collections. |
| `AdmissionError::fmt` | IMP-012 | Render typed rejection categories and required-capability names. |
| `load_report` | IMP-005, IMP-006, IMP-011 | Bound input, parse strict records and attach caller context. |
| `admit` | IMP-007, IMP-008, IMP-009, IMP-010 | Enforce required outcomes, identity, shape and every artifact digest. |
| `valid_text` | IMP-008, IMP-015 | Shared library text-boundary predicate. |
| `validate_shape` | IMP-008 | Enforce version/result/gap/artifact resource and syntax policy. |
| `open_artifact` | IMP-009 | Resolve normal relative components through no-follow directory handles. |
| `hash_file` | IMP-010, IMP-017 | Hash real bounded regular-file bytes and assert internal limits. |
| `unique_map` | IMP-011 | Attach the shared unique-map Serde visitor. |
| `unique_map::UniqueMap<K,V>::expecting` | IMP-011 | Describe expected bounded unique map input. |
| `unique_map::UniqueMap<K,V>::visit_map` | IMP-011 | Reject duplicate/excess entries while decoding. |
| CLI `main` | IMP-014 | Bound argv collection and translate results to process status. |
| CLI `execute` | IMP-014, IMP-005, IMP-007 | Connect validated invocation to report generation or library admission. |
| CLI `parse` | IMP-013 | Validate the bounded two-operation flag grammar. |
| CLI `text_option` | IMP-015 | Borrow validated UTF-8 identities while retaining native path values. |
| CLI `write_report` | IMP-016 | Emit explicit unavailable outcomes without native/provider execution. |

Coverage verification uses actual function definitions before `#[cfg(test)]` and the complete binary/library sources, not added-line counts. No source changed in this task. The shared graph was queried, not refreshed, because the focused brief reserves its symlinked cache for the leader. The leader owns subsequent consolidation, graph refresh and integration; this worker returns ownership of the tracked log on handoff.

## IMP-018: Resolve and inspect the runtime lock before installation

Date: October 17, 2026. Status: implemented. Task: DEC-068 dependency acquisition. Scope: `.local-state/tools/sandbox-runtime`, its generated package lock and npm invocation.

Goal and constraints: acquire the exact candidate runtime using existing Node/npm, with project-local state and no global configuration, credentials, lifecycle hooks or provider calls.

Decision and rationale: resolve the lock first, compare version and SHA-512 with official registry metadata, inspect the entire five-package closure, then run `npm ci --ignore-scripts` with audit, funding and update requests disabled. Supply dedicated empty user/global npm configuration files and a project-local cache. The published runtime needs neither credentials nor installation scripts. This makes the input concrete before execution and avoids importing user authentication or global npm behavior.

Alternatives and costs: a global install is convenient but changes machine state and violates scope. Floating `npx` has less setup but can download changing inputs on invocation. A source rebuild adds a TypeScript/Bun build closure without being necessary for this published package. The selected approach costs local disk/cache duplication and an explicit lock inspection.

Verification: runtime 0.0.80 matched published integrity `sha512-WG4P8faU/JDH90I8NLUBPmg7e72zhanlW3AVCD0OF06JCtId9IDFewFgv4YRvHLcnn//h1r6a1AMosBsMLpxnA==`. The resolved closure is sandbox-runtime 0.0.80, socks5-server 1.0.10, commander 12.1.0, node-forge 1.4.0 and zod 3.25.76. No package declares an install script. `npm ci` added five packages with lifecycle execution disabled; the actual executable returned 0.0.80.

Risks and revisit: acquisition/version proof does not establish confinement. Release packaging still needs retained immutable metadata, license notices and an explicit runtime acquisition or bundle policy. The local tools and cache are ignored artifacts, not shipped dependencies.

## IMP-019: Inspect immutable RCLI source before executing its lifecycle or build

Date: October 17, 2026. Status: source acquired; execution unqualified. Task: Task 3 preparation. Scope: `.local-state/tools/rcli-source` and its bounded upstream archive.

Goal and constraints: reuse the selected open-source leader harness while preserving project-local acquisition authority and avoiding provider calls, host changes and implicit Git hook changes.

Decision and rationale: acquire Backboard-io/Backboard-R-CLI revision `51bee17e02344de5fffd89523ce2b73d5042f216`, validate the archive before extraction, and inspect transport and build metadata before running source. The actual npm package name returned 404; a similarly named package is not a substitute. The upstream `prepare` script changes Git hooks. Observed one-shot JSON output does not establish a persistent bidirectional leader protocol.

Alternatives and costs: a floating installer reduces initial setup but loses immutable input and may run hooks. Forking before checking native capabilities creates avoidable maintenance. The selected path adds provenance bookkeeping and may reveal adapter/build work that changes the forecast. Source inspection can continue without credentials or provider spending.

Verification: the bounded archive contains 787 members and 3,833,836 expanded bytes, with no absolute, parent, symlink, hardlink or device entries. The 1,374,859-byte archive has local SHA-256 `4519cfb485017e1690da72c86dcf7d5073fe09b81d7e632af4707052f57a7603`. Its manifest reports version 3.0.4 and MIT. No source execution or dependency build occurred. Graph generation for this ignored third-party tree reported no code files; the existing project graph and pinned research references supplied scoped navigation before source inspection.

Risks and revisit: the local hash records observed bytes, not a publisher-signed checksum. Persistent leadership, authentication modes and child control remain unproven. Resolve and inspect the pinned dependency closure and any explicit build effects before execution; qualify an actual admitted profile before claiming native leader support.

## IMP-020: Continue independent durable-core work while native admission is blocked

Date: October 17, 2026. Status: planned. Task: Task 5 continuation under Task 4 step 5. Scope: protocol, configuration, storage and engine modules in a separate worktree.

Goal and constraints: continue toward the complete V1 without weakening failed native requirements, claiming simulated integration or spending against provider accounts. One visible worker owns this task; the leader owns integration and the shared graph.

Decision and rationale: implement and test real Rust authority, SQLite durability, local client transport and effective settings independently of native dispatch. Keep native launch unavailable until its profile is admitted. Complete real database/socket/restart tests now; retain the worker-sandbox endpoint denial as an explicit pending integration gate until Task 2 supplies an admitted boundary. The accepted plan explicitly permits independent authorized work when qualification is blocked.

Alternatives and costs: waiting for all native qualification serializes unrelated work and consumes delivery capacity without producing evidence. Enabling native launch with partial boundary evidence violates the accepted contract. Independent implementation costs an additional integration checkpoint and careful separation of verified core behavior from unavailable native behavior.

Acceptance and verification: real tests must cover operation replay after lost acknowledgement and engine restart, strict bounded requests, task-pinned configuration, migration/revocation, exclusive input ownership, event cursor resynchronization and SQLite failures without false acknowledgement. No mock harness or forged capability may satisfy the missing worker-boundary test. Exact dependency versions/licenses, function rationale and unproven edges belong in the worker handoff and this log before integration.

Risks and revisit: the complete Task 5 acceptance remains pending until its real admitted-boundary test passes. Shared manifest, library-export and CLI edits are serialized after Task 2 hands them back. The leader must reconcile actual commits and test output before marking any step complete.


## IMP-021: Explicit bounded probe profile

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: selected before implementation. Symbols: SandboxProfile, ResourceLimits, NetworkPolicy, SandboxProfile::validate, BoundaryError and its Display/Error implementations.

Goal: qualify the actual pinned runtime using disposable files without loading native accounts or granting arbitrary paths. Store runtime and Node executable paths as explicit nonsecret references; require canonical disjoint sibling candidate/trusted directories, exactly the candidate write grant, deny-all network, synthetic credential references within trusted fixtures and bounded positive resource requirements. Reject malformed input with ordinary errors. Use existing Serde and stdlib; add no dependency.

Alternatives: inherit arbitrary PATH/settings/environment, accept broad grants, or build a general sandbox-policy compiler. These either hide effective authority or exceed this qualification slice. The explicit profile costs more invocation arguments but is reproducible and narrow. Cross-platform and arbitrary policy support remain unavailable.

Acceptance: actual-file validation tests reject overlapping roots, symlinked roots, traversal and unsupported policy. Runtime absence/version mismatch must be unavailable. No native pass from validation alone. Revisit when engine-controlled profile storage and authenticated policy exist.

## IMP-022: Native observations separate from admission

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: selected before implementation. Symbols: BoundaryEvidence, run_boundary_probe, probe execution and observation helpers, record_case.

Goal: run a fixed Node probe through the actual srt CLI with explicit settings, sanitized environment and bounded process-group supervision. Retain settings, scripts, stdout/stderr, results and parent-observed filesystem/network outcomes. Check a real loopback positive control before denial. Native child probes use actual subprocesses. Never accept a command exit/output marker alone as confinement proof. Record missing CPU/memory/process enforcement and deferred engine endpoint controls explicitly; unsupported required controls return a blocking result carrying evidence.

Alternatives: shell-output-only assertions, inventing a resource controller, unchecked arbitrary test commands or a generic runner abstraction. They either overstate proof or increase attack surface. Fixed probes cost one bounded native launch and a small number of disposable files. Resource observations are measurements, not enforcement. Revisit backend/resource policy after real results. The application engine endpoint does not exist and is not tested here.

Acceptance: real red/green tests; exact unavailable diagnostics when the outer sandbox blocks launch; preserved evidence and nonzero native qualification exit for failed controls or missing required enforcement. Assertions guard duplicate case recording and have a triggering test.

## Execution plan

1. Add real filesystem profile-validation tests and the explicit native qualification test. Capture red evidence.
2. Implement minimum typed validation and bounded actual-runtime probe. Capture green validation evidence.
3. Run the actual disposable native probe; retain versions, outcomes and exact restrictions.
4. Run make check and record precise status. Prepare scoped commit if metadata permits.
5. Hand decision/evidence paths and graph-refresh request to the leader. Reviews stay deferred.

## IMP-023: Bound the real positive-control accept

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: implemented after reproduced behavioral failure. Symbols: accept_control, positive_loopback_control_waits_for_connection.

Problem: the nonblocking host listener can return WouldBlock immediately after the positive TCP connect completes. The first native probe failed with os error 35 before srt launch. A delayed actual TCP connection reproduced this failure in the production accept helper.

Choice: poll nonblocking accept every 10 ms with a caller-selected one-second deadline. Alternatives were an unbounded blocking accept or assuming immediate readiness. Bounded polling costs at most 100 wakeups for the positive control and avoids a false native failure. No platform privilege escalation or mock is involved.

Verification: the delayed-connection test first failed with Io(WouldBlock); after the change all 14 library tests and all 5 boundary integration tests passed. The actual runtime was 0.0.80, host/child Node v26.12.0. Revisit for a reusable event-driven runner rather than generalizing this qualification helper.

## IMP-024: Empirically qualify inherited Darwin resource limits

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: selected before implementation following explicit owner resource-gap follow-up. Symbols: separate crazyagent-boundary-child executable and its resource-role functions; resource-boundary tests; bounded_command and kill_group.

Problem: native srt observations prove filesystem/network behavior but do not prove the requested CPU=2 seconds, RSS=64 MiB or process-count=4 limits. The first successful native run used 59,852 user CPU microseconds, 20,942 system CPU microseconds, 43,312 KiB max RSS and one successful candidate child. These are observations under no resource controller. The watchdog independently killed a real 5-second sleep after a 40 ms deadline.

Choice: use already-locked rustix safe process/resource APIs in a separately launched Rust child, set inherited limits there and execute only fixed bounded disposable test roles. Measure CPU termination, a small allocation against address-space/resident-set controls and whether process limits prevent even the first child for the already-busy real user. No unsafe pre_exec, Python, new dependency version or privileged mechanism. Keep controls unavailable until measured; inherited per-process CPU and per-UID process counts cannot be silently described as whole-tree budgets.

Alternatives: assume RLIMIT_AS enforcement on macOS, build a new runtime, use a privileged helper or weaken the profile. The first is unproved and the others need a demonstrated gap and further design/authority. The bounded experiment costs one small Rust executable plus real integration tests, a few seconds CPU and at most 32 MiB allocation per child. Revisit with measured results before selecting a production controller.

Measured results: resource-tests/.tmpvFv7th/resource-evidence.json records a 24 MiB allocated/touched positive control and six simultaneous short-lived child processes without NPROC=4. RLIMIT_CPU soft/hard=2 survived exec and terminated the real CPU child with signal 24 after 2,113 ms. RLIMIT_AS=16 MiB, RLIMIT_AS=64 MiB and RLIMIT_RSS=64 MiB all returned EINVAL; rustix represents Darwin Rss as an alias of As. NPROC=4 survived exec but rejected all six spawn attempts with os error 35, including the first. This is not a usable task-local four-process budget. CPU evidence proves the fixed Rust role's per-process limit only, not an aggregate tree budget or application to srt. The full sandbox profile retains all three resource gaps.

## IMP-025: Reuse Task 1 outcome vocabulary without granting authority

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: implemented. Symbols: BoundaryEvidence::confinement_outcome, BoundaryEvidence::admissible.

Choice: missing or false named controls map to Task 1 Outcome::Failed; complete observations with any enforcement gap map to Outcome::Unavailable. Only complete observations without gaps map to Proven. The actual runner always returns the blocking Unenforced error after successful filesystem/network observations because CPU, memory, process, engine endpoint and default-write exceptions remain unqualified.

Alternatives: treating srt exit zero as admission, accepting partial case maps or inventing a second outcome taxonomy. Reusing the existing enum costs no dependency or report migration and prevents successful observations from concealing missing controls. These structures do not authenticate an evidence producer and do not replace Task 1 trusted-context/integrity checks or later engine policy.

Verification: empty/partial evidence is rejected by the library test; the native integration check asserts inadmissibility and the Unavailable outcome. The final native rerun remains pending after workspace loopback infrastructure denial. Revisit when the leader integrates actual trusted evidence production and each required control is empirically established.

## IMP-026: Preserve bounded evidence and cleanup on observation errors

Date: October 17, 2026. Task: Task 2. Source: contemporaneous execution-boundary worker handoff.

Status: implemented. Symbols: write_new, write_json, read_bounded, bounded_command, kill_group, ResourceObservation.

Choice: create evidence files exclusively instead of overwriting; cap each inspected native stdout/stderr/result at 64 KiB; poll command deadlines/output every 10 ms. Clear inherited environment and expose only selected executable directories plus system commands, disposable HOME/TMPDIR and LANG=C. Use isolated process groups and the existing rustix process feature for safe group termination, followed by direct-child reaping on unsuccessful observation. Reject PID 1 before group signaling. No unsafe hook, external kill subprocess, Python or new package version is needed.

Alternatives: inherit provider environment, collect unbounded pipes, reuse files, use unsafe pre_exec or modify the runtime. The fixed launcher and file outputs keep this slice small and observable. Polling bounds wall time and sampled file growth, not atomic hard output bytes or resource consumption. A hostile descendant escaping its group remains unqualified; this watchdog is not claimed as a whole-tree resource boundary.

Verification: a real five-second sleep is stopped by the 40 ms watchdog test; the PID-1 guard has a should-panic test; exclusive writes and output inspection run through actual native artifacts. Final Clippy passed with warnings denied; final build passed. The final make check reached the real library tests and failed on two loopback binds denied by the ordinary workspace sandbox. No test was ignored, weakened or removed.

Revisit: when runner lifetime/cancellation is implemented, use qualified tree identity and cleanup rather than generalizing this probe supervisor into a security authority.

## Production function coverage index

| File and symbols | Decisions | Function-level rationale |
| --- | --- | --- |
| isolation.rs: SandboxProfile::validate, clean_absolute | IMP-021 | Canonical sibling fixtures, exact candidate grant, bounded metadata/requirements, no traversal, no credential values and direct trusted credential references. Invalid inputs return errors before native work. |
| isolation.rs: BoundaryError::fmt, From<io::Error>::from | IMP-021, IMP-026 | Distinguish invalid input, runtime/infrastructure unavailability, failed controls and unsupported requirements. Preserve actual errors and artifact location. |
| isolation.rs: run_boundary_probe | IMP-022, IMP-026 | Validate first, require disposable empty roots, create a fresh evidence directory and persist both blocking evidence and launch errors. Never overwrite user files. |
| isolation.rs: collect_boundary | IMP-022, IMP-025 | Verify actual host/srt/child versions, construct explicit settings, run fixed native probes, inspect independent filesystem and connection outcomes, retain every required-control gap. |
| isolation.rs: record_case | IMP-022 | Duplicate case observations are an internal invariant violation; a test trips the assertion. Distinct named cases prevent silent replacement. |
| isolation.rs: accept_control | IMP-023 | Wait for real nonblocking TCP readiness under a deadline; do not mistake a readiness race for failed confinement. |
| isolation.rs: BoundaryEvidence::admissible, BoundaryEvidence::confinement_outcome | IMP-025 | Require every named positive/negative observation and zero gaps; reuse Task 1 Outcome without authenticating or admitting a worker. |
| isolation.rs: run_resource_probe | IMP-024, IMP-026 | Run seven fixed bounded real roles, preserve exit/signal/time/raw output, provide observation evidence rather than claiming all setters enforce policy. |
| isolation.rs: write_new, write_json | IMP-026 | Exclusive writes prevent destructive fixture/evidence reuse; existing Serde serializes records without a custom format. |
| isolation.rs: read_bounded | IMP-026 | Inspect only regular native outputs, reject oversized data before read and recheck the bounded read. |
| isolation.rs: bounded_command, kill_group | IMP-024, IMP-026 | Sanitize environment, isolate process groups, monitor wall/output bounds, terminate/reap on monitor errors and reject PID 1 before safe rustix signaling. |
| isolation.rs: PROBE deniedWrite, finish and fixed callbacks | IMP-022 | Real direct/traversal/symlink writes, actual child Node writes, synthetic credential reads and loopback connection attempts. Permission-specific errors and parent-observed unchanged bytes/no connection are both required. Child usage is measured, never labeled enforcement. |
| crazyagent-boundary-child.rs: main | IMP-024 | Explicit nonzero exit plus actual diagnostics for failed setters or execution. No unavailable result is translated to successful qualification. |
| crazyagent-boundary-child.rs: execute | IMP-024 | Dispatch only a bounded fixed role set; disable core dumps in the disposable child; query limits after exec to prove inheritance. No arbitrary command role is exposed. |
| crazyagent-boundary-child.rs: limit_and_exec | IMP-024 | Apply soft and hard limits in an independently launched process and exec the same fixed Rust binary. This avoids unsafe pre_exec and does not modify the parent engine/user limits. |
| crazyagent-boundary-child.rs: allocate | IMP-024 | Reserve/touch/checksum exactly 24 MiB, recording success or allocation denial. This is a positive workload control, not proof a 64 MiB budget is enforced. |
| crazyagent-boundary-child.rs: burn_cpu | IMP-024 | Fixed four-second wall envelope with actual arithmetic work; observe OS CPU termination at the inherited two-second limit. |
| crazyagent-boundary-child.rs: spawn_children | IMP-024 | At most six actual children, each sleeping 100 ms; retain setter-inherited spawn errors and reap successful children. This measures feasibility without a fork bomb. |

## Historical worker handoff before leader native verification

The final workspace-native make check passed formatting and strict Clippy, then failed two real loopback unit tests at bind with EPERM. The successful native filesystem/network and resource probes ran through context-mode's separate execution environment. Do not conflate those contexts or retry through that environment after discovering the distinction. No sandbox escalation is attempted by this worker. The leader owns the approved final native command in task-02-report.md.

One concrete replacement for consideration, not implementation: a project-local Linux VM worker boundary with one delegated cgroup v2 per task, memory.max, pids.max and a measured CPU controller, retaining srt where filesystem/network policy is useful. Estimate 2-4 engineering days for a bounded prototype and real enforcement tests, plus separately estimated native account/control/recovery compatibility qualification. This changes deployment and may conflict with required native macOS integrations. No VM installation, privilege change, account change or acceptance-criteria change is selected or authorized here. The current profile remains unavailable; the owner must decide any replacement after reviewing the measured gaps.

Final native command, to be run by the leader with its owner-authorized local-probe escalation:

```sh
cd /Users/chaoticqubit/Projects/CrazyAgent/.worktrees/execution-boundary
PATH=/Users/chaoticqubit/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH CRAZYAGENT_SRT=/Users/chaoticqubit/Projects/CrazyAgent/.local-state/tools/sandbox-runtime/node_modules/.bin/srt CRAZYAGENT_NODE=/opt/homebrew/bin/node make check
```

Commit handoff: scoped git add failed because the workspace sandbox denied creation of /Users/chaoticqubit/Projects/CrazyAgent/.git/worktrees/execution-boundary/index.lock. No commit, metadata escalation or publication was attempted. The leader must stage/commit the preserved six scoped files and consolidate this local log. Task 2 remains partial and the full profile unavailable.

## Leader verification of Task 2 implementation

The unchanged native `make check` exited 0 in the owner-authorized local-probe environment. Formatting, strict Clippy, 17 library tests, 5 filesystem/network boundary tests, 3 CLI tests and 1 resource-observation test passed; build and the complete same suites passed again. No tests were ignored. Raw output is retained at `.worktrees/execution-boundary/.local-state/leader-native-check.log`. This supersedes the earlier workspace-sandbox EPERM as the final execution result, without erasing that historical infrastructure failure.

A source/document consistency check found 22/22 Rust production definitions across `isolation.rs` and `crazyagent-boundary-child.rs` mapped to the decision index above. Fixed embedded JavaScript probe functions and callbacks are mapped separately. These checks establish implementation behavior and documentation coverage, not a completed security/code review or a fully admitted native profile. The resource controller, engine-endpoint denial and default writable exceptions remain explicit gaps. DEC-069 authorizes the bounded Docker replacement prototype documented in `docs/qualification/resource-boundary-proposal.md`; permanent runtime adoption remains undecided.
