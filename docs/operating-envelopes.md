# Operating envelopes

An Operating Envelope records empirically observed behavior under tested conditions. It does not certify safety or locate an exact mathematical boundary.

```text
Envelope → Campaign → Trial → Experience → evaluation / actions / artifacts
```

Each V0.2 campaign with perturbed observations creates one immutable version-1 envelope. `tested_conditions` retain full parameter sets, Trial/Experience IDs, and classifications. `safe_regions`, `degraded_regions`, and `failure_regions` contain **TestedPoint** references, not intervals. Inconclusive tested points join `unknown_regions`; **AllUntestedConditions** is always present. The unperturbed control remains in the linked campaign and is not counted again as a swept point.

For the retry fixture:

| Explicit delay | Observed outcome |
| --- | --- |
| 0ms | PASS |
| 100ms | PASS |
| 500ms | PASS |
| 1000ms | DEGRADED |
| 2000ms | FAIL |

The observation does **not** say that every delay ≤500ms passes or that every delay ≥2000ms fails. 1–99, 101–499, 501–999, 1001–1999, and >2000ms remain untested, as do different files, retry limits, tools, platforms, or compound conditions. Repeated identical points are separate observations, not independent coverage or proof of a range.

```bash
hardknock envelope list
hardknock envelope show envelope-<uuid>
hardknock chaos report chaos-<uuid>
```

The target can be a Task, Command, or Skill. A Skill is manually registered from an unperturbed, explicitly replayable successful Experience:

```bash
hardknock skill register retry-operation --experience exp-<control-uuid>
hardknock skill show retry-operation
hardknock chaos run --skill retry-operation --profile latency
```

Registration stores the procedure, scope, source evidence, and `Supported` status; it never invents a `Validated` Skill. V0.2 executes a single shell procedure. Skill campaigns recreate the source Experience snapshot rather than using `--repo`. The Skill record is immutable; its optional envelope field is initially empty and new envelopes link to the Skill through their target/campaign. Automatic synthesis, procedure revisions, aggregation across campaigns, adaptive boundary search, and automatic context narrowing are deferred. The schema reserves envelope revisions, but V0.2 emits new envelopes per campaign rather than editing old conclusions.
