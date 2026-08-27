# Experience model

> Experiences are immutable evidence. Lessons are revisable interpretations of evidence.

```text
Experience ≠ Memory
Experience ≠ Lesson
Reality ≠ Experience
ExecutionRecord ≠ evaluated task success
```

## Implemented substrate records

`RealityId`, `ExecutionId`, `ExperienceId`, `LessonId`, `ExperimentId`, `ReflexId`, and `RecoveryId` are distinct types. Their textual form is a resource prefix followed by a canonical UUID. Parsing and JSON deserialization reject mismatched types and path-like IDs. Future artifact types have identifiers only, not working engines or operational schemas.

| Record | Current contents |
| --- | --- |
| `StateRef` | Canonical repository path, full starting commit, tree hash |
| `Reality` | ID, optional parent, root, starting state, creation time, status, automatic-cleanup eligibility |
| `AgentIdentity` | Adapter kind and executable; version/model are optional and currently unset |
| `ActionRecord` | Explicit program/argv, working directory, start time, duration, exit code/signal, stdout/stderr references |
| `ExecutionRecord` | ID, Reality reference, starting state, task, agent identity, process status, action, diff reference |
| `ArtifactRef` | Absolute local path, BLAKE3 content hash, byte count |

Process states are `succeeded`, `failed`, `interrupted`, and `timed_out`. These describe the process, not whether an agent fulfilled its goal. Reality states are `created`, `running`, `completed`, `failed`, and `discarded`. A discarded Reality's execution evidence remains available.

SQLite stores structured JSON records with primary IDs and a Reality foreign key. Migration 1 creates the schema and append-only triggers for execution updates/deletes. Reality metadata is mutable; execution insertion is not an upsert. Reopening the store reruns only unapplied migrations and rejects a database from a newer schema version.

Large stdout/stderr and patches live in the artifact directory, not SQLite rows. `metadata.json` mirrors the execution record. Hashes make content changes detectable when recomputed; this is not tamper-proof storage, a cryptographic ledger, or automatic verification on every read. Artifacts and database files remain editable by their owner.

## Planned Experience and Lesson records

Milestone 4 will combine an execution with a goal evaluation, context, predictions, perturbations, failure observations, recovery observations, and provenance into an Experience. Raw observations must remain unchanged as interpretations evolve.

A Lesson will be a structured claim with a context selector, suspected action, preferred alternative, rationale, evidence references, discovered/supported-by identities, and a transparent confidence score. Supporting and contradicting trials must both remain inspectable. Reflection alone creates a Candidate, not a validated fact.

```text
Candidate → CounterfactuallySupported → Validated
                  ↓                        ↓
             contradiction / retest / retirement
```

One differential experiment can support a claim. Validation will require configurable replication criteria. Both-fail and both-pass pairs are inconclusive. Confidence will be a dedicated heuristic policy, explicitly not a calibrated probability or mathematical causal proof. None of these transitions, confidence scores, or lesson operations are implemented in Milestones 0–2.
