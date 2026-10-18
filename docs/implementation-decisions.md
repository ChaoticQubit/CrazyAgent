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

## IMP-027: Reuse the Docker CLI for an isolated qualification experiment

Date: October 17, 2026. Status: selected before implementation. Task: DEC-069 resource-boundary prototype. Scope: development-only qualification script, real integration check, project-local Docker client configuration and immutable image.

Goal and constraints: collect real cgroup evidence with the smallest existing toolchain, preserve the owner's exact experiment caps and leave unrelated Docker workloads untouched. The Rust application engine remains the product implementation.

Decision and rationale: use the installed Docker CLI with the verified local Unix endpoint and an empty project-local Docker configuration. This avoids user credential helpers during anonymous official-image acquisition. The acquired image identity and platform match the approved immutable Linux arm64 manifest. Use existing Python standard-library tooling only to supervise this development experiment, retain bounded artifacts and drive real checks; do not add Python as a product runtime dependency or introduce a Docker SDK.

Each disposable container has a unique experiment label and name. Verify its image, labels and enforced configuration before starting. Run one at a time with the approved caps, no network, read-only root, dropped capabilities, no-new-privileges, an unprivileged user and no host mounts or socket access. Cleanup may target only identity-checked containers from this experiment. A 15-second external execution deadline and bounded output apply even when expected memory/PID exhaustion occurs. Preserve postmortem state and counters before removal.

Alternatives and costs: a new Rust Docker SDK adds dependencies and API surface before selecting a product runtime. Shell pipelines provide less precise identity/deadline/error handling. Reusing an existing application container risks unrelated work and loses clean provenance. A short Python qualification tool costs a separate development command and must be replaced or explicitly retained if a production backend is later adopted.

Acceptance and verification: test before implementation, use actual Docker/SQLite-independent local systems with no mocks or skipped tests, demonstrate successful positive controls, memory OOM, PID exhaustion and CPU throttling, and verify no experiment containers remain. Report CPU rate separately from aggregate CPU time; retain missing native/full-boundary requirements. Source, tests and function rationale are integrated only after the actual bounded command passes.

Risks and revisit: Docker daemon access itself is privileged developer authority and must never be forwarded into a coding sandbox. This probe does not qualify the Docker API as a product control plane, repository mount safety, native authentication, recovery or full isolation. Permanent runtime adoption remains an owner decision after evidence.

## IMP-028: Qualify RCLI's persistent host contract before claiming native leadership

Date: October 17, 2026. Status: source-grounded qualification direction; implementation pending. Task: Task 3 preparation. Scope: pinned RCLI CLI, AgentController assembly, JsonEventStream and provider factory.

Goal and constraints: reuse the selected open-source harness for central leadership while keeping Rust policy authoritative and preserving the original live worker across an engine-only restart. No account/provider calls or source execution are authorized by source inspection.

Decision and rationale: treat the observed one-turn CLI and JSON event output as insufficient for persistent host qualification. Trace and reuse the actual controller/tool/context components through a dedicated host entrypoint if the constructors permit it, then qualify bounded input, permissions, cancellation and reconnect against the original process. Keep its stream-local sequence separate from the engine's durable cursor. Retain observable provider events without claiming hidden reasoning. Exact inspected source points and remaining tests are recorded in `docs/qualification/rcli-host-readiness.md`.

Alternatives and costs: repeated one-shot CLI launches are simple but do not meet live identity/recovery. Reimplementing the complete agent loop in Rust duplicates a selected upstream harness and its maintenance. A dedicated host entrypoint adds a small maintained integration surface whose actual assembly and dependency closure still need verification; source construction alone is not a successful native test.

Acceptance and verification: inspect the pinned production closure and startup effects before execution, then pass two actual turns, permission round trip, original-process reconnect, cancellation and logging-failure checks under an admitted profile and separately approved account/budget. Do not infer Claude Code/Codex subscription compatibility from the observed Backboard/BYOK provider factory.

Risks and revisit: RCLI declares no public exports map and its controller assembly includes multiple supporting services. Trace those constructors before choosing the smallest host composition; forecast additional work if it cannot meet the accepted contract. Permanent execution runtime and native account authority remain unresolved. No new runtime dependency or source fork is shipped by this preparation decision.


## IMP-029: Strict protocol boundary

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Goal: validated string identities, bounded JSON frames, explicit versions and outcomes. Use serde deny_unknown_fields, validated constructors/deserializers and length-prefixed frames. Reject credentials by schema rather than generic secret scanning. Alternatives: arbitrary JSON is smaller initially but lacks a trust boundary; UUID adds no needed capability. Cost: typed enums require coordinated additions. Limits: 64 KiB frames, 128-byte identity, 256-byte project name, 64 repositories. Acceptance: invalid identities, versions, unknown fields and oversized frames rejected. Revisit when another task adds a command. Symbols: protocol identity newtypes, Cursor, WireRequest, WireReply, Command, Outcome, ProjectSpec, ExecutionContext, framing.

## IMP-030: Durable serialized authority

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Goal: commit operation identity, result, revision and event atomically before acknowledgement. One dedicated SQLite thread consumes a bounded Tokio channel; no database work on reactor. Scope idempotency by client plus request ID and canonical typed request bytes. Cache lookup precedes revision checking. Alternatives: reactor-blocking SQLite harms responsiveness; pool adds contention; in-memory replay loses crash safety. Cost: one thread and serialized writes. Use SQLite FULL synchronous WAL, bounded busy timeout, bounded event page with snapshot on gaps. Acceptance: real reopen replay creates one project, changed operation bytes fail, SQLite read-only write fails without success. Symbols: storage worker and transaction functions, Engine durable command methods.

## IMP-031: Typed canonical settings

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Goal: typed TOML defaults with per-setting origin/scope/effect. Resolve bounded real files, preserve last valid snapshot on malformed reload, hash deterministic values for ConfigId. Tasks persist ExecutionContext; migration explicit. Current revocations intersect pinned destinations immediately. Alternatives: generic schema framework adds unnecessary abstraction; copying latest config into tasks violates pinning. Cost: explicit metadata maintained with fields. Acceptance: real-file override, malformed reload, pin/migrate/revoke tests. Symbols: config types/resolution/metadata/reload, engine task configuration methods.

## IMP-032: Local transport and input ownership

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Goal: owner-only Unix socket in private non-symlink runtime directory. Never unlink preexisting paths. Bounded client concurrency, message deadlines and frame limits; one request at a time per connection. Validate peer UID with safe rustix API. Input lease requires session, client and controller epoch; reconnect preserves lease and epoch rotation invalidates it. Native dispatch is unavailable. Alternatives: TCP broadens exposure; UID alone cannot isolate same-user workers and is not Task 2 acceptance. Cost: macOS/Unix-specific endpoint plus pending admitted-boundary test. Acceptance: real socket replay, stalled client, saturation, path permissions and exclusive leases. Symbols: engine server, Client, lease methods, CLI.

## Verification ledger

Pending initial red runs. This record is prospective, not retrospective. Function-level coverage will be expanded as declarations are added. No original intent is inferred for existing qualification code.

Protocol red: four tests failed on empty identity, unsupported version, frame bounds and canonical cursor. Green: 15 library tests. Settings red: override ignored and malformed reload accepted. Green: both real-file checks. Storage red: four behavior failures. Green: all four real SQLite checks after correcting test directory modes and canonicalizing the verified macOS parent before SQLITE_OPEN_NOFOLLOW.

Transport red: missing socket and preexisting file accepted. Sandbox implementation run: path tests passed, socket bind EPERM. Authorized host run: two passed, zero ignored. Lease tests were added alongside implementation, not observed red; they are additional coverage, not claimed test-first evidence. No mocked provider/engine responses are runtime evidence.

