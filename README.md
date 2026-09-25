# Hardknock

**Evidence-backed experience for autonomous agents.** Hardknock runs bounded trials, records what happened, and uses tested, scoped lessons to inform later work. The agent still decides what to do; Hardknock preserves the evidence across runs and agents.

<p align="center">
  <img src="hardknock-murph.png" alt="Murph, the Hardknock axolotl, holding a wrench and an experiment checklist" width="200">
</p>

An agent can fail a task, explain the failure convincingly, and still learn the wrong rule. Hardknock treats that explanation as a hypothesis. It compares alternatives from recorded starting conditions, evaluates the results with explicit checks, and retains both supporting and contradictory evidence. Lessons have scope and can be revised or retired.

```text
Task → disposable Reality → execution + checks → immutable Experience
                                                   ↓
                                  hypothesis → controlled trial
                                                   ↓
                                    scoped Lesson → later decision
```

## Try it locally

Hardknock is a pre-alpha Rust CLI. Build it on Linux or macOS with Rust, Git, and a C compiler. This deterministic example needs no model, package manager, or network service after dependencies are available:

```bash
cargo build --locked
HARDKNOCK_BIN="$PWD/target/debug/hardknock"
DEMO_ROOT="$(mktemp -d)"
cp -R fixtures/strategy-choice "$DEMO_ROOT/project"
git -C "$DEMO_ROOT/project" init -b main
git -C "$DEMO_ROOT/project" config user.name 'Hardknock Demo'
git -C "$DEMO_ROOT/project" config user.email 'demo@example.invalid'
git -C "$DEMO_ROOT/project" add .
git -C "$DEMO_ROOT/project" -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -m 'Strategy fixture'

"$HARDKNOCK_BIN" --home "$DEMO_ROOT/data" --repo "$DEMO_ROOT/project" try \
  --agent test-agent \
  --candidate 'direct=direct-upgrade' \
  --candidate 'staged=staged-upgrade' \
  --check './test.sh'
```

The two candidates start from the same committed fixture. The direct upgrade fails its check; the staged upgrade passes. Hardknock reports the comparison and stores two Experiences, but does not apply the winning candidate to the source repository. See the [experiment walkthrough](docs/agent-experiments.md) for the result format, limitations, and a nonfixture example.

For your own clean, committed repository, start with `hardknock --repo /path/to/project run --help` or the [CLI reference](docs/cli.md). Provide a `--check` command when you need a task outcome: a process exit code alone does not establish task success. `HARDKNOCK_HOME` or `--home` selects a dedicated data directory outside the source repository.

## What it covers

Hardknock is one modular Rust crate with SQLite metadata, local artifacts, Git worktree Realities, and an authenticated local Bridge. Its implemented local flows include:

| Area | What it does | Start here |
| --- | --- | --- |
| Experience and experiments | Capture evaluated runs, compare alternatives, retrieve scoped Lessons, and test transfer | [Experience model](docs/experience-model.md) · [Experiments](docs/experiments.md) |
| Resilience and learning | Run controlled chaos and recovery trials; plan bounded curricula and experience budgets | [Chaos](docs/chaos.md) · [Curriculum](docs/curriculum.md) · [Economics](docs/experience-economics.md) |
| Runtime decisions | Use evidence for advice, abstention, prevention, and inspectable decisions | [Runtime control](docs/runtime-control.md) · [Predictive experience](docs/predictive-experience.md) |
| Execution and effects | Declare capabilities, run portable tools, stage supported external effects, and require explicit commit | [Execution boundary](docs/execution-boundary.md) · [Effects](docs/effects.md) |
| Shared knowledge | Integrate agents, federate signed evidence, resolve scoped knowledge, and coordinate bounded teams | [Integrations](docs/integrations.md) · [Federation](docs/federation.md) · [Knowledge resolution](docs/knowledge-resolution.md) · [Team governance](docs/team-governance.md) |

The [documentation index](docs/README.md) groups the full guides, design details, benchmarks, and implementation reports. The [architecture](docs/architecture.md) explains component boundaries; the [roadmap](docs/roadmap.md) describes the longer direction.

## Current status and limits

Hardknock is pre-alpha and built from source. The local feature work through V0.21 is documented; V0.22 distributed sync is in development. The [V0.22 progress record](docs/v0.22-progress.md) lists the verified workflows and remaining release criteria.

Git worktrees are disposable working states, **not security sandboxes**. They share access to host files, credentials, network, and Git state. Use trusted commands and disposable tasks; choose a documented container or tool capability boundary where isolation matters. Supported external Effects require a separate explicit commit, and Hardknock does not intercept arbitrary external calls. Read the [execution boundary](docs/execution-boundary.md) and [effect security guide](docs/effect-security.md) before using those features.

Native Claude Code, Codex, Hermes, and OpenClaw adapters have deterministic fixture coverage, while live cross-agent acceptance is still incomplete. Benchmarks in the docs are designed local fixtures, not estimates of production reliability or general agent improvement.

## Repository map

| Path | Purpose |
| --- | --- |
| `src/` | CLI, runtime, evidence, learning, integrations, and storage modules |
| `tests/` | Integration and security tests |
| `fixtures/` | Deterministic, offline scenarios used by demos and tests |
| `integrations/` | Native agent adapter assets |
| `migrations/` | Append-only SQLite schema migrations |
| `docs/` | Guides, design notes, benchmarks, and milestone reports; start at [docs/README.md](docs/README.md) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build checks and contribution guidance. Hardknock is licensed under [Apache-2.0](LICENSE); see [NOTICE](NOTICE).
