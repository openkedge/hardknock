# Deterministic idempotency hierarchy

These are synthetic test observations and directives, not provider policy.
`artifacts.json` describes each referenced directive. All IDs and times are fixed.

| Hierarchy | Context | Expected |
| --- | --- | --- |
| hierarchy.json | valid-token.json | Exact replay exception; linked reconciliation directives suppressed |
| hierarchy.json | expired-token.json | Reconciliation effective; exact replay inapplicable |
| hierarchy.json | unknown-token.json | Reconciliation effective; replay applicability unknown/partial |
| hierarchy.json | wrong-provider.json | Provider X subtree inapplicable |
| stale-exception.json | valid-token.json | Reconciliation effective; exception advisory |
| conflicting-siblings.json | valid-token.json | CompetingExceptions; no exception suppresses defaults |
| supersession.json | api-v3.json | V3 applied; V2 compatibility rule superseded |
| supersession.json | valid-token.json | V2 remains applicable |

Run `hardknock knowledge explain --hierarchy <hierarchy-file> --context <context-file>`.
Add `--json` for the versioned structured result and trace.