## IMP-033: State and endpoint lifetime bounds

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Date: 2026-10-17. Goal: all authoritative state remains recoverable within the bounded wire protocol. Limit projects/tasks to 128 each and their complete serialized snapshot to 63 KiB. Reject the transaction at capacity, preserving the prior snapshot. Event pages are bounded independently of retention. Durable operation history remains on disk for idempotency and is not silently evicted. Cost: a concrete initial state quota; revisit with explicit snapshot pagination before increasing it. Graceful server shutdown unlinks only the socket inode/device created by that instance. Existing paths, including stale sockets after a crash, fail closed and are never silently removed. Restart can select a fresh socket filename while reopening the same database; a supervisor may later own explicit stale-endpoint recovery. No native capability is inferred.

## IMP-034: Dedicated-worker deadlines

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Date: 2026-10-17. Goal: bounded callers without false rollback claims. Queue insertion uses try_send and explicit saturation errors; response waits have configured deadlines. A timed-out write can still commit, so report outcome_unknown_retry_same_identity. Retrying the same durable identity recovers the committed outcome. SQLite busy timeout is 250 ms. Task config snapshots will be stored alongside tasks so a restart retains original settings, not only their hash. Revocation filtering applies to every context retrieval, including replay. Alternatives: cancelling a queued mutation introduces acknowledgement races; copying latest settings breaks pinning. Revisit with broader task commands. Evidence: real reopen and read-only database rejection tests.

## IMP-035: Cargo entrypoint remains explicit

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Date: 2026-10-17. Owner supplied a real integration failure on leader source 77e540f after Task 2 added crazyagent-boundary-child; shared graph refreshed after 381d6d6. Goal: preserve the documented cargo run --offline -- qualification report invocation with multiple binary targets. Decision: default-run = crazyagent in the package manifest. Alternative: document --bin everywhere, rejected because it changes the established entrypoint. Cost: one manifest key and one real Cargo regression check. The test copies actual production source and manifests into disposable real files, retains the actual probe when present or adds a competing exit-only binary when absent, then runs actual Cargo and parses the real qualification CLI output. No provider or engine response is fabricated. Red evidence: actual Cargo exit 101 naming both targets and requesting default-run. Revisit only if the primary executable changes. Symbols: cargo_entrypoint test and package.default-run; original qualification functions remain untouched.

## IMP-036: Input invariants and bounded epoch history

Date: October 17, 2026. Task: Task 5. Source: contemporaneous durable-engine worker handoff.

Date: 2026-10-17. Goal: no stale lease becomes valid after release or controller transfer. Require a fresh controller epoch after release, remember up to 4096 session/epoch pairs in this engine lifetime and cap registered sessions at 128. Assert that each lease has its matching session/epoch, is not retired and belongs to recorded history. The internal orphaned-lease test trips a production runtime assertion. These are engine authorization checks, not native input/resize evidence. Native runner epoch recovery remains Task 3 integration. Alternatives: reusing a released epoch resurrects stale leases; unbounded epoch history violates resource bounds. Cost: explicit epoch/session capacity errors. Revisit only when qualified runner integration supplies a durable epoch lifecycle.

## Final production-symbol coverage index

Each entry maps actual production declarations to decisions above. Formatting/error wrappers are included; existing qualification implementation is not retroactively attributed to this worker. Derive-generated Serde/Eq/Ord/Hash implementations inherit the declaration's decision.

| File and symbols | Decisions | Rationale and verification |
| --- | --- | --- |
| protocol.rs: PROTOCOL_VERSION, FRAME_BYTES_MAX, ProtocolError, ProtocolError::fmt, validate_identity, identity macro | IMP-029 | Strict boundary and explicit errors; protocol red/green |
| protocol.rs: ProjectId, SessionId, TaskId, PlanId, ConfigId, SnapshotId, BaselineId, EnvironmentId, ClientId, RequestId, ControllerEpoch, CredentialRef; each generated new/as_str/TryFrom<String>::try_from | IMP-029 | Validated caller-generated safe string identifiers; rejects_invalid_request |
| protocol.rs: Cursor, Cursor::new, Cursor::value, Cursor::try_from, String::from(Cursor) | IMP-029, IMP-030 | Canonical string serialization, checked SQLite signed-range advancement; cursor tests |
| protocol.rs: ProjectSpec, ExecutionContext, Command, WireRequest, WireRequest::validate | IMP-029, IMP-031 | Closed command schema, references only, bounded project/context fields |
| protocol.rs: StoredProject, Event, Outcome, WireReply, encode_frame, decode_request | IMP-029, IMP-030, IMP-033 | Bounded versioned exchange and ordered cursor outcomes; framing and socket checks |
| config.rs: Settings, Settings::default, Origin, EffectTiming, SettingScope, SettingDescriptor, EffectiveConfig | IMP-031, IMP-033 | Seven typed settings and limits; real override metadata test |
| config.rs: EffectiveConfig::validate, ConfigError, ConfigError::fmt, ConfigFile | IMP-031 | Validate bounds, descriptor shape/values/defaults and content hash; config identity checks |
| config.rs: resolve_config, read_config, apply_file, validate_settings, from_settings, ConfigState, ConfigState::reload | IMP-031 | Bounded real TOML, linked defaults and project override, union revocations, all-or-nothing reload; malformed file tests |
| storage.rs: Job, Storage, Storage::open, error | IMP-030, IMP-034 | Bounded dedicated worker with explicit startup and request results |
| storage.rs: open_database, lock_database, initialize_schema | IMP-030, IMP-032, IMP-033 | Owner-only files, no-follow SQLite open, canonical verified parent, private separate flock file, FULL/WAL persistence; real ownership/reopen tests |
| storage.rs: apply, finish_operation, execute | IMP-030, IMP-034 | Atomic identity/result/revision/event commit. Mutating replay preserves exact original outcome; reads re-execute under current revocations after byte-identity validation |
| storage.rs: create_project, snapshot, read_rows, enforce_snapshot_bound | IMP-030, IMP-033 | Complete bounded snapshots, atomic quota rollback; real state-quota test |
| storage.rs: events, persist_event | IMP-030, IMP-033 | Retain configured event count, return at most 128 per page, future/gap resync; cursor-gap test |
| storage.rs: task_context, pin_task, migrate_task, store_config | IMP-031, IMP-034 | Persist full pinned config with context, explicit migration, current revocations on reads; pin/revoke/reopen tests |
| engine.rs: InputLease, InputState, InputState::assert_consistent | IMP-032, IMP-036 | Exclusive control invariants, bounded state and assertion-trip check |
| engine.rs: EngineError, EngineError::fmt, ClientError, io_error | IMP-029, IMP-034 | Explicit errors and unknown-outcome distinction |
| engine.rs: Client, Client::new, Client::request | IMP-032, IMP-034 | Owner endpoint/peer validation, frame and response identity checks, overall deadline; real socket reconnect tests |
| engine.rs: Server, Server::bind, Server::run | IMP-032, IMP-033, IMP-034 | Private Unix listener, bounded clients, isolated stalled exchanges, bounded shutdown and exact-created-inode unlink |
| engine.rs: verify_directory, verify_endpoint, verify_peer, read_frame, serve_client | IMP-029, IMP-032, IMP-034 | Permissions, real peer UID, bounded frame/deadline and explicit closure on invalid frames; no worker-isolation claim |
| engine.rs: Engine, Engine::open, Engine::open_readonly, Engine::apply, Engine::update_config, Engine::close | IMP-030, IMP-031, IMP-034 | Dedicated worker admission, bounded queue and waits, persisted config changes, write-failure checks |
| engine.rs: Engine::set_controller_epoch, Engine::acquire_input, Engine::authorize_input, Engine::authorize_resize, Engine::release_input | IMP-032, IMP-036 | Current exclusive lease and non-reused epoch; engine-only authorization tests, no native dispatch |
| bin/crazyagent.rs: changed main routing and USAGE, new run_engine | IMP-032, IMP-034, IMP-035 | Bounded argument shape, existing qualification operations preserved, local engine startup and Ctrl-C shutdown; actual process and Cargo checks |
| lib.rs: config/engine/protocol/storage exports; Cargo.toml dependencies/default-run; Cargo.lock | IMP-029 through IMP-035 | Accepted shared-file scope; Cargo-generated offline lock, no new acquisition |

