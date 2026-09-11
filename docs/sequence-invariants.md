# Sequence invariants and state handoffs

SequenceInvariant supports EntireComposition, Between two ordered steps, Before a step, After a step, and UntilCommitPoint. Evaluation records the phase, step and true/false/unknown result. Whole-composition requirements are checked before/after steps and at completion; commitment requirements are checked before the point is recorded.

StateClaim contains a typed primitive value, source and freshness record. Equal highest-trust conflicting observations remain unknown. Expired/future observations and mismatched external resource versions are excluded. Agent/user reports cannot become authoritative state. Bridge composition reports are explicitly downgraded to AgentReported.

StateHandoff identifies producer and consumer, facts, artifact references, external versions and provenance. BeforeNextMutation requires producer provenance and a matching external version; a prior observation alone cannot establish current external state. AuthoritativeRefreshRequired asks for a current-step authoritative observation.

Runtime composition context includes the revision, next step, completed steps, handoffs, commit state and observations. Each decision rechecks current dependencies and active invariants, then resolves the V0.18 hierarchy. Publication checks again for changes. Stored historical decisions retain their original context; current replay can recommend revalidation.

Observed invariants can be exported as ordinary scoped operational Constraint revisions in the V0.18 hierarchy. They activate on a derived violation fact and composition identity. Guard candidates require composition assurance and remain proposals for external review; knowledge never becomes enforcement authority.

The local trial engine carries explicitly bound JSON facts rather than a live service snapshot. External freshness/commit receipts require adapter observations. Strings are excluded from predictive observations to avoid transporting credential material; numeric and boolean step observations feed existing trajectories and forecasts.
