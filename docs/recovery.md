# Recovery experiments

A Recovery is a scoped restoration procedure tied to a failure signature and its source chaos Trial. Its steps are typed: `ShellCommand`, `SetEnvironmentVariable`, and `Replan`. It starts Candidate; one controlled successful test may make it Supported, never automatically Validated.

```bash
hardknock chaos run --fixture stale-credential --profile credential
hardknock recovery list
hardknock recovery show recovery-<uuid>
hardknock recovery test recovery-<uuid>
```

The credential fixture uses the literal strings `VALID_TOKEN` and `STALE_TOKEN`, with no real credential service. The control succeeds. The perturbation produces six observed failed authentication attempts. Its candidate procedure is:

1. Run the local `refresh-token.sh` to write `VALID_TOKEN`.
2. Set the explicit child variable `HK_TOKEN_STATE=VALID_TOKEN` so the stale injected input is actually replaced.
3. Run `read-state.sh` to re-read desired state.
4. Replan/retry the operation, then run the declared evaluator.

`recovery test` first records a failure-only replay. In a second fresh Reality it recreates the perturbation, runs to failure again, matches the expected signature and context, and runs a failing pre-recovery check **before** any recovery steps. Only then does it apply the procedure in that same Reality and evaluate. A clean-state success cannot count as recovery.

Both arms are Experiences. The response has a `recovery_of` relation to the without-arm failure and records all operation, precheck, step, retry, and evaluator processes. `RecoveryAttempt` records reproduction, signature, attempted/succeeded, wall-clock time to execute the procedure, and number of typed steps executed. That duration excludes the final evaluator; the ordinary action records retain its separate duration. Environment-setting steps count as steps even though they are not separate processes.

Successful reproduction + procedure + final check moves Candidate to Supported (0.42 → 0.81). Failed restoration after successful reproduction is Contradicted (0.25). Missing failure reproduction, interruption, and timeout are inconclusive. Existing contradictions are not erased by later support. Evidence and versioned procedures are retained; finalization loads the latest revision inside an immediate SQLite transaction.

`RecoverySuccessRate` counts successful attempts / attempted recoveries, with `null` for no attempts. There is no claim that repetition of this same deterministic fixture establishes independence or universally validates the recovery. Replication policy, Validated promotion, retirement CLI, arbitrary user-authored recovery registration, external rollback, and production credential handling are deferred.

The retry fixture also proposes its known alternative as a candidate recovery; configuration drift proposes re-reading generation before applying the plan. All three are explicit local fixture procedures, not autonomous recovery synthesis.