## Additional implementation costs and evidence limits

The SQLite flock must use a separate engine-lock file: macOS SQLite locking conflicts with flock on the actual database inode. This was found by a real failed run and corrected. SQLite work is only on its dedicated OS thread. Configuration parsing is synchronous and happens before CLI reactor startup; caller-driven reload is bounded to 64 KiB per file. Bulk activity processing is not implemented here; the typed bulk_queue_capacity is the downstream capture boundary setting, not capture-runtime evidence.

New direct dependencies: rusqlite 0.40.2 with default features disabled (system SQLite, MIT), tokio 1.53.2 with rt-multi-thread/macros/net/io-util/sync/time/signal (MIT), toml 1.1.6+spec-1.1.0 with parse/serde only (MIT OR Apache-2.0). rustix retains fs and adds process, matching Task 2's reported shared addition. No net feature is needed. Twenty lock packages were added from the existing cache. libsqlite3-sys 0.38.2 uses its declared build.rs to locate/link system SQLite; no bundled SQLite compilation/download or lifecycle-hook acquisition was introduced. System sqlite3 reports 3.51.0. No dependency request is outstanding.

Observed behavioral reds exist for protocol, settings, durable storage, pin settings retrieval, replay revocation, released-epoch reuse, Cargo binary selection and CLI process startup. Existing supplemental tests for saturation/quota/client scoping and the internal invariant were green on first execution; they are not retrospectively described as missing-behavior reds. The initial exclusive-lease checks were added alongside implementation; the later released-epoch regression has an observed red/green pair. No broad future features were added.

Graph remains leader-owned. No graph update was run from this worker. Code/security reviews remain deferred under DEC-061. worker_cannot_control_engine against the actual admitted Task 2 boundary remains pending, as do native runner input/resize and provider qualification. Passing owner-only socket checks does not establish same-user sandbox isolation.

## Final verification and commit

Local commit: 092666284accda02430701aaaa0b0a9f72dd7c77. Twelve scoped files, 2588 insertions, one deletion. Final make check exit 0: formatting, strict lint, 35 real tests, build, the same 35 tests again; zero ignored. The initial 33 increased by one invariant-trip check and one actual Cargo default-entrypoint check. The final log is .local-state/evidence/make-check-final.log, SHA-256 8790f2d859da58ef8a54462756191a792b01aa65de65ba53ac15309a8ea118df. Qualification source/tests have zero diff from 154ab76; isolation/probe files were untouched. Full handoff: .local-state/task-05-report.md. Tracked checkout clean after commit; preexisting untracked .serena/ preserved. No push, publication, global changes, provider calls or review jobs.


## Task 5 replay correction and Task 2 resource prototype consolidation

Date: October 17, 2026. Source: worker-owned decision records and retained native evidence. Historical pending statements below describe the point when each choice was made; the leader's integration record following them supplies current results.

## IMP-037: Authenticate retained peer identity after disconnect

Date: 2026-10-17. Task: leader's integrated engine_process commit-observation failure. Goal: persist a complete authorized request even when its sender disconnects before accept, preserving exact replay after a lost acknowledgement. Constraints: no weaker assertion, no longer polling, no unsafe Rust, no provider/Docker/global changes, native worker boundary still pending.

Root cause: Tokio 1.53.2 macOS peer_cred first invokes getsockopt LOCAL_PEEREPID, which returns ENOTCONN after a peer disconnects. serve_client exits before read_frame and Engine::apply, so SQLite correctly contains no operation. The E2E was nondeterministic because server scheduling decided whether peer_cred ran before client close. Exact minimal PATH reproduced the unchanged process test failure. A new deterministic real engine/socket/SQLite regression writes the whole request and disconnects before starting accept; it fails with the same missing commit. Temporary diagnostics record peer=Socket is not connected and no frame/DB call. A separate disposable macOS socket probe established LOCAL_PEEREPID fails while getpeereid succeeds with the correct UID and all synthetic bytes remain readable.

Decision: on macOS, use the safe cached nix getpeereid wrapper for precisely the UID authority already required, without making peer-PID availability a prerequisite. Keep the existing safe Tokio path on other platforms. No fail-open fallback on credential errors. Alternatives: ignoring credential errors weakens authentication; adding handshake and changing when the test disconnects avoids the accepted scenario rather than fixing it; moving the same PID query closer to accept cannot eliminate a close-before-accept race; unsafe local FFI violates project policy. Cost: one cached macOS-only dependency and its feature-minimal lock closure. Revisit when Tokio's macOS API can safely expose retained UID credentials independently of a live PID. Symbols: engine::verify_peer, target dependency selection, new complete_request_commits_when_peer_disconnects_before_accept regression. Acceptance: deterministic regression green, unchanged process/replay test green repeatedly with exact minimal PATH, full make check test/build/test green, no isolation/qualification changes. Implementation and green evidence pending at the time of this entry.

Completion evidence: c56d93f2aed54fcf5ca7a70fa6c64eb04cb79bda. Four scoped files, 110 insertions, one deletion. Deterministic behavior red/green retained; real socket without kernel credentials rejected. The unchanged process test passed 20 consecutive exact-minimal-PATH runs. Final exact-PATH make check passed 37 tests/build/37 tests, zero ignored. The original engine_process test has zero diff; no polling or timeout changed. Cached nix 0.28.0 socket/user features add memoffset 0.9.1, cfg_aliases 0.1.1 and autocfg 1.5.1; no acquisition required. Full diagnostic, trust-boundary analysis, hashes and handoff are in .local-state/task-05-replay-fix-report.md. This is a pre-admission authentication correction, not a SQLite replay algorithm change. Write completion is not accepted as persistence proof; actual stored row/reply remain required. Native worker-boundary qualification is pending and reviews are deferred.


Date: 2026-10-17. Task: bounded development-only Docker prototype under DEC-069. No permanent runtime adoption, native admission, provider calls or review/publication authority.

## IMP-038: Fixed experiment and supervisor

Goal: collect resource-enforcement evidence without adding product dependencies. Use installed Python stdlib, the exact acquired immutable image, fixed CLI/config/Unix endpoint and five fixed workloads. Rejected a Rust engine change, Docker SDK dependency and configurable arbitrary commands because each expands scope or acquisition. Pros: offline, small dependency surface and repeatable real probes. Cost: development-only Python code; no production control-plane qualification. Risk: existing daemon may be inaccessible from the coding sandbox; preserve failures and the unchanged integration command rather than modifying access. Acceptance: exact HostConfig/cgroup evidence, successful controls, OOMKilled, PID EAGAIN and CPU throttling. Revisit only for owner-approved permanent runtime work.

## IMP-039: Lifecycle and bounded evidence

Goal: prevent unbounded work and accidental mutation outside this slot. Use a local exclusive lock, unique run/probe labels, create stopped, verify full identity/security/resource settings before start, stream stdout/stderr with a fixed cap, enforce a 15-second external execution deadline and reconcile exact labels on all exit paths. Remove only a verified full container ID after postmortem inspect. Rejected docker run --rm because it loses postmortem evidence; rejected broad cleanup and client-timeout-only cleanup because they cannot establish ownership or termination. Pros: attributable artifacts and fail-closed cleanup. Costs: extra inspect/list calls and explicit unavailable results if cleanup cannot be proven. Risks: hard supervisor/host death remains outside Python finally guarantees. Revisit for product crash recovery.

## IMP-040: Measurement semantics

