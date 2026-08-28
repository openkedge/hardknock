# Curriculum and skill hardening

V0.5 is explicitly invoked experience planning. The deterministic planner identifies missing evidence; it does not generate arbitrary faults or run in the background. Git worktrees isolate repository copies only. Processes, network, credentials, Git metadata, and the host filesystem are shared. Run only trusted local procedures.

## Reproducible local demo

Build with `cargo build --locked`. The following uses a dedicated temporary data directory and a bundled fixture; it does not modify your checkout:

```bash
export HARDKNOCK_HOME="$(mktemp -d /tmp/hardknock-curriculum.XXXXXX)"
HK="$PWD/target/debug/hardknock"
"$HK" --json chaos run --fixture skill-hardening --perturb delay:0 > /tmp/hardknock-seed.json
SOURCE=$(python3 -c 'import json; print(json.load(open("/tmp/hardknock-seed.json"))["result"]["campaign"]["control"]["experience_id"])')
"$HK" skill register deploy-rolling-update --experience "$SOURCE"
"$HK" skill harden deploy-rolling-update --profile resilience-basic --budget 5
"$HK" skill package deploy-rolling-update
```

The registered name is illustrative: the procedure is a local task-processing fixture, not a real deployment. The first curriculum tests five unknown conditions. It retains separate control/condition Experiences and proposes response artifacts. It should remain **Validated**, because candidate recoveries have not yet been tested. Run `skill harden` again to validate proposed recoveries and challenge the reflex. A later plan can cover the remaining delay condition. Hardened may coexist with named UNKNOWN conditions: the default policy requires three tested dimensions, not complete catalog coverage.

For an exact four-condition fixture demonstration, add to the dedicated home's `config.toml`:

```toml
[curriculum.profiles.hardening]
conditions = ["delay:500", "env:missing", "config:drift", "dependency:unavailable"]
```

```bash
hardknock skill harden deploy-rolling-update --profile hardening --budget 4
hardknock skill harden deploy-rolling-update --profile hardening --budget 3
hardknock skill package deploy-rolling-update --profile hardening
```

With a fresh Skill the first call records delay PASS, empty required credential FAIL, config drift FAIL, and dependency fallback DEGRADED. It creates 8 Experiences, 2 Candidate Lessons, 1 Candidate Reflex, 2 Candidate Recoveries and 4 sparse envelopes. Profile Coverage goes from 1/5 (20%) to 5/5 (100%); maturity becomes Validated. The next three trials test both recoveries and the reflex negative control. Only then does the default policy yield Hardened. These observations do not promote Lessons or activate Reflexes automatically.

## Commands

| Command | Behavior |
| --- | --- |
| `curriculum plan --skill NAME --profile PROFILE --budget N` | Persist an inspectable plan; no Reality is created |
| `curriculum plan --task-family NAME --budget N` | Select matching registered Skills with one aggregate budget |
| `curriculum run ID` | Execute the selected bounded plan |
| `curriculum list`, `show ID`, `why ID`, `report ID` | Inspect targets, rationale, goals, costs, trials, evidence, coverage, maturity, usage and remaining gaps |
| `curriculum cancel ID` | Cancel an unstarted plan immediately; request cancellation of running work and inspect terminal cleanup status |
| `skill harden NAME` | Plan and run; defaults to `resilience-basic`, five trial slots |
| `skill package NAME --profile PROFILE` | Assemble a serializable local package with versioned evidence references |
| `skill show NAME` | Detailed package view using `resilience-basic` |
| `skill list` | Registered immutable Skills, enriched by latest saved package metadata |
| `task-family register NAME --experience ID` | Create a deterministic context selector from one or more matching examples |
| `task-family list`, `show NAME` | Inspect the explicit selector and example IDs |

Use `--replicate` on `curriculum plan` or `skill harden` to deliberately repeat known conditions. This bypasses novelty suppression, not budgets or safety. Use `--json` for machine-readable results. All IDs are typed canonical UUIDs with prefixes. Exit codes: planned/completed 0, incomplete 3, cancelled 5, invalid input/policy errors 2. A completed curriculum means its selected trials finished; it does not mean all possible gaps are closed or every tested procedure succeeded.

## Catalog

Built-ins: `resilience-basic`, `credential-lifecycle`, `latency-basic`, `retry-behavior`. Custom profiles contain condition names, never executable fault scripts. A healthy `control` condition is always included in the denominator.

