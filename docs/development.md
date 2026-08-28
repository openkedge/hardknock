# Persistent development

V0.6 projects canonical evidence into local Agent, Repository, TaskFamily, or SharedLocal profiles. An agent subject can optionally pin version/model; its default includes successive versions. Repository paths are canonical. Task families use explicit selectors. SharedLocal aggregates the local store and does not grant broader execution authority. Workspace and organization aggregation are reserved, not implemented.

## Profiles and windows

```bash
hardknock profile show --agent claude
hardknock profile show --repo /path/to/repository --last-days 30
hardknock profile show --task-family dependency-maintenance --last-experiences 100
hardknock profile show --shared --since 2026-08-01
hardknock profile snapshot --agent claude
hardknock profile history --agent claude
hardknock profile compare --agent claude --from 2026-08-01 --to 2026-08-28
hardknock profile compare --from <snapshot-id> --to <snapshot-id>
hardknock profile gaps --agent claude
hardknock profile rebuild --agent claude
hardknock profile export --agent claude --output /tmp/claude-profile.json
```

`--since` accepts RFC3339, a UTC date, or `30d`. Comparison dates choose the latest stored snapshot on or before that date's end; timestamps choose the latest at or before that instant. They do not reconstruct imaginary historical snapshots. Snapshot IDs select exact observations. Show/rebuild updates the disposable cache; only snapshot/episode operations create immutable snapshots.

Windows select observations used in rates. The artifact inventory describes the current known revisions, not exclusively artifacts created in that window. Counts of owned Experiences and the snapshot's evidence IDs can differ: metric provenance also links associated experiment/response arms executed by another adapter. An agent profile is not a leaderboard.

## Episodes and growth

```bash
hardknock episode start dependency-maintenance --agent claude
# Perform tasks through the existing run/Bridge APIs.
hardknock episode finish <episode-id>
hardknock episode list
hardknock growth --agent claude
hardknock timeline --skill deploy-rolling-update --limit 100
hardknock timeline --lesson <lesson-id>
hardknock timeline --agent claude --since 30d
```

An episode captures an all-time before snapshot and an episode-window after snapshot, with links to observed Experiences and learning outcomes. Finish is idempotent. Growth prefers the last two completed episode windows. Cumulative snapshots overlap, so their deltas are descriptive and do not establish a trend. Different subjects, policy hashes, incompatible window types, nonchronological snapshots, unknown metrics, or fewer than five samples produce `InsufficientEvidence` by default. The five percentage-point threshold is a heuristic, not statistical significance. Regressions recommend inspection; they never launch a curriculum.

Timeline includes Experiences, Lesson/Skill/Reflex/Recovery/package revisions, episodes and benchmarks. It scans at most the latest 20,000 events before applying optional filters; returned records are capped by `--limit`. For older evidence use the artifact-specific history commands. No compaction deletes canonical records.

## Freshness and maintenance

```bash
hardknock experience health --repo /path/to/repository
hardknock experience maintain --repo /path/to/repository
hardknock revalidation list
hardknock revalidation run <revalidation-id>
hardknock lesson history <lesson-id>
hardknock skill history deploy-rolling-update
hardknock skill revise deploy-rolling-update --experience <successful-experience-id>
hardknock doctor
```

Health is read-only. Maintain records deduplicated review requests and possible duplicate Lessons. It does not execute, merge, broaden scopes, retire objects, or delete evidence. Lesson queue execution reuses the existing two-arm experiment engine, with recorded revision, commit and runtime checks. Reflex/Recovery revalidation currently directs the user to `skill harden` or their paired test commands. A changed tested claim/scope/action still requires a new Lesson hypothesis; rationale and evidence revisions remain in Lesson history.

Freshness considers the last supporting observation, runtime fingerprint, repository commit, and retained origin context after transfer. Age alone yields Aging, not invalidation. Old evidence plus changed material context yields Stale. Contradictions remain visible and suppress automatic advice even after later support. Supported/validated Lesson ranking uses a common freshness multiplier in cold retrieval and the Bridge cache: Fresh 1.0, Aging 0.9, Stale 0.4, Contradicted 0.0. Scope gates still apply. Cached automatic activation excludes Stale items. Latest support is an evidence freshness heuristic, not proof of safety across every prior context.

```toml
[development]
aging_after_days = 30
stale_after_days = 90
min_trend_samples = 5
rate_change_threshold = 0.05
max_lessons = 5
max_reflexes = 3
max_recoveries = 3
bridge_context = false
```

The optional Bridge bundle contains bounded item IDs, known unknowns, stale/contradicted references and recommendations. Full raw artifacts are not sent. Its serialized response must fit the existing Bridge byte cap; the optional bundle is omitted if it cannot fit. Session/context requests may read projections; pre-tool decisions remain in memory without database access, model calls, or experiments.

## Packages and trust

```bash
hardknock skill package deploy-rolling-update
hardknock skill package history deploy-rolling-update
hardknock skill package diff deploy-rolling-update --from 1 --to 2
hardknock skill package export deploy-rolling-update --revision 2 --output /tmp/package.json
```

Package generation records a revision only when evidence content changes; generation time alone does not create another revision. Revisions pin the Skill revision and artifact versions, with a BLAKE3 hash. Old hardened metadata does not certify a new procedure revision. Earlier V0.5 package snapshots remain in their original table; numbered V0.6 package revisions begin on generation after upgrade. Skill revision 1 is backfilled from immutable original Skill evidence.

Exports use exclusive file creation and private permissions. Profile exports omit raw outputs and procedures. Package exports are local reference manifests, not self-contained executable bundles; referenced raw evidence is not copied. They are labeled untrusted when shared. No import or automatic promotion is implemented. Local provenance can include sensitive paths and user-authored text: review before sharing.

## Benchmark

`hardknock benchmark longitudinal` requires a fresh, unconfigured dedicated home and writes its finite catalog to that home. It creates isolated baseline stores under `fixtures/`, records every evaluated task, and persists a terminal result both in SQLite and `artifacts/<benchmark-id>.json`. `--output` additionally writes a new JSON file; it will not overwrite an existing file. `benchmark list` inspects recorded runs.

Episodes are initial failures, learning/recovery, related-context transfer, model replacement, and an environment update. Training uses the same paired experiment and bounded curriculum engines. Reflection memory saves a deterministic text preference derived from the initial failed Experience, without controlled testing. The benchmark never invokes external models or package managers. On cancellation/error it records partial tasks and a stop reason; it does not claim a completed benchmark. SIGKILL recovery/resume is not implemented.

See the [implementation report](implementation-v06.md) for exact denominators, measured results, and scale gates.
