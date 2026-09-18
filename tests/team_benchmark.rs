// SPDX-License-Identifier: Apache-2.0
use hardknock::team::{TeamBenchmarkArm, run_flagship_team_benchmark};

#[test]
fn flagship_common_mode_fixture_compares_all_three_arms() {
    let report = run_flagship_team_benchmark();
    assert_eq!(report.arms.len(), 3);
    let single = report
        .arms
        .iter()
        .find(|arm| arm.arm == TeamBenchmarkArm::SingleAgent)
        .unwrap();
    let naive = report
        .arms
        .iter()
        .find(|arm| arm.arm == TeamBenchmarkArm::NaiveMajority)
        .unwrap();
    let hardknock = report
        .arms
        .iter()
        .find(|arm| arm.arm == TeamBenchmarkArm::RoleSeparated)
        .unwrap();
    assert!(!single.task_success && single.correlated_error_escaped);
    assert!(!naive.task_success && naive.redundant_agent_runs == 2);
    assert!(hardknock.task_success && !hardknock.correlated_error_escaped);
    assert_eq!(hardknock.challenge_yield, 1);
    assert_eq!(hardknock.authority_escapes, 0);
    assert_eq!(hardknock.effect_bypasses, 0);
    assert_eq!(hardknock.contradictions_discovered, 1);
}
