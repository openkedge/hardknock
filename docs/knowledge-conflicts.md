# Conflict learning

Unresolved conflicts remain unresolved until evidence discriminates their scopes. The planner never selects a winner by model confidence. A plan without a concrete paired experiment is deferred with `PreserveConflict`.

```sh
hardknock knowledge conflicts
hardknock knowledge conflict show <conflict-id>
hardknock knowledge conflict plan <conflict-id> --template comparison.json --skill existing-skill
```

The template is an existing `ExperimentRequest`: at least two candidates, concrete evaluator checks, explicit starting state and bounded cost. Strategy comparisons use the existing CompareStrategies engine. `--skill` compiles a persisted Curriculum with an existing Skill and Experiment trials; execute it using the normal curriculum command. No fabricated Skill, process runner or separate budget system is introduced.

Goal types include ResolveKnowledgeConflict, ValidateException, ValidateSpecialization, ResolveScopeOverlap and RevalidateKnowledgeOverride. Causal and evidence-diversity helpers reuse the existing causal intervention compiler and epistemic acquisition planner. Choosing among those helpers is currently explicit; automatic mechanism inference is not implemented.

Plans create existing ExperienceOpportunities with runtime exposure and Constraint risk. The existing experience-debt query can surface frequently exposed unresolved override opportunities through the `UnvalidatedKnowledgeOverride` reason. Rare informational conflicts receive lower risk value.

`Store::narrow_knowledge_conflict` accepts stored completed controlled experiments planned for that conflict, checks a provably narrower scope, creates new artifact/hierarchy revisions and verifies the observed context no longer conflicts. Scope interpretation remains an explicit caller decision: the API does not infer numeric boundaries from arbitrary shell output. Historical snapshots remain unchanged. Resolution of one observed context is not a claim that all possible scope overlaps have disappeared.

The fixture compares a deterministic unsafe-retry model with reconciliation using an evaluator, then narrows a competing exception and re-resolves. This is a local execution/integration test, not empirical evidence about a real provider's token expiry.

The API-v3 end-to-end test additionally executes 3/7/11-minute comparisons and a missing-token negative control. Its explicitly modeled boundary is five minutes. Trusted observations produce ACT only inside that boundary, external approval still wins, and unknown historical context remains REPLAN after new evidence. The Guard export includes the resulting controlled experiment references.