Goal: distinguish resource enforcement from workload errors. Compare small allocation and six-child controls against bounded 96 MiB OOM and the same six-fork workload under four PIDs; collect cgroup limits/counters before/after where the process survives. Measure one CPU worker for 3.5 seconds at 0.5 CPU, require positive throttling and capped usage with explicit scheduling tolerance. Rejected allocation exceptions alone and per-process CPU-time inference. Pros: kernel evidence plus successful workload controls. Costs: Docker integration requires actual daemon access; OOM post-counter capture may be unavailable after cgroup removal. Risks: CPU accounting varies with scheduling; report tolerance and raw counters. No claim of aggregate CPU budget or filesystem/network/credential/native qualification. Revisit when full runtime qualification is authorized.

Function-symbol mapping and verification will be appended with the implementation. Shared tracked decision consolidation and graph refresh belong to the leader.

## IMP-041: Deadline implementation and real local TDD

Goal: prevent even starting a client with an expired deadline, and leave time for Docker termination dispatch inside the approved 15-second running ceiling. The start/attach client has a tighter 14-second watchdog. A fixed sixth deadline workload sleeps for 30 seconds but must be killed by that watchdog; actual Docker StartedAt/FinishedAt must remain within 15 seconds. No second CPU worker is needed for the 3.5-second CPU probe. The five resource probes remain unchanged in purpose. Rejected treating a killed Docker client as a stopped container; exact verified-ID kill plus exact-label reconciliation and stopped postmortem evidence are required. Cost: one additional sequential, low-resource deadline execution during native integration. Risk: unavailable/slow Docker control plane cannot guarantee termination; the test fails and cleanup uncertainty stops further probes. This is not production crash recovery.

The existing deadline test was extended before the guard existed. The real function failed with ValueError not raised when passed an already-expired deadline (.local-state/deadline-red.log). Implementation now rejects that input before Popen. The same test then passed alongside real output-bound and invariant tests (.local-state/container-local-green.log). The earlier missing-script integration failure remains separately classified as missing-source evidence only.

## Complete production-symbol map

| Symbols | Decisions | Verification and remaining gap |
| --- | --- | --- |
| invariant | IMP-038, IMP-039 | Real true/false calls exercised; false raises AssertionError. Uses explicit raise so Python optimization cannot strip it. |
| run_command | IMP-039, IMP-041 | Real Python stdout/stderr each produced 100000 bytes, retained 32768 each; real five-second sleep client was terminated at 0.3 seconds; expired deadline red/green passed. |
| save | IMP-039 | Actual command/error/report artifacts written by failed real Docker preflight. |
| docker | IMP-038, IMP-039 | Actual installed CLI invoked with fixed config/Unix endpoint and sanitized environment; permission denial retained, no substitute daemon used. Successful native calls remain pending. |
| require | IMP-038, IMP-039, IMP-040 | Used in actual preflight for empty-auths config; other trust-boundary failures remain pending native integration. |
| inspect_container, verify_identity, verify_stopped | IMP-038, IMP-039 | Full-ID, exact-label/name/image and stopped HostConfig checks implemented; runtime coverage blocked before create. |
| cleanup | IMP-039, IMP-041 | Exact unique label, at most one match, verified full-ID stop/postmortem/remove/relist implemented; native execution pending. No broad selector, prune or image mutation. Interrupted create with no reconciled ID is explicitly uncertain and stops the sequence. |
| counters, evaluate | IMP-040, IMP-041 | Actual cgroup parsing and state/counter/usage/deadline assertions remain pending native execution. CPU tolerance is 0.10 to 0.65 CPU plus 100ms accounting allowance over 3.5 to 5 wall seconds. OOM retains pre-counters and State.OOMKilled; post-OOM cgroup capture is explicitly unavailable. |
| execute_probe | IMP-038, IMP-039, IMP-040, IMP-041 | Fixed create/verify/start/finally-cleanup lifecycle implemented; native execution pending. No next probe on unavailable or uncertain cleanup. |
| preflight | IMP-038, IMP-039 | Actual empty-auths config read; actual Docker info denied by sandbox before image inspection or any create. Image platform/digest/size revalidation remains pending. |
| interrupted | IMP-039, IMP-041 | SIGTERM routes through the same interruption/cleanup path; this signal path has not been exercised. Hard supervisor death remains an explicit qualification gap. |
| main | IMP-038, IMP-039 | Actual CLI returned nonzero and persisted unavailable report, with no admission/runtime-selection claim. Full successful loop awaits native execution. |
| embedded snapshot, emit | IMP-040 | Embedded Python compiled successfully; real in-container cgroup reads/structured streaming remain pending native execution. |

All 15 supervisor functions and both embedded functions are mapped. No fabricated daemon/provider reply, mock, skip or ignored test was used. Coverage here means decision coverage, not executed runtime coverage.

## Freeze and ownership handoff

Owner steering: the leader confirmed the sandbox denial and will execute the native command under existing DEC-069 authority. Resource ownership was returned to the leader immediately. No further Docker calls were made after that instruction. The four active tests remain intact; the three local checks were selected for local verification only. Full native integration remains mandatory.

Git add was attempted only for the two scoped Python files. The sandbox denied creation of the shared worktree index.lock; no escalation or commit was attempted. Source remains intact for leader staging. Tracked Rust/docs and shared graph are unchanged. Graph refresh and tracked decision consolidation remain leader-owned; reviews remain deferred under DEC-061.

## IMP-042: Python 3.9 Docker timestamp precision

Date: 2026-10-17. Task: repair the shared timestamp conversion after native run 74ad4c934ea44520a53c5f80430f5794. The owner reported six actual containers, preserved outcomes and clean_container_confirmation=true; inspection of the retained report confirms all six failed at the same Python 3.9 fractional timestamp parser. No new Docker calls are authorized for this repair, and the leader retains the slot.

Goal: accept actual nanosecond RFC3339 StartedAt/FinishedAt values for every probe while retaining measured durations and all deadline thresholds. Regression first replays the actual recorded CPU cgroup stream, state and execution metadata in production evaluate. The first execution reproduced ValueError at the original fromisoformat call with 2026-10-18T02:53:23.385560418+00:00; see .local-state/timestamp-red.log. This is an executed production-path parser failure, not a missing-module failure or a new enforcement run.

Decision: add one shared docker_timestamp_ns converter. Validate RFC3339 syntax with zero through nine fractional digits; parse only the whole-second date and timezone using Python 3.9 datetime.fromisoformat, and combine integer UTC epoch seconds with the right-padded fractional nanoseconds. Both StartedAt and FinishedAt callers in evaluate use it. Preserve the original floating seconds assertion and add an exact integer nanosecond bounds check before it. The reported running_nanoseconds retains the complete measured difference; running_seconds divides that small difference by one billion for the existing report/test interface.

Alternatives: truncating to six decimal places is shorter but loses up to 999 nanoseconds per timestamp and can change an at-threshold decision; using datetime.timestamp on the full epoch introduces avoidable float rounding; adding a date library or upgrading Python violates the offline minimal-tooling scope. Pros: Python 3.9 compatible, no dependencies, UTC offsets supported, exact nanosecond threshold decisions. Cost: one small pure conversion function and an extra integer report field. No sub-microsecond input precision is discarded; only running_seconds has ordinary binary floating-point representation rounding after exact subtraction. Raw native timestamps remain untouched in artifacts. Malformed, timezone-free or greater-than-nine-digit values are rejected rather than guessed.

Acceptance: the actual observed CPU pair gives exactly 3751966793 nanoseconds and 3.751966793 reported seconds, UTC Z and numeric timezone spellings agree, fractional widths and invalid inputs are checked, all four prior tests remain unchanged, and no deadline/CPU threshold is weakened. Native full-suite rerun stays leader-owned. Revisit if Docker emits a different documented timestamp grammar or precision above nanoseconds.

