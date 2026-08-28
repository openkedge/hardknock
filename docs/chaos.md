# Local chaos campaigns

V0.2 generates controlled adversity around a healthy task, shell command, or manually registered Skill. These are **local deterministic perturbations**, not infrastructure fault injection. No model, network service, real token, or privileged setup is required by the bundled fixtures. Git worktrees remain a shared-host boundary, not security sandboxes.

## Run the deterministic demo

After `cargo build --locked`, put `target/debug` on PATH, or replace `hardknock` below with `./target/debug/hardknock`:

```bash
hardknock chaos run --fixture retry-resilience \
  --perturb-sweep delay=0,100,500,1000,2000
hardknock chaos list
hardknock chaos show chaos-<uuid>
hardknock chaos report chaos-<uuid>
hardknock envelope list
hardknock envelope show envelope-<uuid>
hardknock reflex test reflex-<uuid>
hardknock recovery test recovery-<uuid>
```

Copy IDs from the actual output; `<uuid>` is a placeholder. The sweep observes control PASS, 0/100/500ms PASS, 1000ms DEGRADED, and 2000ms FAIL (`retry_exhaustion`). The failure creates a Candidate Lesson, Candidate Reflex, and Candidate Recovery. Nothing is activated automatically.

`--fixture` materializes a versioned bundled source repository under the dedicated Hardknock home, outside the user's checkout. Its source remains available for later tests; only its disposable trial worktrees are removed. This mode does not use `--repo`. Reusing a bundle verifies its known file contents and clean state. To use an initialized copy of `fixtures/retry-resilience` instead:

```bash
hardknock --repo /path/to/initialized-fixture chaos run \
  --agent test-agent --check './test.sh' \
  --perturb delay:100ms --perturb delay:500ms --perturb delay:2000ms \
  'deploy service'
```

Use trusted local scripts and an external data home, as for `run`. General Command targets use `--command 'script' --check 'check'`; the positional task is metadata, not shell substitution.

## Conditions and profiles

| Type | Syntax | Effect |
| --- | --- | --- |
| EnvironmentVariable | `env:KEY=VALUE` | Explicit child environment override, including evaluation; never a host environment mutation |
| FileMutation | `file:relative-path=content` | Replace/create a bounded regular file inside the Reality; preserve the original for cleanup |
| CommandFailure | `command-failure:once`, `:3`, `:always` | Fail the first N fixture operation attempts; `always` means all six possible attempts |
| CommandDelay | `delay:100ms` | Fixture logical delay; for a general Command, an actual local sleep before its top-level invocation |

Repeated `--perturb` arguments are separate trials. The Rust plan also supports compound conditions (up to 16 inputs), with order recorded. A general Command has one invocation, so any positive CommandFailure count replaces that invocation with the configured nonzero exit; this does not intercept arbitrary child commands. File paths reject traversal, `.git`, symlinks, and hard-linked targets. Existing files and content are limited to 64 KiB. Environment values are limited to 4 KiB; runner/loader/Git routing keys and internal fixture controls are reserved. **Explicit values, commands, and artifacts are persisted: never supply real secrets.**

| Built-in profile | Trials |
| --- | --- |
| `latency` | delay 0, 100, 500, 1000, 2000ms |
| `command-failure` | 1, 3, 6 failed attempts |
| `config-drift` | change `generation` to 2 after the committed plan for generation 1 |
| `credential` | set `HK_TOKEN_STATE=STALE_TOKEN` |

Profiles are data expanded into explicit Perturbations before execution. General user TOML profiles, arbitrary interception, network shaping, and infrastructure faults are deferred.

## Control, budgets, and evidence

Each campaign persists its plan first and runs an unperturbed control in a fresh Reality. Both execution and checks must succeed. An unhealthy or inconclusive control aborts the campaign before any perturbed execution or envelope is produced.

`--trials N` bounds perturbed trials (default 10, maximum 100). The mandatory control is one additional run. At most 100 planned condition sets are accepted. `--timeout-secs` bounds the fixture action sequence or Command, and each evaluator check separately (default 30). `--max-duration` is a campaign **dispatch deadline** in seconds (default 300): no further trial starts after it, but the current bounded run/evaluation may finish. It is not a hard wall-clock deadline across Git operations or checks.

Each executed trial commits an immutable Experience and its `chaos_trials` row atomically. A variant has a `chaos_variant_of` relation to its control. The Experience includes condition IDs, campaign/trial IDs, temporal observations, outcome, signatures, metrics, any reflex matches, and recovery observations. Its action list contains every fixture operation/replan/recovery process plus evaluator checks; `ExecutionRecord.action` identifies the last agent process, not an invented aggregate process.

The plan records source commit/tree, commands/checks, condition order, budgets, agent, fixture/runtime/Hardknock versions, environment facts/fingerprint, and active Reflex snapshots. This is sufficient to reconstruct this local protocol, but is not a hermetic host snapshot. Full `chaos replay` is deferred; `reflex test` and `recovery test` implement the required controlled replays now.

Normal success, failure, timeout, and cancellation remove trial worktrees after capture. Perturbation handles unwind in reverse on errors and restore files before removal/preservation. Capture/storage errors preserve the Reality and report it. SIGKILL/power loss can leave orphan worktrees/running plans: inspect and use `reality cleanup` after stopping abandoned processes. No in-process cleanup guarantee survives host termination or hostile commands modifying shared host state.

## Outcomes and metrics

Checks and process status distinguish PASS, FAIL, and INCONCLUSIVE. A successful perturbed run is DEGRADED if retries increased, or its duration metric crossed the declared threshold. The fixture uses logical time: `delay × operation attempts`, degraded at 1000ms or more. General Commands use measured time, degraded at `2 × control duration + 100ms` or more; scheduling can affect that result. Logical fixture delays do not sleep or measure a real service.

`chaos report` provides pass/degraded/fail/inconclusive counts, task success rate (pass + degraded / executed variants), retry/failed-attempt counts, failure detection time with its clock basis, generated candidate counts, paired false-positive rate, recovery success rate, and envelope tested-point count. Empty denominators are `null`. Repeated-mistake rate is `null` for chaos because the V0.1 Lesson retrieval audit is not run inside this protocol; its existing `run` measurements remain available. Tested-point counts are not a percentage of a continuous condition space. Learning Value remains a design-only possible ranking heuristic, not a measured causal quantity.

## JSON and exit codes

`--json` preserves one stdout result: `{"event":"resilience","result":{"kind":"campaign",...}}`. Resource results use `kind` values such as `envelope`, `reflex`, `recovery`, `test`, and `report`. Diagnostics and campaign progress are NDJSON on **stderr**: `chaos_campaign_started`, `chaos_trial_started`, `chaos_trial_completed`, `operating_envelope_updated`, `reflex_created`, and `recovery_created`. Child logs remain artifact files. This deliberately does not introduce streaming stdout.

A completed campaign exits 0 even if it found failures. An unhealthy control or exhausted dispatch/trial budget exits 3; interruption exits 5; runtime failure exits 2. Classified paired tests (support, contradiction, or false positive) exit 0; inspect their status, not only their exit code. Inconclusive tests exit 3. General usage errors retain the existing error contract.

See [operating envelopes](operating-envelopes.md), [reflexes](reflexes.md), [recovery](recovery.md), and the [V0.2 implementation report](implementation-v02.md).
