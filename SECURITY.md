# Security Policy

Hardknock is currently pre-release software. Its documented trust boundaries
are part of the product contract: Git worktrees are cooperative isolation,
container execution shares the host kernel, local Bridge authentication trusts
the operating-system user, and agent evidence never grants external commit or
approval authority.

## Supported versions

| Version | Security status |
| --- | --- |
| Current development branch and latest tagged checkpoint | Best-effort security fixes during pre-release development |
| Older checkpoints | Unsupported; reproduce on the latest checkpoint before reporting |
| Future stable 1.x release | Supported according to `docs/support-policy.md` |

## Reporting a vulnerability

Use the repository's private GitHub security advisory form:

<https://github.com/openkedge/hardknock/security/advisories/new>

Do not include credentials, production data, private prompts, or sensitive
artifacts. Include:

- affected Hardknock version or commit;
- operating system, architecture, and execution provider;
- the smallest safe reproduction;
- the expected and observed authority or isolation boundary;
- whether external state, credentials, signatures, or evidence integrity were
  affected.

If private advisories are unavailable, contact the maintainers through a
private channel before opening a public issue.

## Response targets

These are pre-release targets, not a service-level agreement:

- acknowledge a complete report within three business days;
- provide an initial severity and scope assessment within seven business days;
- coordinate remediation and disclosure timing according to exploitability,
  affected releases, and available mitigations.

## Security boundaries

Reports are especially useful when they demonstrate:

- capability or filesystem escape beyond a declared Reality;
- agent self-approval or unauthorized Effect commit;
- signature, replay, revocation, or trust-policy bypass;
- cross-Reality or cross-node authority confusion;
- unsafe installer overwrite, archive traversal, or update substitution;
- secret persistence in a capture path documented as redacted;
- mutation or deletion of records documented as immutable;
- a remote advisory artifact becoming local authority without reproduction.

Known and explicitly documented limitations remain product gaps rather than
vulnerabilities unless the implementation exceeds its declared authority or
misrepresents the boundary. See the [threat model](docs/threat-model.md) and
[production-readiness plan](docs/production-readiness-plan.md).
