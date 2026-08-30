# Hardknock V0.7 implementation report

## 1. Files created and changed

V0.7 adds `src/federation/{model,identity,redaction,service,transport,benchmark}.rs`, `src/store/federation.rs`, `src/cli/federation.rs`, migration 010, federation tests, this report, the federation guide, and a committed benchmark summary. Core, configuration, retrieval display, CLI dispatch, README, architecture, roadmap, and Cargo metadata were extended.

## 2. Schema migration

Migration 010 adds local nodes, peers, immutable received/published bundles, external objects with origin mappings and lineage uniqueness, reproductions, conflicts, provenance nodes/edges, append-only audit, revocation storage, and benchmark runs. Existing records are not rewritten. Schema 10 is transactional and newer schemas remain rejected.

## 3. Experience Node identity design

An Ed25519 public key identifies a node through `hk-node:<blake3-public-key>`. Node name/type are metadata and hostnames are irrelevant. Keys live under the dedicated Hardknock home; directories are `0700`, the private key is `0600`, symlinks and mismatched keypairs are refused.

## 4. Cryptographic signing design

Hardknock signs compact serde JSON for a typed, redacted bundle with the `hardknock.signed-experience.v1` domain separator. The public key travels with the signature so unknown signers can be authenticated and surfaced without being trusted. Payload BLAKE3, Ed25519 signature, signer/public-key-derived node ID, manifest equality, and content-derived bundle ID are checked.

## 5. Portable bundle schema

`hardknock.bundle.v1` has explicit portable Experience, Lesson, Skill, experiment summary, Reflex, Recovery, envelope, manifest, ancestry, dependency, and provenance types. SQLite rows are never serialized as a transport format. Missing object collections are empty.

## 6. Redaction strategy

Deterministic recursive redaction replaces repository/home paths, authentication headers, common token/password/secret assignments, AWS access key forms, JWT-like tokens, and known secret field names. Raw prompts, conversations, output logs, inherited environment values, and artifact bytes are excluded. Redaction precedes content addressing and signing.

## 7. Peer trust model

Peers are local records with `Unknown`, `Known`, `Trusted`, or `Blocked` producer status. Add/trust/block/remove changes are audited. No peer can declare another globally trusted, and a changed known key is refused pending manual review.

## 8. Authenticity versus epistemic trust

A valid signature proves origin and integrity. It does not prove correctness, compatibility, or safety. External evidence remains advisory even from a trusted producer. `ExperienceTrust` keeps authenticity, producer relationship, local reproduction, context compatibility, and contradiction as separate dimensions.

## 9. Context compatibility algorithm

The deterministic policy compares OS, architecture, abstract repository family, required markers, selected environment-family tags, and captured version facts with explicit matches, mismatches, and unknowns. Version-major matches receive partial credit. Scores screen reproduction candidates; they are not probabilities.

## 10. Imported experience lifecycle

Remote IDs map to new `federated-<uuid>` local IDs while retaining origin node/object/bundle. Received evidence can become context matched, reproduction recommended, locally supported, locally contradicted, or locally validated. Remote status/confidence are source claims and never directly set local validation.

## 11. Local reproduction semantics

`federate test` creates an ordinary two-candidate strategy experiment from the imported Lesson baseline/alternative and a local committed snapshot. Local checks decide outcomes. Baseline failure plus alternative success supports; the inverse contradicts; other results are inconclusive. Trial Experiences and experiment provenance are retained.

## 12. Federated conflict behavior

Contradiction keeps the signed remote bundle, local trial Experiences, both action chains, and a `FederatedConflict`. No remote claim is rewritten or deleted. Conflict commands inspect and retest; there is no majority vote.

## 13. Provenance graph implementation

Typed nodes represent Node, Experience, Experiment, Lesson, Skill, Reflex, Recovery, and Bundle concepts. Edges include derived/support/contradict/export/import/reproduce/supersede/narrow relations. `hardknock provenance` follows the connected local/remote component.

