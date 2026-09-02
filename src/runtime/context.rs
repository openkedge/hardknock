// SPDX-License-Identifier: Apache-2.0

use chrono::{Duration, Utc};

use crate::{
    Result,
    assurance::{AssuranceGap, AssuranceGapKind, CertificationStatus, SkillCertification},
    bridge::protocol::NormalizedAction,
    core::{AgentIdentity, HardknockSessionId},
    development::{EvidenceState, ExperienceRef, assess_freshness},
    lesson::{ActionPattern, EvidenceRelationship, LessonStatus},
    resilience::{RecoveryStatus, ReflexStatus, SkillStatus},
    retrieval::{DeterministicRetriever, LessonRetriever, QueryContext, RetrievalOptions},
    store::{AssuranceStore, EpistemicStore, Store},
};

use super::*;

#[derive(Clone, Debug)]
pub struct RuntimeContextRequest {
    pub external_session_id: String,
    pub agent: AgentIdentity,
    pub task: TaskDescriptor,
    pub query_context: QueryContext,
    pub proposed_action: Option<NormalizedAction>,
    pub proposed_effect: Option<crate::effects::EffectRequest>,
    pub risk: Option<RuntimeRiskAssessment>,
    pub capability_context: CapabilityContext,
    pub failure_signature: Option<FailureSignatureRef>,
    pub consecutive_failures: u32,
    pub no_state_change: bool,
    pub config_changed: bool,
    pub candidate_strategies: Vec<StrategyCandidate>,
    pub experiment_capability: ExperimentCapabilitySummary,
    pub known_unknowns: Vec<String>,
    pub externally_supported: bool,
    pub envelope_position: Option<EnvelopePosition>,
}

pub struct RuntimeContextSynthesizer<'a> {
    pub store: &'a Store,
}

