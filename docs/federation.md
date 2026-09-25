# Experience federation

Hardknock federation exchanges signed, normalized evidence between local nodes. It does not synchronize mutable Lessons and does not create distributed consensus.

The V0.22 distributed-sync checkpoint extends this package exchange with persistent
envelopes, cursors, sessions, and remote advisory records. Its current boundary and
unfinished integration are documented in [V0.22 progress](v0.22-progress.md).

## Distributed Lesson exchange

With both nodes configured as peers using `peer add --public-key ... --sync-dir ...`,
the origin can publish a locally validated Lesson to a filesystem endpoint:

```bash
hardknock sync publish receiver --lesson lesson-<uuid>
hardknock sync status
```

On the receiver, pull the origin's endpoint and use the returned artifact hash to
enter the existing federation import and controlled reproduction flow:

```bash
hardknock sync pull origin
hardknock sync import <content-hash>
hardknock federate search --kind lesson
hardknock federate test <federated-object-id>
```

The receiver may run `sync relay downstream <content-hash>` for a verified,
non-revoked artifact after trusting the origin. Downstream must trust both the
relay and the origin before the copy is advisory. The origin signature proves
the origin signed the artifact content; it does not prove the Lesson is correct or make remote knowledge local
authority. See the [checkpoint](v0.22-progress.md) for remaining integration work.

The origin can withdraw a published Lesson artifact by hash. The signed revocation
travels through the same pull and relay path:

```bash
hardknock sync revoke receiver <content-hash> --reason corrupt_evidence
hardknock sync pull origin
```

Supported reasons are `contradicted`, `security_issue`, `corrupt_evidence`,
`superseded`, `scope_incorrect`, and `origin_compromised`; other nonempty text
is recorded as a custom reason. Repeating the same command reuses the original
signed revocation. On receipt, unreproduced sync advice is revoked. Imported
federation objects from a revoked Lesson bundle leave search results and cannot
be newly reproduced or promoted; any earlier independent local reproduction is
retained for review.

## Trust model

Every node has an Ed25519 keypair in `identity/node.key` and `identity/node.pub`; the private key is mode `0600`. The node ID is the BLAKE3 digest of the public key. Compact canonical JSON for the redacted `hardknock.bundle.v1` payload is signed with a domain separator. The payload hash and content-addressed bundle ID are verified independently.

Authenticity and correctness are separate. `SignatureValid` means the payload is intact and attributable. `Known`/`Trusted` peer status is a local administrative choice. Neither establishes applicability. Local reproduction and later application provide epistemic evidence for this node.

## Lifecycle

```text
RECEIVED → CONTEXT_MATCHED → REPRODUCTION_RECOMMENDED
                                      ↓
                         local controlled experiment
                           /                    \
                  LOCALLY_SUPPORTED    LOCALLY_CONTRADICTED
                           ↓                    ↓
              later successful use          CONFLICT
                           ↓
                  LOCALLY_VALIDATED
```

The deterministic comparison uses OS, architecture, abstract repository family, required markers, selected environment-family tags, and explicitly captured version facts. Missing data remains visible. It is a screening policy, not proof of compatibility and not an LLM judgment.

## Privacy and safety

The redaction pass runs before hashing and signing. It replaces the selected repository and home paths, authorization values, API/access tokens, common secret assignments, AWS access-key patterns, JWT-like tokens, and secret-named JSON fields. Bundles contain action patterns needed for explicit reproduction, normalized context/evaluations, provenance, and artifact hashes. They exclude raw prompt conversations, arbitrary stdout/stderr, credentials, inherited environment values, and artifact bytes.

Imported Reflexes can warn only: any requested `WARN`, `REPLAN`, or `BLOCK` is represented locally as `ADVISE`. Imported Recoveries are suggestions and Skills are candidates. No import activates or executes either. Reproduction is explicit and runs the standard controlled strategy experiment engine with local evaluator checks.

## Filesystem repository

`FilesystemTransport` writes immutable `.hkexp` files and a small `index.json` containing bundle ID, producer, schema, time, labels, object kinds, abstract families, and markers. It never pushes Git or uses a network. Bundle parsing uses size, object-count, nesting-depth, reference, path, content-ID, payload-hash, and signature gates before evidence is persisted.

## Evidence diversity

Node, agent, repository, and context counts describe diversity. They do not establish independence. Nodes can share a model, prompt, evaluator, test suite, documentation, dependency, or upstream defect. Hardknock therefore does not vote on claims, infer correctness from popularity, or count a re-exported lineage twice.

V0.9 Experience evidence can include execution assurance: provider/security levels, capability manifest hash/revision, image digest when observed, network mode, credential isolation, Effect gating, and frozen state. This metadata improves interpretation but is producer-supplied signed provenance, not remote attestation. A receiving node must not treat `container` as a hardened sandbox or infer that an unobserved runtime test passed.

## Configuration

```toml
[federation]
auto_publish = false
node_name = "platform-developer"
minimum_context_match = 0.70
allow_raw_artifacts = false
```

The size defaults are 50 MiB per bundle, 10,000 objects, 10 MiB per artifact (reserved; raw artifacts disabled), and nesting depth 32. V0.7 supports signed bundles. Unsigned import and key rotation notices remain deferred rather than weakening verification.

## Threats

Signatures address tampering and attribution, not experience poisoning. The safety design assumes peers or signing keys may be compromised and Lessons may be plausible but harmful. Advisory defaults, context visibility, local experiments, contradiction retention, lineage deduplication, no automatic blocking/recovery, conservative parsing, and local peer blocking limit the impact. There is no popularity score or global trust declaration.