Function map addition: docker_timestamp_ns maps to IMP-042; evaluate also maps to IMP-042 for both native timestamp fields. All prior function choices remain in force. Shared graph query had no matching timestamp/prototype nodes; refresh remains leader-owned.

Verification: the new regression and three selected local tests passed on actual Python 3.9.6 in 0.354 seconds; .local-state/timestamp-local-green.log retains output. The regression covers exact observed nanosecond strings with Z and numeric offsets, no fraction and one/six/seven/eight/nine digits, malformed dates/precision/missing timezone, and production rejection of a duration one nanosecond over 15 seconds or below zero. Both modules and the embedded payload compile, whitespace is clean, and no comment tokens were added. Removing only the new regression reproduces the original four-test source SHA-256, proving those tests are unchanged. Offline production evaluate re-analysis of all six retained native outcomes passed with exact durations, without Docker calls or modifying the raw report. This is not a new native run. The current full-suite command now discovers five tests and still requires leader execution.

Frozen source hashes: container_resources.py = 75e096a46252e590e3cfdf7e276526f95e52b60fa1cbab3e4f48d2e2a91b9849; test_container_resources.py = cc89d320932f2184e358a59175e8aa8dfc312f0eb9e74e9061423197844dc0d2. The leader retains resource ownership; no Docker calls, reviews or publication were performed for this repair. Tracked docs and Rust remain unchanged.

## IMP-043: Integrate evidence without broadening admission

Date: October 17, 2026. Task: DEC-070 V1 continuation. Goal: land completed core and resource work with explicit evidence limits.

Decision: preserve engine 4919e1d, disconnected-peer fix 72d5b5d and development-only resource scripts f285a2b. The leader's integrated make check passed 49 real tests, build and 49 tests again, with zero failed or ignored tests. Log .local-state/integrated-engine-check-02.log SHA-256: 445110f4c9fcd0811ab877965e4375bfc1452a93cc09b1c9eb9d39d8504ab031. The initial integrated replay failure remains retained, not relabeled as a passing run.

The unchanged integrated Python suite passed five checks in 21.054 seconds. Native run cc982da2309c4153aac2745a4b02f292 proved all six resource probes and exact cleanup. Report SHA-256: ac32c2af3c0799fc38852f908c9318b1d1b6f314e2c3f36ba65dfdd0ff35. The deadline container ran 14.019154243 seconds. Source hashes match IMP-042. Native admission, aggregate CPU-time budget and permanent runtime selection remain false.

Rejected alternatives: equating resource probes with complete confinement; accepting worker-only results as integrated evidence; deleting failed attempts; or restarting completed tasks. Pros: independent core work proceeds from verified source. Cost and risk: worker endpoint denial, native recovery/auth/child control, total CPU budgets and product runtime adoption remain separate gates. Revisit on a concrete complete runtime proposal and separately approved provider scope.

Symbols: protocol/config/storage/engine and CLI choices map to IMP-029 through IMP-037; every prototype function maps to IMP-038 through IMP-042. Acceptance: preserve source lineage, actual results and open gaps; never silently admit a runtime or provider.

## IMP-044: Prove actual worker-to-engine denial in parallel with worktree ownership

Date: October 17, 2026. Task: the accepted Task 2/5 endpoint-denial gap. Goal: measure actual sandboxed worker authority against the real engine while Task 6 implements independent Git ownership.

Decision: assign one fresh visible worker only isolation.rs and a new real engine-boundary test in its own worktree; Task 6 exclusively owns core project/lease files. Reuse the already approved installed runtime and disposable native probes, with no Docker/provider/global changes. Compare legitimate host access and workspace writes against worker/descendant engine socket, state and fixture-process control attempts. Preserve any genuine enforcement failure.

Alternatives: waiting for all project work before measuring a known launch blocker adds delay; merely exercising same-UID socket checks proves no worker confinement; selecting Docker or Vercel Sandbox now exceeds the current decision. Pros: concrete missing qualification evidence with disjoint writers. Costs: native checks may need leader execution outside the coding sandbox, and macOS resource-control gaps remain even if endpoint denial passes. Acceptance: real positive and negative controls, bounded fixture cleanup, actual source/policy identity and no full-admission claim. Revisit if the runtime cannot enforce this boundary.

## IMP-045: Preserve an early rejected-admission checkpoint before further implementation

Date: October 17, 2026. Task: accepted Task 4 preparation. Goal: expose the difference between verified core behavior and an admitted native launch profile without inventing working-time or delivery measurements.

Decision: freeze source 7cfcb66b10cc415ff5d8e0452f2ed27c9c05ca6e and execute the real qualification CLI for candidate macOS Claude Code, Codex and RCLI profiles. Each generated report explicitly records all four mandatory capabilities unavailable. Each actual admission exits one, preserving confinement, live recovery, native authentication and child control requirements. Retain all six artifact hashes in docs/qualification/forecast.md and raw private artifacts under .local-state/qualification-checkpoint-20261017. The report command performs no provider execution and does not claim a configured account exists.

Alternatives: postponing visible qualification status risks confusing source/test progress with release readiness; converting resource-only observations into complete profile evidence is invalid; claiming sixteen measured working hours or a firm November date lacks a reliable ledger. Pros: explicit current proof and fail-closed launch status. Cost and risk: the November 30 target remains conditional; the 151 to 201 hours after reserve is calendar-derived capacity before subtracting work already spent, not an effort estimate. Continue independent authorized work while preparing a concrete runtime proposal and separately approved provider scope. Acceptance: no missing capability becomes a pass, no current requirement is weakened, and the checkpoint identifies outstanding integration and permission dependencies. Revisit when actual runtime/profile evidence and measured throughput change the forecast.

## IMP-046: Real fixture and qualification scope

Date: October 18, 2026. Source: worker 71e9ce7, bounded Task 2/5 proof. The worker's original unavailable observations are retained below; subsequent native evidence is in IMP-049.

Goal: measure actual host, worker and descendant access to a disposable engine socket, SQLite state and process. Constraints: only isolation.rs and worker_engine_boundary.rs are tracked write targets; no new dependencies, installations, providers, reviews or runtime admission. Use the existing engine executable, protocol, runtime 0.0.80 and bounded subprocess machinery. Use synthetic state and a sanitized environment. The fixture owns and reaps the engine; signal probes use SIGCONT, a real nonterminating signal. A short temporary root avoids Unix socket path limits.

Alternatives: same-UID permission checks or fabricated engine replies cannot demonstrate isolation; a new sandbox or engine authentication direction exceeds scope. SIGTERM would destroy the positive-control engine before descendant and protected checks. Cost: native fixture startup and a small real integration test. Risk: the outer sandbox may prevent Unix binding or nested sandbox-exec. Acceptance: legitimate host and unconfined controls work first; confined worker and descendant candidate writes work; live socket contact/control, protected reads/writes and signals are denied with actual permission errors; durable state agrees. No missing infrastructure is a pass. Resource-unqualified profile remains unavailable.

Unresolved: actual native runtime outcomes. Evidence is retained in raw logs and the worker report. Revisit if installed runtime behavior differs or native prerequisites prevent measurement.

## IMP-047: Bounded probe and raw observations

Date: October 18, 2026. Source: worker 71e9ce7, bounded Task 2/5 proof. The worker's original unavailable observations are retained below; subsequent native evidence is in IMP-049.

Symbols: isolation::run_engine_boundary_probe, isolation::EngineProbeResult, ENGINE_PROBE observe, exchange, exchange::finish and main. Use the existing bounded_command, read_bounded, write_new, write_json and record_case functions unchanged. Resolve the installed Node and srt versions before execution; require srt 0.0.80. Construct real WireRequest values using the engine's protocol types. The Node worker sends bounded length-prefixed JSON, captures the actual snapshot revision, then sends a uniquely identified CreateProject mutation. The child executes the same program once with the child role. Rust deserializes real control replies as WireReply and requires Applied for the unconfined control. Host checks verify actual candidate file contents; the integration test queries persisted operation IDs and projects independently.