impl RuntimeContextSynthesizer<'_> {
    pub fn synthesize(&self, request: RuntimeContextRequest) -> Result<RuntimeDecisionContext> {
        let now = Utc::now();
        let config = crate::bridge::config::Config::load(&self.store.home)?;
        let report = DeterministicRetriever {
            store: self.store,
            options: RetrievalOptions::default(),
        }
        .retrieve(&request.query_context)?;
        let all_lessons = self.store.all_lessons()?;
        let freshness_bases = self.store.lesson_freshness_bases(&all_lessons)?;
        let action_pattern = request
            .proposed_action
            .as_ref()
            .and_then(normalized_action_pattern);
        let scoped_lessons = all_lessons
            .iter()
            .filter(|lesson| {
                lesson
                    .context_match
                    .matches(&request.query_context.experience_context())
            })
            .filter(|lesson| {
                action_pattern.as_ref().is_none_or(|action| {
                    lesson.avoid.as_ref() == Some(action) || lesson.prefer.as_ref() == Some(action)
                })
            })
            .collect::<Vec<_>>();
        let local_contradicted = scoped_lessons.iter().any(|lesson| {
            lesson.status == LessonStatus::Contradicted
                || lesson.evidence.iter().any(|item| {
                    matches!(
                        item,
                        crate::lesson::EvidenceRef::Experience {
                            relationship: EvidenceRelationship::Contradicts,
                            ..
                        } | crate::lesson::EvidenceRef::Trial {
                            relationship: EvidenceRelationship::Contradicts,
                            ..
                        }
                    )
                })
        });
        let evidence_stale = scoped_lessons.iter().any(|lesson| {
            freshness_bases.get(&lesson.id).is_some_and(|basis| {
                matches!(
                    assess_freshness(
                        basis,
                        &request.query_context,
                        Some(&request.agent),
                        now,
                        &config.development,
                    )
                    .state,
                    EvidenceState::Stale
                )
            })
        });
        let known_failure_precursor = action_pattern.as_ref().is_some_and(|action| {
            report.matches.iter().any(|retrieved| {
                retrieved.lesson.avoid.as_ref() == Some(action)
                    && !matches!(retrieved.lesson.status, LessonStatus::Contradicted)
            })
        }) && !evidence_stale
            && !local_contradicted;

        let mut applicable_skills = Vec::new();
        let mut selected_skills = Vec::new();
        for skill in self.store.skills()? {
            let applicable = skill
                .context
                .matches(&request.query_context.experience_context())
                && action_pattern.as_ref().is_none_or(|action| {
                    skill.procedure.iter().any(|procedure| procedure == action)
                });
            if !applicable
                || !matches!(
                    skill.status,
                    SkillStatus::Validated | SkillStatus::Supported
                )
                || !matches!(
                    skill.maturity,
                    crate::curriculum::SkillMaturity::Validated
                        | crate::curriculum::SkillMaturity::Hardened
                )
            {
                continue;
            }
            let revision = self
                .store
                .skill_revisions(&skill.id)?
                .last()
                .map(|revision| revision.revision)
                .unwrap_or(1);
            applicable_skills.push(SkillRef {
                id: skill.id.clone(),
                revision,
                name: skill.name.clone(),
            });
            selected_skills.push(skill);
        }

        let mut matched_reflexes = Vec::new();
        let mut active_reflex_items = Vec::new();
        for reflex in self.store.reflexes()? {
            let trigger = &reflex.trigger;
            let matches = matches!(
                reflex.status,
                ReflexStatus::Active | ReflexStatus::Supported
            ) && trigger
                .context
                .matches(&request.query_context.experience_context())
                && action_pattern.as_ref() == Some(&trigger.proposed_action)
                && trigger
                    .repeated_failures
                    .is_none_or(|required| request.consecutive_failures >= required)
                && (!trigger.no_state_change || request.no_state_change)
                && (!trigger.config_changed || request.config_changed);
            if matches {
                matched_reflexes.push(ReflexRef {
                    id: reflex.id.clone(),
                    version: reflex.version,
                });
                active_reflex_items.push(ExperienceRef {
                    kind: "reflex".into(),
                    id: reflex.id.to_string(),
                    revision: u64::from(reflex.version),
                });
            }
        }

        let mut available_recovery = Vec::new();
        let mut recovery_items = Vec::new();
        for recovery in self.store.recoveries()? {
            if !matches!(
                recovery.status,
                RecoveryStatus::Supported | RecoveryStatus::Validated
            ) || !recovery
                .context
                .matches(&request.query_context.experience_context())
                || request.failure_signature.as_ref().is_some_and(|signature| {
                    signature.signature != recovery.failure_signature.signature
                })
            {
                continue;
            }
            let fresh = now.signed_duration_since(recovery.updated_at)
                <= Duration::days(i64::from(config.development.stale_after_days));
            available_recovery.push(RecoveryRef {
                id: recovery.id.clone(),
                version: recovery.version,
                failure_signature: recovery.failure_signature.signature,
                confidence: recovery.confidence,
                fresh,
                scope_matches: true,
            });
            recovery_items.push(ExperienceRef {
                kind: "recovery".into(),
                id: recovery.id.to_string(),
                revision: u64::from(recovery.version),
            });
        }

        let (assurance, envelope) = self.assurance_and_envelope(
            &selected_skills,
            &applicable_skills,
            request.envelope_position,
            request.risk.as_ref(),
        )?;
        let known_gap_matches = !request.known_unknowns.is_empty();
        let local_supported = !report.matches.is_empty() || !applicable_skills.is_empty();
        let relevant_experience = crate::development::ActiveExperienceSet {
            lessons: report.matches,
            reflexes: active_reflex_items,
            recoveries: recovery_items,
        };
        let mut uncertainty = RuntimeUncertainty {
            level: if local_contradicted || evidence_stale || known_gap_matches || !local_supported
            {
                UncertaintyLevel::High
            } else if request.candidate_strategies.len() > 1 {
                UncertaintyLevel::Medium
            } else {
                UncertaintyLevel::Low
            },
            reasons: Vec::new(),
            candidate_strategies: request.candidate_strategies,
        };
        if local_contradicted {
            uncertainty
                .reasons
                .push(UncertaintyReason::ContradictoryEvidence);
        }
        if evidence_stale {
            uncertainty
                .reasons
                .push(UncertaintyReason::FailedPrediction);
        }
        if !local_supported {
            uncertainty
                .reasons
                .push(UncertaintyReason::MissingExperience);
        }
        if uncertainty.candidate_strategies.len() > 1 {
            uncertainty
                .reasons
                .push(UncertaintyReason::MultipleStrategies);
        }
        uncertainty.reasons.extend(
            request
                .known_unknowns
                .iter()
                .cloned()
                .map(UncertaintyReason::KnownGap),
        );

        let mut context = RuntimeDecisionContext {
            session_id: HardknockSessionId::from_external(&request.external_session_id),
            agent: request.agent,
            task: request.task,
            query_context: request.query_context,
            proposed_action: request.proposed_action,
            proposed_effect: request.proposed_effect,
            relevant_experience,
            assurance,
            operating_envelope: envelope,
            capability_context: request.capability_context,
            risk: request.risk.unwrap_or_default(),
            uncertainty,
            available_recovery,
            available_experiments: request.experiment_capability,
            knowledge_signals: KnowledgeSignals {
                local_supported,
                local_contradicted,
                evidence_stale,
                context_in_scope: true,
                validated_skill: !applicable_skills.is_empty(),
                applicable_lesson: !scoped_lessons.is_empty(),
                known_failure_precursor,
                remote_advisory_only: request.externally_supported && !local_supported,
                known_gap_matches,
            },
            applicable_skills,
            matched_reflexes,
            advisory_reflexes: Vec::new(),
            failure_signature: request.failure_signature,
            known_unknowns: request.known_unknowns,
            externally_supported: request.externally_supported,
            tool_candidates: Vec::new(),
            epistemic: None,
            diversity_requirements: Vec::new(),
        };
        if let Some(claim) = self
            .store
            .claims()?
            .into_iter()
            .filter(|claim| {
                claim
                    .scope
                    .matches(&context.query_context.experience_context())
            })
            .find(|claim| {
                claim.canonical_statement()
                    == context
                        .task
                        .description
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase()
            })
        {
            let report = self.store.epistemic_report(&claim.id)?;
            context.epistemic = Some(RuntimeEpistemicSummary {
                claim: claim.id.clone(),
                status: report.fused.status,
                diversity: report.diversity.diversity_class,
                supporting_paths: report.fused.support_paths.len(),
                controlled_empirical_path: report.paths.iter().any(|path| {
                    path.outcome == crate::epistemic::EvidenceOutcome::Supports
                        && matches!(
                            path.source,
                            crate::epistemic::EvidenceSource::Experiment { .. }
                        )
                }),
                common_dependencies: report.diversity.dependency_overlaps,
                caveats: report.fused.caveats,
            });
            if context.risk.severity >= crate::curriculum::Severity::High
                && let Some(action_pattern) = action_pattern
            {
                context
                    .diversity_requirements
                    .push(RuntimeDiversityRequirement {
                        action_pattern,
                        minimum_diversity: crate::epistemic::DiversityClass::Moderate,
                    });
            }
        }
        if context.proposed_effect.is_some() && context.risk.effect_risk.is_none() {
            context.risk = DeterministicRuntimeRiskPolicy.assess(&context);
        }
        Ok(context)
    }

    fn assurance_and_envelope(
        &self,
        skills: &[crate::resilience::Skill],
        refs: &[SkillRef],
        position: Option<EnvelopePosition>,
        risk: Option<&RuntimeRiskAssessment>,
    ) -> Result<(AssuranceContext, Option<OperatingEnvelopeRef>)> {
        let envelope = skills.iter().find_map(|skill| {
            let id = skill.operating_envelope.as_ref()?;
            let envelope = self.store.envelope(id).ok()?;
            Some(OperatingEnvelopeRef {
                id: envelope.id,
                version: envelope.version,
                position: position.unwrap_or(EnvelopePosition::Unknown),
            })
        });
        let mut selected: Option<(SkillCertification, AssuranceRuntimeStatus, Vec<String>)> = None;
        for skill in refs {
            for certification in self.store.skill_certifications(&skill.id)? {
                let mut reasons = Vec::new();
                let mut status = if certification.skill.revision == skill.revision
                    && certification.status == CertificationStatus::Certified
                {
                    AssuranceRuntimeStatus::Current
                } else {
                    reasons.push("Certification does not match the current Skill revision".into());
                    AssuranceRuntimeStatus::Invalidated
                };
                if certification
                    .expires_at
                    .is_some_and(|expires| Utc::now() >= expires)
                {
                    reasons.push("Certification has expired".into());
                    status = AssuranceRuntimeStatus::Expired;
                }
                if self
                    .store
                    .certification_revocation(&certification.id)?
                    .is_some()
                {
                    reasons.push("Certification was explicitly revoked".into());
                    status = AssuranceRuntimeStatus::Invalidated;
                }
                if risk
                    .and_then(|risk| risk.assurance_requirement.as_ref())
                    .is_some_and(|required| required != &certification.profile)
                {
                    reasons.push("Certification profile is outside the action requirement".into());
                    status = AssuranceRuntimeStatus::OutOfScope;
                }
                let replace = selected.as_ref().is_none_or(|(_, current, _)| {
                    assurance_rank(status) > assurance_rank(*current)
                        || (assurance_rank(status) == assurance_rank(*current)
                            && certification.issued_at
                                > selected.as_ref().expect("selected exists").0.issued_at)
                });
                if replace {
                    selected = Some((certification, status, reasons));
                }
            }
        }
        let assurance = if let Some((certification, status, reasons)) = selected {
            AssuranceContext {
                summary: AssuranceSummary {
                    status,
                    certification: Some(certification.id),
                    profile: Some(certification.profile),
                    reasons,
                },
                applicability: AssuranceApplicability {
                    applicable: status == AssuranceRuntimeStatus::Current,
                    reasons: if status == AssuranceRuntimeStatus::Current {
                        vec!["Skill revision, profile, scope, and validity are current".into()]
                    } else {
                        vec!["Certification is not current and applicable".into()]
                    },
                },
                requirements: Vec::new(),
                gaps: if status == AssuranceRuntimeStatus::Current {
                    Vec::new()
                } else {
                    vec![AssuranceGap {
                        kind: AssuranceGapKind::StaleEvidence,
                        description: "Applicable current certification is unavailable".into(),
                        severity: risk.map(|risk| risk.severity),
                    }]
                },
            }
        } else {
            AssuranceContext {
                gaps: vec![AssuranceGap {
                    kind: AssuranceGapKind::InsufficientEvidence,
                    description: "No applicable local certification was found".into(),
                    severity: risk.map(|risk| risk.severity),
                }],
                ..Default::default()
            }
        };
        Ok((assurance, envelope))
    }
}

fn normalized_action_pattern(action: &NormalizedAction) -> Option<ActionPattern> {
    match action {
        NormalizedAction::Shell { command, .. } => Some(ActionPattern::shell(command)),
        NormalizedAction::FileWrite { path } | NormalizedAction::FileDelete { path } => {
            Some(ActionPattern::FileOperation {
                pattern: path.clone(),
            })
        }
        NormalizedAction::FileRead { .. } => None,
        action => Some(ActionPattern::Custom {
            kind: "normalized_action".into(),
            value: serde_json::to_string(action).ok()?,
        }),
    }
}

fn assurance_rank(status: AssuranceRuntimeStatus) -> u8 {
    match status {
        AssuranceRuntimeStatus::Current => 6,
        AssuranceRuntimeStatus::ReviewRecommended => 5,
        AssuranceRuntimeStatus::Expired => 4,
        AssuranceRuntimeStatus::OutOfScope => 3,
        AssuranceRuntimeStatus::Inconclusive => 2,
        AssuranceRuntimeStatus::Invalidated => 1,
        AssuranceRuntimeStatus::Missing => 0,
    }
}