## 14. Duplicate evidence handling

Portable objects carry stable origin identity and immutable lineage hashes. SQLite uniqueness over origin node, origin object, and lineage suppresses repeated evidence. Re-export preserves the original identity and adds the receiver's distinct local trial Experiences; the benchmark receives the origin once and new local evidence once.

## 15. Federation transports

The public service is transport independent. The initial filesystem transport publishes immutable `.hkexp` files and maintains a bounded, non-secret index. It performs no network operations, Git authentication, commits, or pushes.

## 16. CLI commands added

V0.7 adds cohesive `peer`, `federate`, `provenance`, and `conflict` families, `profile federation`, `benchmark federation`, plus optional `lesson search --include-federated`. Export/publish dry runs expose exactly what would leave the machine.

## 17. Successful transfer benchmark results

Node A learned and locally validated a real fixture Lesson. Node B imported it as advisory, reproduced baseline FAIL/alternative PASS, reached locally supported, and succeeded on a future task. Federated transfer and utilization were 1/1.

## 18. Contradiction benchmark results

Node C used a different fixture where baseline PASS/remote alternative FAIL. Reproduction returned `CONTRADICTS`, generated one conflict, preserved the remote evidence, did not activate it, and succeeded with the local action.

## 19. Malicious-bundle test results

Bad signatures, payload/hash changes, invalid content IDs, path traversal, unsafe nesting/size paths, and invalid provenance references are rejected before advisory objects are stored. The measured tampered-signature rejection rate was 1/1.

## 20. Secret-redaction test results

Tests cover API tokens, AWS secret/access key forms, authorization headers, absolute `/Users`/`home` paths, secret JSON names, and token-like strings. The signed redacted serialization contains no tested secret value.

## 21. Duplicate and re-export results

Node B re-exported Node A's original Lesson with two local reproduction Experiences. Node C suppressed the repeated Lesson lineage once and imported the two new local Experiences. Duplicate suppression was 1/1.

## 22. External Reflex safety results

A source Reflex requested high-confidence `BLOCK npm install`. Import preserved requested `BLOCK` while setting local effective behavior to `ADVISE`. No enforcement or replanning occurred. The gate passed 1/1.

## 23. Performance results

The debug-profile scale test imported 1,000 signed bundles containing 10,000 external Experience objects in **11,516 ms**. Search over all 10,000 matching objects took **596 ms**, connected provenance lookup **598 ms**, and duplicate import detection **4 ms**. Gates are deliberately broad (30 s import; 5 s common queries) and results are local-machine measurements, not service SLOs.

## 24. Known limitations

V0.7 has no hosted service, public discovery, marketplace, consensus, organization RBAC, automatic publishing, Git/HTTP transport, raw artifact transport, unsigned import, key-rotation notice, full revocation propagation, feedback delivery, scheduler, or autonomous federation curriculum. Repository/task-family exports are typed but CLI priority remains objects and Skill packages. Live Claude/Codex acceptance remains pending.

## 25. Deviations and rationale

Signatures and hashes are hex strings in JSON rather than byte arrays for inspectability. BLAKE3 provides content/lineage IDs while Ed25519 provides signatures. Unknown embedded keys can be authenticated but remain `UnknownKey`; unsigned imports are refused rather than implementing a weaker parser path. Raw artifact inclusion is refused in this pass because safe artifact manifests need a separate bounded format. Local future use in the benchmark consumes advisory context explicitly; external Lessons are not silently installed into the local Lesson table.

## 26. Recommended V0.8 direction

V0.8 should add governed external effects and transactional Realities: normalized effect intent, adapter-scoped virtualization/preparation, immutable effect events, explicit authority-bound commit, receipts, stale preparation checks, idempotency, unknown-outcome reconciliation, and compensation evidence. A Git worktree cannot reverse an email, cloud mutation, database write, or message delivery.