Protected-state reads inspect the real SQLite header. Writes write the identical first header byte through a real writable file descriptor, demonstrating authority without corrupting the fixture. Signals use SIGCONT against the Child handle supplied by the fixture. Settings reuse the existing network and filesystem shape, with the entire trusted engine directory denied for reads and writes. Only EACCES, EPERM or EROFS observations count as denied; missing files, timeouts, closed connections, malformed replies or launch failures never count as confinement. Overall execution is bounded by the validated profile, 64 KiB output, a 1500 ms socket deadline and a 7000 ms descendant deadline. Resource gaps and default runtime write exceptions remain explicit; this helper never admits a runtime.

Alternatives: a new subprocess abstraction duplicates existing bounded machinery; fixed revision numbers hide stale-contract defects; destructive database edits or terminating signals impair subsequent controls. Pros: real engine traffic and persisted state, permission-specific errors, inherited descendant execution. Cons/costs: native execution required and no resource-enforcement claim. Evidence: initial missing-function compile red retained; probe compiles, Clippy and Node syntax pass; live engine launch is blocked by the outer sandbox before native behavior. No behavioral red/green has been observed. Revisit after native rerun or if protocol/runtime versions change.

## IMP-048: Fixture lifecycle and independent checks

Date: October 18, 2026. Source: worker 71e9ce7, bounded Task 2/5 proof. The worker's original unavailable observations are retained below; subsequent native evidence is in IMP-049.

Symbols: worker_engine_boundary::EngineProcess::drop, launch, snapshot and worker_cannot_control_engine. The test owns a real executable child in a short retained /private/tmp/crazyagent-web-* root with private sibling directories, null stdin and a cleared environment. A five-second readiness deadline polls only owned logs/process status. Drop kills only the owned still-running engine and always waits, printing the actual reap status. The host uses Client::request before and after probes; SQLite integrity, actual projects and durable operation IDs are checked separately. Raw files are retained on failure rather than deleted.

Alternatives: an in-process fixture cannot qualify signaling a separate engine; temporary long worktree paths can exceed Unix socket limits; discarding failed fixture roots loses evidence. Cost: retained small scratch roots and one engine per invocation. Risk: native startup is blocked here. Evidence: retained logs report actual engine EPERM; final focused rerun records reap status. Acceptance and revisit: complete the exact native commands in the worker report, then hand actual native observations to the leader. Reviews remain deferred and the graph cache remains leader-owned.

## IMP-049: Integrate actual native worker and descendant engine denial

Date: October 18, 2026. Task: accepted Task 2/5 integration. Goal: establish the specific missing worker-to-engine control evidence with real positive controls, without admitting an incomplete resource profile.

Decision: preserve source worker 71e9ce7, integrated as a88573d. The focused leader-native test passed against the real executable and SQLite state. In /private/tmp/crazyagent-web-LdBt4Z, unconfined worker and descendant candidate writes, protected state reads/writes, SIGCONT, socket contact and actual project-control requests succeeded. Confined worker and descendant candidate writes succeeded, while each protected read/write, signal, contact and control attempt returned EPERM. Parent-observed persisted projects contained only the host and two unconfined project IDs. The owned engine was reaped; unrelated processes were untouched. Confined evidence SHA-256: bd761695fe77447b1b68431c4c7cb4efca86b2433da7cb0e45316f191a8d0cbd.

The integrated native make check passed formatting, strict lint, 50 tests, build and 50 tests again, zero failures and zero ignored tests. Log .local-state/integrated-engine-boundary-check.log SHA-256: e6d7a23ce16dc4673a6022461d43ffced59cdf102b0cc3c780e376180184a27d. Earlier outer-sandbox failures remain infrastructure failures, not confinement passes. Reviews remain deferred under DEC-061.

Alternatives: socket-owner checks alone do not establish confinement; missing fixture paths or failed engine startup are invalid negative controls; removing resource requirements to admit the profile would weaken the contract. Pros: actual kernel denials across worker and child, durable state checked independently. Costs and risks: finite tested operations do not prove every host path or signal, aggregate CPU/memory/PID enforcement and runtime default write exceptions remain unqualified. Acceptance: retain raw observations and exact source lineage; native admission remains unavailable. Revisit when a complete supported runtime profile is proposed and measured.

## IMP-050: Admission and authority

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: explicitly admit real canonical repositories without changing existing project declaration and replay behavior. Constraints: existing ProjectSpec paths are declarations, including historical nonexistent paths; no implicit admission. Choice: add an explicit repository admission command and persist canonical root plus filesystem identity. Allocation requires a pinned task in that project and an admitted repository. Alternatives: treating project membership as immediate filesystem authority; breaking existing declaration behavior. Benefit: explicit authority and compatibility. Cost: one admission operation per repository. Failure modes: symlink aliases, replaced roots or metadata, unsupported repository layout fail closed. Acceptance: real multi-repository projects, outside-path rejection, persistent admission and ownership.

## IMP-051: Narrow Git operations

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: create useful task worktrees without executing repository hooks or content filters. Choice: fixed host Git executable, cleared environment, disabled hooks/fsmonitor/replace objects/lazy fetch, no network operations, no-checkout worktree creation, index-only read-tree and raw object materialization. Inspect local config without includes; reject unsupported configuration, shared/linked primary metadata, alternates, partial clones, submodules and tracked symlinks. Bound subprocess duration/output and file counts/bytes. Reject unsafe paths before effects and revalidate metadata identity. Alternatives: ordinary checkout invokes filters; adopting a new Git dependency is unauthorized; cloning duplicates history and does not implement linked worktrees. Costs: per-object subprocess overhead and conservative repository support. Risks: worktrees are not OS confinement; same-user processes with unrestricted host authority can mutate shared metadata. Runtime isolation remains a separate gate. Revisit when a qualified protected Git service or sandbox allows wider repository support.

## IMP-052: Crash-safe reservations and conservative release

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: never forget an exclusive writer across interrupted allocation or engine restart. Choice: commit an allocation intent in SQLite before Git effects, retain failed/interrupted ownership, reconcile recorded identity against actual Git registration on startup, and never delete/reset/stash owner files. Ready leases become recovery-required after restart; missing/replaced state becomes uncertain. Lease release requires qualified writer-quiescence evidence; this slice has no admitted native profile or supervisor, so release rejects rather than believing a client-supplied stopped flag. Alternatives: expiry or PID absence can authorize a live descendant; filesystem/SQLite rollback cannot undo Git safely. Cost: conservative retained leases need later supervisor integration. Acceptance: concurrency, interruption, dirty preservation, native-descendant references, unknown/live release rejection. Revisit only with independently qualified session-tree quiescence evidence.

## IMP-053: Status and inert remote settings

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: status must not retain Ready after a pointer swap, while ordinary cloned repositories remain usable offline. Choice: validate current filesystem and Git registration on every lease read; report Uncertain immediately on failure. Permit only inert remote URL/fetch and branch remote/merge config alongside primitive core settings. Host operations still forbid every protocol and clear inherited Git configuration. Alternatives: cached-only status missed a live symlink replacement; rejecting every remote made ordinary clones unsupported. Actual evidence: 08-status-remote-red.log has 8 passes and the two intended failures; 09-status-remote-green.log has 10 passes. Costs: status scans metadata and runs bounded Git commands. Revisit with a protected metadata service and measured larger-repository requirements.

### TDD evidence so far

01-red.log: missing module/types/commands, exit 101. 02-behavior-red.log: test fixture parent lacked mode 0700; fixed fixture privacy, not the production requirement. 03-behavior-red.log: admission returns worktree_not_implemented, exit 101. 04-green.log: compile failure because rusqlite COUNT requires signed i64; corrected type. 05-green.log: first real behavior test passes. 06-hostile-concurrency.log: test match arms had inconsistent return types; fixed fixture syntax. 07-boundaries.log: 6 real tests pass. 08-status-remote-red.log: two missing behaviors fail. 09-status-remote-green.log: all 10 tests pass. Commands are cargo test -p crazyagent --offline --test worktree_ownership. No tests skipped and no fabricated replies.

