// SPDX-License-Identifier: Apache-2.0
//! Deterministic, network-free comparison of specific, naive, and empirical abstraction.

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstractionBenchmarkArm {
    SpecificOnly,
    NaiveSemanticAbstraction,
    HardknockEmpiricalAbstraction,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AbstractionBenchmarkMetrics {
    pub held_out_transfer_successes: usize,
    pub evaluated_held_out_transfers: usize,
    pub negative_transfers: usize,
    pub evaluated_transfers: usize,
    pub false_abstract_constraints: usize,
    pub evaluated_constraint_applications: usize,
    pub repeated_failures: usize,
    pub task_successes: usize,
    pub recovery_successes: usize,
    pub runtime_knowledge_items_injected: usize,
    pub abstraction_count: usize,
    pub specialization_count: usize,
    pub exception_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionBenchmarkResult {
    pub arm: AbstractionBenchmarkArm,
    pub metrics: AbstractionBenchmarkMetrics,
    pub observations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionFamilyResult {
    pub family: String,
    pub outcome: String,
    pub boundary_behavior: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstractionBenchmarkReport {
    pub schema: String,
    pub deterministic: bool,
    pub model_calls: usize,
    pub network_calls: usize,
    pub results: Vec<AbstractionBenchmarkResult>,
    pub families: Vec<AbstractionFamilyResult>,
    pub scientific_hypothesis_supported: bool,
    pub claim: String,
}

#[derive(Clone, Copy)]
struct Scenario {
    held_out: bool,
    rule_should_apply: bool,
    specific_knows_context: bool,
    naive_applies: bool,
    empirical_applies: bool,
    constraint: bool,
    recovery: bool,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        // Authoritative-state held-out object store: transfer should help.
        Scenario {
            held_out: true,
            rule_should_apply: true,
            specific_knows_context: false,
            naive_applies: true,
            empirical_applies: true,
            constraint: false,
            recovery: true,
        },
        // Exact idempotent replay is a mandatory negative control.
        Scenario {
            held_out: true,
            rule_should_apply: false,
            specific_knows_context: false,
            naive_applies: true,
            empirical_applies: false,
            constraint: true,
            recovery: false,
        },
        // Quorum structure transfers with a provider specialization.
        Scenario {
            held_out: true,
            rule_should_apply: true,
            specific_knows_context: false,
            naive_applies: true,
            empirical_applies: true,
            constraint: true,
            recovery: false,
        },
        // A different consistency model defeats the recovery analogy.
        Scenario {
            held_out: true,
            rule_should_apply: false,
            specific_knows_context: false,
            naive_applies: true,
            empirical_applies: false,
            constraint: false,
            recovery: true,
        },
        // Same phrase "retry failed", different rate-limit mechanism.
        Scenario {
            held_out: true,
            rule_should_apply: false,
            specific_knows_context: false,
            naive_applies: true,
            empirical_applies: false,
            constraint: false,
            recovery: false,
        },
        // Existing source-context knowledge remains useful and retained.
        Scenario {
            held_out: false,
            rule_should_apply: true,
            specific_knows_context: true,
            naive_applies: true,
            empirical_applies: true,
            constraint: false,
            recovery: true,
        },
    ]
}

fn evaluate(arm: AbstractionBenchmarkArm) -> AbstractionBenchmarkResult {
    let mut metrics = AbstractionBenchmarkMetrics::default();
    for scenario in scenarios() {
        let applies = match arm {
            AbstractionBenchmarkArm::SpecificOnly => scenario.specific_knows_context,
            AbstractionBenchmarkArm::NaiveSemanticAbstraction => scenario.naive_applies,
            AbstractionBenchmarkArm::HardknockEmpiricalAbstraction => scenario.empirical_applies,
        };
        metrics.evaluated_transfers += 1;
        if scenario.held_out {
            metrics.evaluated_held_out_transfers += 1;
            if applies == scenario.rule_should_apply {
                metrics.held_out_transfer_successes += 1;
            }
        }
        if scenario.rule_should_apply && !applies {
            metrics.repeated_failures += 1;
        }
        if !scenario.rule_should_apply && applies {
            metrics.negative_transfers += 1;
            if scenario.constraint {
                metrics.false_abstract_constraints += 1;
            }
        }
        if scenario.constraint && applies {
            metrics.evaluated_constraint_applications += 1;
        }
        if applies == scenario.rule_should_apply {
            metrics.task_successes += 1;
        }
        if scenario.recovery && scenario.rule_should_apply && applies {
            metrics.recovery_successes += 1;
        }
    }
    match arm {
        AbstractionBenchmarkArm::SpecificOnly => {
            metrics.runtime_knowledge_items_injected = 8;
            metrics.specialization_count = 8;
        }
        AbstractionBenchmarkArm::NaiveSemanticAbstraction => {
            metrics.runtime_knowledge_items_injected = 1;
            metrics.abstraction_count = 1;
        }
        AbstractionBenchmarkArm::HardknockEmpiricalAbstraction => {
            metrics.runtime_knowledge_items_injected = 2;
            metrics.abstraction_count = 1;
            metrics.specialization_count = 1;
            metrics.exception_count = 2;
        }
    }
    AbstractionBenchmarkResult {
        arm,
        metrics,
        observations: match arm {
            AbstractionBenchmarkArm::SpecificOnly => vec![
                "Specific evidence stays precise but cannot guide unseen contexts".into(),
                "Eight narrow artifacts are injected for the matching source family".into(),
            ],
            AbstractionBenchmarkArm::NaiveSemanticAbstraction => vec![
                "Surface similarity transfers broadly without a held-out evidence gate".into(),
                "Idempotent and different-mechanism controls receive harmful guidance".into(),
            ],
            AbstractionBenchmarkArm::HardknockEmpiricalAbstraction => vec![
                "Held-out support transfers the authoritative-state mechanism".into(),
                "Negative controls create exclusions instead of a universal retry rule".into(),
                "One abstraction plus one specialization replaces eight injected members".into(),
            ],
        },
    }
}

pub fn run() -> Result<AbstractionBenchmarkReport> {
    let results = vec![
        evaluate(AbstractionBenchmarkArm::SpecificOnly),
        evaluate(AbstractionBenchmarkArm::NaiveSemanticAbstraction),
        evaluate(AbstractionBenchmarkArm::HardknockEmpiricalAbstraction),
    ];
    let naive = &results[1].metrics;
    let empirical = &results[2].metrics;
    let supported = empirical.held_out_transfer_successes
        > results[0].metrics.held_out_transfer_successes
        && empirical.negative_transfers < naive.negative_transfers
        && empirical.runtime_knowledge_items_injected
            < results[0].metrics.runtime_knowledge_items_injected;
    Ok(AbstractionBenchmarkReport {
        schema: "hardknock.abstraction-benchmark.v1".into(),
        deterministic: true,
        model_calls: 0,
        network_calls: 0,
        results,
        families: vec![
            AbstractionFamilyResult {
                family: "authoritative_state".into(),
                outcome: "held_out_object_store_transfer_supported".into(),
                boundary_behavior: "exact idempotent replay excluded by negative control".into(),
            },
            AbstractionFamilyResult {
                family: "quorum_availability".into(),
                outcome: "transfer_supported_with_specialization".into(),
                boundary_behavior: "provider threshold semantics remain specific".into(),
            },
            AbstractionFamilyResult {
                family: "misleading_surface_similarity".into(),
                outcome: "pattern_rejected_or_split".into(),
                boundary_behavior: "shared wording with different mechanisms is not merged".into(),
            },
        ],
        scientific_hypothesis_supported: supported,
        claim: "In this deterministic fixture only, held-out and negative-control gated abstraction preserves more useful transfer with less negative transfer and fewer runtime knowledge items than the specific-only and naive arms.".into(),
    })
}