| Condition | Exact local semantics |
| --- | --- |
| `control` | Unperturbed, evaluator-confirmed base behavior |
| `delay:N` | Bounded top-level real delay for ordinary scripts; logical time for fixture runtime |
| `command-failure:N` | Bounded injected top-level failure; fixture retries are explicit |
| `env:missing` | Fixture's required credential value is empty; this does not remove arbitrary host variables |
| `credential:stale` | Fixture-only invalid synthetic credential value |
| `config:drift` | Fixture `generation` becomes 2 |
| `dependency:unavailable` | Fixture dependency file becomes `down`; one failed attempt then local fallback |
| `input:stale` | Fixture `input-generation` becomes 0 |

Unknown names such as `credential:revoked` or `send-email:real` remain visible and rejected as unsupported. No real credentials are expired, no cloud resource is modified, and no network partition is created. Fixture-specific faults are not scheduled for arbitrary scripts. No interpolation between tested delay points is claimed.

## Budget and policy

```toml
[curriculum]
max_rounds = 1                     # only 1 or 2 supported
max_trials = 8
max_realities = 16
max_agent_runs = 16
max_duration_seconds = 300
max_parallel_trials = 1            # V0.5 serial curriculum dispatch
min_hardening_dimensions = 3
require_high_severity_recovery = true
require_reflex_negative_controls = true
stale_after_days = 30
agent_requests = false
max_agent_session_trials = 2
```

Curriculum caps are separate configuration from V0.4 strategy-request caps, using the same `ExperienceBudget` type. `--budget` means curriculum trial slots. A chaos condition costs two Realities and conservatively two agent runs, including its fresh control. A recovery/reflex test reserves both arms. A shell revalidation costs its candidate count and no native-agent runs. Budget reduction defers whole trials, not one arm of a comparison. The V0.4 orchestrator still enforces its own caps for revalidation requests. Command caps are rejected for curricula because internal fixture/response steps are not one top-level command.

`reserved` records charged slots before dispatch; `usage` records Realities with persisted Experiences and their actions. Setup/capture failure can consume a reservation without a recorded Experience. Reservations are not refunded into new trials. An outer monotonic timer cancels the current engine, awaits process-group teardown and worktree cleanup, then stops. Synchronous Git/file/SQLite work and cleanup can exceed the deadline: this is not an OS hard wall-time limit. Shared provider leases coordinate with V0.4 experiments; older manual/chaos invocations do not participate in that capacity pool.

At `max_rounds=2`, the first round reserves one trial slot. The only adaptive expansion allowed is testing a newly proposed Recovery after an observed failure, within remaining slots/Realities/agent runs/time. There is no recursive agent-generated exploration.

## Evidence discipline

Every trial records its reason, concrete gap, priority rationale, exact semantic fingerprint, required isolation, estimated cost, round and intent. Exact novelty signatures cover Skill/procedure, repository commit/tree/path, perturbation parameters (excluding random IDs), agent identity, evaluator, shell/environment fingerprint and runtime version. Recent conclusive results suppress novel exploration; planned/running identical trials are also deferred. Replication and revalidation have separate intent.

Priority is inspectable: a dimension severity weight × 100, plus capped observed execution frequency, plus unknown-value count. Recovery and contradiction gaps rank highest, followed by credential/config gaps, freshness/reflex checks, other conditions and latency. It is a heuristic, not a probability or an optimal information-gain score.

Coverage counts unique configured conditions with recent conclusive observations in the current context. Failure and degradation are observations; interrupted/inconclusive runs are not coverage. Repeated observations do not enlarge the denominator. Old evidence remains stored and available through linked engines and package snapshots.

Maturity: Observed → Supported (one successful base observation) → Validated (at least two current base observations) → Hardened (configured dimension minimum, no unresolved Critical/base failure, tested responses for configured High-severity failure classes, and reflex negative-control checks). A known false-positive reflex must be disabled; it cannot stay active and qualify as hardened. Old-context recovery support does not satisfy new-context hardening. No automatic Skill procedure rewrite, Lesson scope rewrite, Reflex activation, or production mutation occurs.

Age beyond the configured threshold, changed repository commit, or changed shell/environment fingerprint recommends revalidation; evidence is not deleted. The current implementation does not infer dependency-major-version semantics, architecture changes, or a new live model's hidden state. Agent identity is in trial fingerprints; general agent/model migration remains explicit work.