Interruption evidence: a separate instance of the real test executable opens the production Engine and SQLite database and allocates a 65-file real Git tree. The parent reads SQLite read-only, observes the committed lease, kills and waits for that process, confirms the persisted state was Reserved, then reopens the Engine. Restart reports Uncertain, rejects another owner and preserves source bytes. This is not a native harness or descendant-survival qualification. The parent does not claim all possible child processes have stopped.

## IMP-054: Real Git reconciliation and internal invariants

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: compare durable ownership to actual branch registration rather than trusting the .git pointer alone. Choice: require bidirectional worktree metadata links, stable repository/worktree inode identities, the expected symbolic branch and a resolvable commit matching Git's reported HEAD. Internal record consistency asserts that lease and admission identify the same repository and Ready/recovery states have both directory seals. Alternatives: a cached Ready value or branch name can survive deletion of the ref. Actual evidence: 11-reconciliation-red.log fails on deleted branch remaining Ready; 12-reconciliation-green.log passes 12 tests. 13-invariant-red.log lacks the helper; 14-invariant-behavior-red.log proves two corrupt records were not detected; 15-invariant-green.log passes both should-panic checks. Risks: Git and filesystem changes can occur after inspection; results do not grant process execution authority. Revisit with runner integration, protected metadata and independent quiescence evidence.

## IMP-055: Descriptor-relative materialization

Date: October 18, 2026. Task: accepted Task 6 ownership core. Source worker 84de215, integrated as c0a3731. Preserve worker evidence below; integrated results are in IMP-056.

Goal: never follow a swapped root pathname or an intermediate symlink while writing a raw blob. Choice: open and seal the new worktree directory, retain the descriptor, create/traverse at most 64 components with mkdirat/openat and NOFOLLOW, and create each file exclusively. This uses existing approved rustix and safe File ownership conversions, with no dependency or unsafe Rust. Alternatives: canonicalizing a pathname and subsequently opening it leaves a parent-symlink race. Cost: one bounded directory traversal per file; at most 128 files, 8 MiB per subprocess output and 32 MiB aggregate contents. Evidence: 16-descriptor-red.log lacks the function; 17-descriptor-behavior-red.log fails with missing descriptor behavior; 18-descriptor-green.log passes three real unit checks. That command accidentally filtered all integration tests to zero and is not integration proof. 19-refactor-green.log reruns all 12 integration tests successfully after switching production materialization to descriptors. The unit fixture renames the already-open root, substitutes an outside symlink, and proves content goes only to the retained directory; an intermediate outside symlink is rejected. Residual boundary: Git metadata is shared host state, and these operations do not isolate arbitrary same-user processes with unrestricted host authority. A moved already-open directory remains the same admitted inode; OS confinement and native execution remain separate qualifications.

### Production symbol decision map

All additions are in worktrees.rs unless qualified below. Common choices are intentional; error and rejection adapters are trivial glue.

| Symbols | Decisions | Purpose and alternatives |
| --- | --- | --- |
| error, rejected | IMP-050, IMP-052 | Preserve typed engine errors and stable rejected outcomes rather than panic on external inputs. |
| identity, canonical, bounded_read | IMP-050, IMP-051, IMP-054 | Canonical owner-controlled paths, no symlink aliases or hardlinked metadata, bounded pointer reads. Ordinary errors handle external replacement. |
| git, collect_git_output | IMP-051 | Fixed host executable, sterile environment, per-command five-second deadline, bounded concurrently drained output, no shell interpolation. |
| validate_metadata, validate_config, inspect, verify | IMP-050, IMP-051, IMP-053 | Explicit conservative admission and revalidation; unknown config, shared roots and partial stores are unsupported. Inert remote settings confer no network authority. |
| initialize, admit | IMP-050, IMP-052 | SQLite admission/lease schema and explicit membership admission, separate from historical project declarations. |
| reserve, persist_reservation, prepare, private_worktree_root | IMP-050, IMP-052, IMP-055 | Bind task/project/admission, choose deterministic task/repository branch/path, reject existing paths/branches, commit ownership before side effects. |
| validate_oid, tree_entries | IMP-051, IMP-055 | Bounded SHA-1 regular-file trees; reject attributes, gitlinks, symlinks, metadata paths and unsupported object formats. |
| load, save, assert_record_consistent | IMP-052, IMP-054 | Typed durable records, exact-row writes and test-trippable internal invariants. |
| leases, release | IMP-052, IMP-053, IMP-054 | Fresh bounded status; release is unavailable without independently qualified native writer-tree quiescence. |
| provision, materialize, write_blob_at | IMP-051, IMP-052, IMP-055 | No-checkout Git creation, raw blobs and index-only read-tree, descriptor writes; failures retain Uncertain ownership. |
| worktree_metadata, verify_record, reconcile | IMP-052, IMP-054 | Actual Git state and inode/backlink checks; restart never grants a new writer, and interrupted/failed allocations remain Uncertain. |
| protocol::WireRequest::validate | IMP-050, IMP-052 | Bound absolute repository inputs and safe native descendant references; keep existing protocol/frame checks. |
| storage::Storage::open, storage::open_database | IMP-052, IMP-054 | Pass database-local runtime root, initialize tables and reconcile before announcing readiness; readonly opens do not mutate reconciliation. |
| storage::apply, storage::execute | IMP-050, IMP-052 | Extend existing replay/revision dispatch, persist allocation intent in an independent FULL-sync transaction before Git, then record the terminal operation. Ordinary duplicate replies remain historical acknowledgments, not current execution authorization. |

Interface ruling: expose allocation through the existing Engine/WireRequest authority rather than a free function accepting only ProjectSpec/task/session, which cannot establish persistent task/project admission, operation identity or revision authority. WorktreeLease retains the planned project/task/repository/path/branch/owner fields and adds native reference and state. No runner, merge or verification authority is introduced.

Supported repositories: canonical local non-bare primary repositories with ordinary file trees and allowlisted config, including inert origin/tracking settings. Explicitly unsupported: linked primary roots, bare repositories, shared/alternate or partial object stores, hardlinked Git metadata, non-SHA-1 object formats, submodules, tracked symlinks, tracked .gitattributes, unknown Git config and repositories exceeding fixed bounds. No rejected repository is labeled qualified. These fail closed rather than silently changing checkout semantics.

Engineering costs: metadata walks cap at 8192 entries, leases cap at 128, each task lease response must fit the existing frame, tree materialization caps at 128 files and 32 MiB total, each Git call caps at 8 MiB output and five seconds, blob writing caps at 64 path components, and the materialization loop has a ten-second deadline. These are conservative availability limits, not throughput claims. The leader owns shared graph refresh; this worker never writes the shared symlink.

### Worker verification before leader integration

20-format-lint.log: make fmt-check lint passed. 21-scoped-test.log: 55 tests passed with zero failed, ignored or filtered. 22-build.log: make build passed. 23-scoped-retest.log: the same 55 tests passed again. Exact test command: cargo test --workspace --offline --lib --test cargo_entrypoint --test engine_boundary --test engine_process --test engine_replay --test worktree_ownership. Task 6 has 12 integration checks and 3 unit checks. No runner/resource/provider qualification is inferred.

10-make-check.log: intact full gate failed at qualification_boundary::boundary_denies_escape_and_trusted_writes because CRAZYAGENT_SRT was not set to the leader-verified runtime path. No timeout or test prerequisite was weakened. The worker did not run Docker or invent native qualification. Review gates are deferred; graph refresh is leader-owned. Scoped git add failed with Operation not permitted on the shared Git index.lock outside the writable worktree; no stage, commit or escalation occurred. The leader has exact commands, tested SHA-256 source fingerprints and remaining prerequisites in .local-state/task-06-report.md.

