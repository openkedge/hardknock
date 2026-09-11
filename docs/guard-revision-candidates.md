# Guard revision candidates

Hardknock learns empirically. OpenKedge enforces deterministically.

Fresh, active, validated Constraints can propose a Guard candidate. Evidence-backed Excepts nodes can propose an exception. Contradicted knowledge linked to an existing Guard creates a review candidate. Inherited support is invalidated conservatively; independently evidenced children survive a contradicted parent.

Candidates contain exact knowledge/hierarchy revisions, scope, source Guard revision, reason, proposed change and a sealed V0.11 evidence manifest wrapper. The wrapper binds the hierarchy hash, knowledge references, evidence references and scope. Export adds a schema and content hash; verification rejects tampering. Hash integrity establishes consistency, not third-party attestation or scientific validity.

```sh
hardknock guard-candidate generate <hierarchy-id> <node-id> --guard reconciliation --guard-revision 3
hardknock guard-candidate list
hardknock guard-candidate show <candidate-id>
hardknock guard-candidate export <candidate-id> --output candidate.json
hardknock guard-candidate verify candidate.json
```

Export is a local file handoff for separate review. It never contacts OpenKedge, changes a Guard, grants approval or assigns external acceptance. External acceptance/rejection status values exist for interoperability; Hardknock's local candidate insertion rejects self-assigned external acceptance. Existing Guard dependencies identify what requires review when supporting knowledge is contradicted.

Scope expansion, narrowing, retirement and supersession are represented proposal types. Automatic generation currently covers new Constraints, explicit exceptions and contradiction review. No automatic retirement or external enforcement mutation is performed.
