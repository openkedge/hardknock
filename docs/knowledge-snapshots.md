# Knowledge snapshots and replay

Migration 023 stores immutable hierarchy revisions and operational artifact revisions once. A snapshot stores their references, policy/version and a content fingerprint. Multiple independent hierarchies are represented as a vector of revisions. Unchanged knowledge reuses one snapshot across runtime contexts; each resolution has a separate context hash and trace.

Creation runs inside an immediate SQLite transaction. Triggers reject updates/deletes of snapshots, archived hierarchy/artifact revisions and resolution records. Artifact registration rejects different contents at an existing revision. Hierarchy edits advance the aggregate revision exactly once. Lesson/Skill/Recovery statements are captured through the existing abstraction artifact interface; abstract knowledge is captured at the matching revision. Opaque imported references without bodies remain explicitly identified guidance. Executable Recovery requires an explicitly registered pinned procedure.

Historical resolution reads archived hierarchies and captured policy, never current hierarchy data. `decision replay` verifies the historical result against the recorded result and compares today's hierarchy using the original context. It writes no new decision, resolution, snapshot or outcome. Unknown historical token validity remains unknown. Current hypothetical results are not evidence of historical safety.

```sh
hardknock knowledge snapshot show <snapshot-id>
hardknock knowledge snapshot diff <before> <after>
hardknock decision replay <decision-id>
```

Snapshot JSON contains reference arrays, not complete artifact bodies. Resolution records contain references and reasons. Runtime decision records retain the bounded operational statements they used. Storage measurements in the Pass 2 report distinguish snapshot rows from the larger decision/resolution audit history.