## IMP-056: Integrate real repository ownership without inventing native quiescence

Date: October 18, 2026. Task: accepted Task 6 integration. Goal: safely allocate independent task worktrees and preserve exclusive ownership and owner bytes across interruption.

Decision: commit the worker's five exact source files as 84de215 after its shared-index write was denied by the coding sandbox, then integrate as c0a3731. No source bytes were changed by the leader. The integrated native make check passed formatting, strict Clippy, 65 tests, build and 65 tests again, zero failed or ignored. Log .local-state/integrated-worktree-check.log SHA-256: 22991b6fd4199b7ed0cba6c1e5e03813e572502e21eed4fcbb041838a14affee. The 15 added checks include 12 real Git/engine integration cases and three production-invariant/materialization unit checks. Earlier scoped checks and the missing-runtime full-gate attempt remain retained in the source worktree.

The implemented ownership core validates individually admitted repositories, retains dirty owner/candidate bytes, reserves one writer before Git effects, checks native descendant owner references, allocates separate task branches, reconciles actual Git state and retains uncertain interrupted reservations. Release remains fail-closed as writer_quiescence_unqualified; no client assertion substitutes for a qualified native supervisor. Idempotent historical replies do not grant current execution authority. The source does not implement provider dispatch, native recovery, merges or verification bypass.

Alternatives: resetting or cleaning a task to simplify recovery risks data loss; accepting a stopped boolean lets unknown descendants retain write access after ownership release; ordinary Git checkout can execute repository-controlled hooks or filters. Pros: real repository state and deterministic conservative ownership. Costs and risks: current fixed resource ceilings and explicitly unsupported repository forms are restrictive. Tracked symlinks/gitattributes, submodules, linked/bare primary roots, alternate/shared/partial stores, non-SHA-1 stores and oversized trees fail closed. The initial limits are 128 leases, 128 materialized files, 32 MiB total contents, 8 MiB Git output, five seconds per Git process and ten seconds for materialization. These are disclosed implementation limits, not a claim to support arbitrary repositories or proof that the complete V1 resource-settings experience exists. Acceptance: preserve ownership and bytes on every rejected/recovered case, keep release unavailable until real quiescence evidence, and carry unsupported cases into qualification. Revisit limits/settings against measured supported-project requirements before release.

## IMP-057: Prepare a bounded combined Linux containment proposal before expanding the experiment

Date: October 18, 2026. Task: accepted Task 2/4 qualification follow-up. Goal: give the owner a concrete decision on the next missing runtime evidence while independent V1 core work continues.

Decision: preserve a proposal in docs/qualification/linux-containment-proposal.md for at most five sequential synthetic-only containers, one pinned existing image, 64 MiB RAM, 0.5 CPU, 16 PIDs, no more than 15 seconds per container and a 20-minute total allocation. The actual Rust engine remains a private host child with no container mount or endpoint exposure. Only fresh candidate, read-only synthetic canary and read-only probe directories are mounted. There is no real credential, provider, publication, privileged mode, host socket or other-workload access. No experiment has run under this proposal. DEC-069's resource-only authorization does not silently become mounted-filesystem/network/IPC experimental authority.

The leader corrects the proposed fixture root to /private/tmp/ca-q2-<run-id>, preserving the existing private mode and avoiding the demonstrated macOS Unix socket path-length limit. Existing Docker file sharing must suffice or the experiment is unavailable. The original worker proposal remains retained in its ignored worktree state.

Alternatives: immediately adopting Docker or merging native and Linux proof would misstate compatibility and qualification; probing real credentials or a mounted engine endpoint is unnecessary for this bounded non-exposure experiment; using a long project-local socket path can fail before the controls execute. Pros: reviewable exact fixtures, launch settings, observations and cleanup. Costs and risks: even a complete finite pass cannot prove native harness authentication/recovery, aggregate CPU-time limits, arbitrary escape resistance or future IPC bridge safety. Acceptance: obtain explicit owner approval before execution, preserve every unavailable/failed observation, and keep permanent adoption and native profile admission separate. Revisit only on the measured result or an owner-approved replacement scope.

## IMP-058: Continue deterministic coordination while native execution qualification remains open

Date: October 18, 2026. Task: accepted Task 7 authority core. Goal: implement real durable plan, routing/resource and scoped team-mailbox behavior without substituting fabricated native leader/provider execution.

Decision: assign a fresh visible worker in w11:p3, isolated feat/leader-routing worktree, exclusive routing/coordination and necessary protocol/storage/config integration scope. Use the verified 65-test local trunk. Default human plan approval, required-target binding, approved fallback, fair sharing, pinned configuration, bounded all-role reservations and explicit cross-task/project scope follow existing DEC records. Engine-owned authority cannot be created by memory content, a model proposal or a supplied approval/admission flag. No actual profile is admitted, so dispatch and the real RCLI-to-Claude/Codex workflow stay unavailable.

Alternatives: waiting for native authentication before implementing deterministic Rust policy delays independent work; simulating leader replies or child states would not prove the accepted integration; giving multiple workers these shared core files creates conflicting writers. Pros: actual engine/SQLite plan and message state can be tested independently. Costs and risks: this is the authority core of Task 7, not its complete model-connected qualification. Acceptance: real TDD and restart/idempotency tests, preserved current gates, a complete function decision map, no ignored/fake provider tests and explicit native gaps. Revisit when admitted profiles and account scope permit the actual leader workflow.

## IMP-059: Build the frozen RCLI dependency closure without running a model turn

Date: October 18, 2026. Task: accepted Task 3 source-build preparation under DEC-068. Goal: remove the actual dependency/build prerequisite while preserving the separate native profile and account gates.

Decision: reuse installed Bun 1.4.3 and sandbox-runtime 0.0.80, the pinned RCLI source 51bee17e02344de5fffd89523ce2b73d5042f216 and its unchanged upstream lock. Acquire only the frozen production install from the explicit official registry with project-local cache/config, cleared environment, disabled lifecycle scripts and no-save. Bun reports 172 installed packages; inventory records 176 physical package instances whose name/version entries all occur in the frozen lock. All thirteen direct production roots are present. The dry-run printed development entries and is not treated as proof that they were installed. Actual package.json/bun.lock/bunfig.toml bytes remained unchanged.

Use a direct Bun-target source build under an all-network-denied profile instead of the upstream build/prepare scripts. It bundled 1347 modules into a 4.31 MB entrypoint, returned zero and preserved frozen inputs. Run only its help path under all-network denial, disabled automatic .env loading and auto-install, with home reads denied except explicit isolated source/tool/configuration paths. Help returned zero in 0.45 seconds. No login, model turn, provider account, global config or tool installation occurred. The full current artifact/hashes are in docs/qualification/rcli-host-readiness.md; the private installation inventory SHA-256 is d9a050f62a58af281846ce91901e8241070884d9ce3b1b2fad8be67c87f56e57 and built entrypoint SHA-256 is 6ab5214c4f251566db49304219c2a2115b605df0ec786b975e4f249b0f235d5b.

Alternatives: a similarly named npm package is not the accepted RCLI source; unfrozen installs or lifecycle execution add unnecessary supply-chain/global-hook effects; a successful help/build is not authentication, live-turn, permission, persistent transport or recovery evidence. Pros: real reusable source is locally buildable without an agent-loop rewrite. Costs and risks: upstream has no stable public library exports, the CLI still handles a single headless prompt, dependency integrity relies on Bun enforcement rather than an independent tarball audit, and native account/runtime integration remains unqualified. Acceptance: keep source/lock/config/artifact identity, reuse actual RCLI controller behavior, and do not translate this preparation into native profile admission. Revisit the persistent-host seam and separately approved account scope during Task 3 implementation.