Contradicted Lessons produce comparisons in each recorded support/contradiction context (including legacy Trial evidence). Results create review records, preserving the original Lesson scope and confidence. Candidate suggestion-provider output is design-only: named suggestions are rejected or require approval, and cannot directly become executable perturbations.

## Experience Package

Skill: what works. Lesson: a scoped claim about behavior that tends not to work and a possible alternative. Envelope: where the Skill has been observed to work, degrade or fail. Reflex: how to recognize an approaching failure. Recovery: what to do after reproducing a failure.

A package contains Skill, all linked envelope IDs, Lesson/Reflex/Recovery IDs, profile coverage, maturity, evidence summary, and provenance entries with item versions and immutable evidence references. Those references retain original agent/environment/scope/confidence data in the local store. Packages are serializable local indexes, not standalone portable exports; remote import/export and trust semantics are not implemented. Original Skill rows stay immutable. Derived coverage and maturity come from append-only package snapshots.

## Bridge

No MCP server exists in this repository. The authenticated Bridge is the callable boundary; a future MCP facade can map `hardknock_request_curriculum`, `hardknock_plan_curriculum`, `hardknock_run_curriculum`, and `hardknock_get_skill_package` to these same APIs.

With `[curriculum] agent_requests=true`, an active agent can send:

```json
{"event":"curriculum_requested","data":{"hardknock_session_id":"<session>","request_id":"curriculum-<UUID>","target":{"skill":"deploy-rolling-update"},"profile":"resilience-basic","budget":{"max_trials":2}}}
```

Then `curriculum_started` with `hardknock_session_id` and `curriculum_id`. Planning never starts execution. Poll `curriculum_progress` with optional `after`; `curriculum_cancelled` requests cancellation. `skill_package_requested` returns bounded package details. Planning IDs are idempotent within one session and conflict on different targets/budgets. Session spending includes prior planned/cancelled work. Agent curricula are restricted to verified bundled hardening procedures/evaluators in the requesting session's repository; arbitrary native task replay remains unsupported. Session end and Bridge shutdown cancel queued/running curricula, even if strategy experiments are configured to continue.

The shared experiment worker queues at most 16 waiting jobs. Poll payloads cap progress to 16 events and summary lists; local CLI inspection preserves complete evidence. The request/start split and bounded queue keep experiments out of pre-tool callbacks. Agent recommendations are returned with explicit plans; no automatic session-start curriculum launch or background scheduler is installed.

## Held-out benchmark

```bash
cargo test --test curriculum held_out_resilience_benchmark -- --nocapture
```

Training uses an empty credential and generation 2 in `skill-hardening`. Held-out cases use a different synthetic credential and generation 9 in `skill-hardening-transfer`, whose healthy generation is 7. The executable fixture contract is shared; conditions and repository state differ. The fixture controller matches actual observed failure signatures to tested recovery IDs. It does not receive an answer oracle or choose a recovery from the perturbation label.

| Metric | No experience | Lesson advice only | Full package |
| --- | ---: | ---: | ---: |
| Success | 0/2 | 1/2 | 2/2 |
| Resilience gain vs no experience | — | +0.5 | +1.0 |
| Repeated failure rate | 2/2 | 1/2 | 0/2 |
| Recovery success rate | undefined: no attempts | undefined: no typed recovery attempts | 2/2 |
| Time to recovery | undefined | undefined | measured per successful typed attempt |

The Lesson-only fixture controller explicitly tries Candidate advice inside the test. Production retrieval still excludes unvalidated Candidates. The full-package group uses the actual stored supported Recovery steps with failure reproduction/precheck and final evaluation. Recovery latency is measured wall time, not a fabricated speedup; runs with no recovery are null rather than zero. There are only two discrete local cases, with no statistical, continuous-latency, live-agent, or production claim.

## Failure handling and limitations

Terminal plans are immutable and repeated `run` returns the stored result. Concurrent executors cannot run the same plan. Hard process death is not automatically resumable: a Running plan can retain partial engine references, but requires inspection and a new plan. Ordinary cancellation is tested; escaped process sessions, disk failure, and adversarial commands are not contained by Git worktrees. Full process/VM snapshots, Docker isolation, external effects, native tool-call cost, remote packages and continuous autonomous scheduling remain deferred.
