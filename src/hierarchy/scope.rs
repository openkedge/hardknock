// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod context_keys {
    pub const TASK_FAMILY: &str = "task_family";
    pub const ENVIRONMENT: &str = "environment";
    pub const PROVIDER: &str = "provider";
    pub const RESOURCE_TYPE: &str = "resource_type";
    pub const API_VERSION: &str = "api_version";
    pub const DEPENDENCY_VERSION: &str = "dependency_version";
    pub const TOOL_VERSION: &str = "tool_version";
    pub const ACTION_TYPE: &str = "action_type";
    pub const EFFECT_TYPE: &str = "effect_type";
    pub const IDEMPOTENCY: &str = "idempotency";
    pub const REVERSIBILITY: &str = "reversibility";
    pub const CONSISTENCY_MODEL: &str = "consistency_model";
    pub const CREDENTIAL_STATE: &str = "credential_state";
    pub const RUNTIME: &str = "runtime";
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ScopeValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Version(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum ScopePredicate {
    Equals {
        key: String,
        value: ScopeValue,
    },
    NotEquals {
        key: String,
        value: ScopeValue,
    },
    In {
        key: String,
        values: Vec<ScopeValue>,
    },
    Exists {
        key: String,
    },
    Bool {
        key: String,
        expected: bool,
    },
    IntegerRange {
        key: String,
        min: Option<i64>,
        max: Option<i64>,
    },
    VersionRange {
        key: String,
        requirement: String,
    },
    Custom {
        kind: String,
        payload: serde_json::Value,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeScope {
    pub predicates: Vec<ScopePredicate>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeContext {
    pub values: BTreeMap<String, ScopeValue>,
}
impl KnowledgeContext {
    /// Accept CLI primitive objects as well as the stable typed context schema.
    pub fn from_json(value: serde_json::Value) -> Result<Self> {
        if value
            .get("values")
            .is_some_and(serde_json::Value::is_object)
        {
            return Ok(serde_json::from_value(value)?);
        }
        let object = value
            .as_object()
            .ok_or_else(|| Error::InvalidInput("Context must be an object".into()))?;
        let values = object
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    serde_json::Value::String(v) => ScopeValue::String(v.clone()),
                    serde_json::Value::Bool(v) => ScopeValue::Boolean(*v),
                    serde_json::Value::Number(v) if v.as_i64().is_some() => {
                        ScopeValue::Integer(v.as_i64().unwrap())
                    }
                    _ => {
                        return Err(Error::InvalidInput(format!(
                            "Context {key} requires a string, integer, or boolean"
                        )));
                    }
                };
                Ok((key.clone(), value))
            })
            .collect::<Result<_>>()?;
        Ok(Self { values })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicabilityStatus {
    Applicable,
    Inapplicable,
    PartiallyKnown,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeApplicability {
    pub status: ApplicabilityStatus,
    pub matched: Vec<ScopePredicate>,
    pub failed: Vec<ScopePredicate>,
    pub unknown: Vec<ScopePredicate>,
}
pub trait KnowledgeApplicabilityEvaluator {
    fn evaluate(
        &self,
        scope: &KnowledgeScope,
        context: &KnowledgeContext,
    ) -> KnowledgeApplicability;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicApplicabilityEvaluator;
impl ScopePredicate {
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Equals { key, .. }
            | Self::NotEquals { key, .. }
            | Self::In { key, .. }
            | Self::Exists { key }
            | Self::Bool { key, .. }
            | Self::IntegerRange { key, .. }
            | Self::VersionRange { key, .. } => Some(key),
            Self::Custom { .. } => None,
        }
    }
    fn matches(&self, value: &ScopeValue) -> Option<bool> {
        Some(match self {
            Self::Equals {
                value: expected, ..
            } => value == expected,
            Self::NotEquals {
                value: expected, ..
            } => value != expected,
            Self::In { values, .. } => values.contains(value),
            Self::Exists { .. } => true,
            Self::Bool { expected, .. } => value == &ScopeValue::Boolean(*expected),
            Self::IntegerRange { min, max, .. } => {
                if min.zip(*max).is_some_and(|(a, b)| a > b) {
                    return None;
                }
                match value {
                    ScopeValue::Integer(v) => {
                        min.is_none_or(|m| *v >= m) && max.is_none_or(|m| *v <= m)
                    }
                    _ => false,
                }
            }
            Self::VersionRange { requirement, .. } => {
                let req = VersionReq::parse(requirement).ok()?;
                match value {
                    ScopeValue::Version(v) | ScopeValue::String(v) => {
                        req.matches(&Version::parse(v).ok()?)
                    }
                    _ => false,
                }
            }
            Self::Custom { .. } => return None,
        })
    }
}
impl KnowledgeApplicabilityEvaluator for DeterministicApplicabilityEvaluator {
    fn evaluate(
        &self,
        scope: &KnowledgeScope,
        context: &KnowledgeContext,
    ) -> KnowledgeApplicability {
        let mut result = KnowledgeApplicability {
            status: ApplicabilityStatus::Applicable,
            matched: vec![],
            failed: vec![],
            unknown: vec![],
        };
        let mut predicates = scope.predicates.clone();
        predicates
            .sort_by_cached_key(|p| serde_json::to_string(p).expect("predicate serialization"));
        predicates.dedup();
        for predicate in predicates {
            let outcome = predicate
                .key()
                .and_then(|k| context.values.get(k))
                .and_then(|v| predicate.matches(v));
            match outcome {
                Some(true) => result.matched.push(predicate),
                Some(false) => result.failed.push(predicate),
                None => result.unknown.push(predicate),
            }
        }
        result.status = if !result.failed.is_empty() {
            ApplicabilityStatus::Inapplicable
        } else if result.unknown.is_empty() {
            ApplicabilityStatus::Applicable
        } else if result.matched.is_empty() {
            ApplicabilityStatus::Unknown
        } else {
            ApplicabilityStatus::PartiallyKnown
        };
        result
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScopeSpecificity {
    pub exact_predicates: usize,
    pub bounded_predicates: usize,
    pub existence_predicates: usize,
}
impl KnowledgeScope {
    pub fn specificity(&self) -> ScopeSpecificity {
        let mut dimensions = BTreeMap::new();
        for p in &self.predicates {
            let rank = match p {
                ScopePredicate::Equals { .. } | ScopePredicate::Bool { .. } => 3,
                ScopePredicate::In { .. }
                | ScopePredicate::IntegerRange { .. }
                | ScopePredicate::VersionRange { .. }
                | ScopePredicate::NotEquals { .. } => 2,
                ScopePredicate::Exists { .. } => 1,
                ScopePredicate::Custom { .. } => 0,
            };
            if let Some(key) = p.key() {
                let r = dimensions.entry(key).or_insert(0);
                *r = (*r).max(rank);
            }
        }
        ScopeSpecificity {
            exact_predicates: dimensions.values().filter(|&&r| r == 3).count(),
            bounded_predicates: dimensions.values().filter(|&&r| r == 2).count(),
            existence_predicates: dimensions.values().filter(|&&r| r == 1).count(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRelation {
    Equal,
    Narrower,
    Broader,
    Overlapping,
    Disjoint,
    Unknown,
}
pub trait ScopeRelationEvaluator {
    fn compare(&self, a: &KnowledgeScope, b: &KnowledgeScope) -> ScopeRelation;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicScopeRelationEvaluator;

// A conjunction is a product of independent dimension domains. Normalize repeated
// constraints before containment checks; predicate counts do not prove containment.
#[derive(Clone, Debug, Default)]
struct Domain {
    finite: Option<BTreeSet<ScopeValue>>,
    excluded: BTreeSet<ScopeValue>,
    integer: Option<(i64, i64)>,
    versions: BTreeSet<String>,
    version_interval: Option<VersionInterval>,
}
impl Domain {
    fn accepts(&self, v: &ScopeValue) -> bool {
        !self.excluded.contains(v)
            && self.finite.as_ref().is_none_or(|set| set.contains(v))
            && self
                .integer
                .is_none_or(|(a, b)| matches!(v,ScopeValue::Integer(n) if *n>=a && *n<=b))
            && self.versions.iter().all(|r| {
                ScopePredicate::VersionRange {
                    key: String::new(),
                    requirement: r.clone(),
                }
                .matches(v)
                    == Some(true)
            })
    }
    fn empty(&self) -> bool {
        self.integer.is_some_and(|(a, b)| a > b)
            || self.finite.as_ref().is_some_and(BTreeSet::is_empty)
            || self
                .version_interval
                .as_ref()
                .is_some_and(|v| v.upper.as_ref().is_some_and(|u| u <= &v.lower))
    }
    fn subset(&self, other: &Self) -> Option<bool> {
        if self.empty() {
            return Some(true);
        }
        if let Some(set) = &self.finite {
            return Some(set.iter().all(|v| other.accepts(v)));
        }
        if other.finite.is_some() {
            return Some(false);
        }
        if !other.excluded.iter().all(|v| !self.accepts(v)) {
            return Some(false);
        }
        if let Some((a, b)) = other.integer
            && !self.integer.is_some_and(|(x, y)| x >= a && y <= b)
        {
            return Some(false);
        }
        if !other.versions.is_subset(&self.versions) {
            if self.versions.is_empty() {
                return Some(false);
            }
            return match (&self.version_interval, &other.version_interval) {
                (Some(a), Some(b)) => Some(
                    a.lower >= b.lower
                        && (b.upper.is_none()
                            || a.upper
                                .as_ref()
                                .is_some_and(|v| Some(v) <= b.upper.as_ref())),
                ),
                _ => None,
            };
        }
        Some(true)
    }
    fn disjoint(&self, other: &Self) -> Option<bool> {
        if self.empty() || other.empty() {
            return Some(true);
        }
        if let Some(set) = &self.finite {
            return Some(set.iter().all(|v| !other.accepts(v)));
        }
        if let Some(set) = &other.finite {
            return Some(set.iter().all(|v| !self.accepts(v)));
        }
        if let (Some((a, b)), Some((x, y))) = (self.integer, other.integer) {
            return Some(b < x || y < a);
        }
        if (self.integer.is_some() && !other.versions.is_empty())
            || (other.integer.is_some() && !self.versions.is_empty())
        {
            return Some(true);
        }
        if !self.versions.is_empty() || !other.versions.is_empty() {
            return match (&self.version_interval, &other.version_interval) {
                (Some(a), Some(b)) => Some(
                    a.upper.as_ref().is_some_and(|v| v <= &b.lower)
                        || b.upper.as_ref().is_some_and(|v| v <= &a.lower),
                ),
                _ => None,
            };
        }
        Some(false)
    }
}
fn domains(scope: &KnowledgeScope) -> Option<BTreeMap<String, Domain>> {
    let mut map = BTreeMap::<String, Domain>::new();
    for p in &scope.predicates {
        let d = map.entry(p.key()?.into()).or_default();
        let finite = match p {
            ScopePredicate::Equals { value, .. } => Some(BTreeSet::from([value.clone()])),
            ScopePredicate::Bool { expected, .. } => {
                Some(BTreeSet::from([ScopeValue::Boolean(*expected)]))
            }
            ScopePredicate::In { values, .. } => Some(values.iter().cloned().collect()),
            ScopePredicate::NotEquals { value, .. } => {
                d.excluded.insert(value.clone());
                None
            }
            ScopePredicate::IntegerRange { min, max, .. } => {
                let (a, b) = d.integer.unwrap_or((i64::MIN, i64::MAX));
                d.integer = Some((
                    a.max(min.unwrap_or(i64::MIN)),
                    b.min(max.unwrap_or(i64::MAX)),
                ));
                None
            }
            ScopePredicate::VersionRange { requirement, .. } => {
                VersionReq::parse(requirement).ok()?;
                d.versions.insert(requirement.clone());
                None
            }
            ScopePredicate::Exists { .. } => None,
            ScopePredicate::Custom { .. } => return None,
        };
        if let Some(set) = finite {
            d.finite = Some(match d.finite.take() {
                Some(old) => old.intersection(&set).cloned().collect(),
                None => set,
            });
        }
    }
    for d in map.values_mut() {
        if !d.versions.is_empty() {
            d.version_interval = version_interval(&d.versions);
        }
        if let Some(set) = d.finite.take() {
            d.finite = Some(set.into_iter().filter(|v| d.accepts(v)).collect());
        }
    }
    Some(map)
}
impl ScopeRelationEvaluator for DeterministicScopeRelationEvaluator {
    fn compare(&self, a: &KnowledgeScope, b: &KnowledgeScope) -> ScopeRelation {
        use ScopeRelation::*;
        let (Some(a), Some(b)) = (domains(a), domains(b)) else {
            return Unknown;
        };
        if a.values().any(Domain::empty) || b.values().any(Domain::empty) {
            return Disjoint;
        }
        let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
        let (mut ab, mut ba, mut overlap) = (Some(true), Some(true), true);
        for key in keys {
            match (a.get(key), b.get(key)) {
                (Some(x), Some(y)) => {
                    match x.disjoint(y) {
                        Some(true) => return Disjoint,
                        None => overlap = false,
                        _ => {}
                    }
                    ab = and(ab, x.subset(y));
                    ba = and(ba, y.subset(x));
                }
                (Some(_), None) => ba = Some(false),
                (None, Some(_)) => ab = Some(false),
                _ => {}
            }
        }
        match (ab, ba) {
            (Some(true), Some(true)) => Equal,
            (Some(true), Some(false)) => Narrower,
            (Some(false), Some(true)) => Broader,
            _ if overlap => Overlapping,
            _ => Unknown,
        }
    }
}
fn and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    if a == Some(false) || b == Some(false) {
        Some(false)
    } else if a.is_none() || b.is_none() {
        None
    } else {
        Some(true)
    }
}

// Stable release semver requirements reduce to half-open intervals. Prerelease
// comparator sets have different admission rules; return Unknown for those.
#[derive(Clone, Debug)]
struct VersionInterval {
    lower: Version,
    upper: Option<Version>,
}
fn next_release(v: &Version) -> Option<Version> {
    Some(if let Some(patch) = v.patch.checked_add(1) {
        Version::new(v.major, v.minor, patch)
    } else if let Some(minor) = v.minor.checked_add(1) {
        Version::new(v.major, minor, 0)
    } else {
        Version::new(v.major.checked_add(1)?, 0, 0)
    })
}
fn version_interval(requirements: &BTreeSet<String>) -> Option<VersionInterval> {
    use semver::Op;
    let mut interval = VersionInterval {
        lower: Version::new(0, 0, 0),
        upper: None,
    };
    for requirement in requirements {
        for c in VersionReq::parse(requirement).ok()?.comparators {
            if !c.pre.is_empty() {
                return None;
            }
            let lower = Version::new(c.major, c.minor.unwrap_or(0), c.patch.unwrap_or(0));
            let partial_upper = || -> Option<Version> {
                if c.patch.is_some() {
                    next_release(&lower)
                } else if let Some(minor) = c.minor {
                    Some(Version::new(c.major, minor.checked_add(1)?, 0))
                } else {
                    Some(Version::new(c.major.checked_add(1)?, 0, 0))
                }
            };
            let (lo, hi) = match c.op {
                Op::Exact | Op::Wildcard => (Some(lower.clone()), Some(partial_upper()?)),
                Op::Greater => (Some(partial_upper()?), None),
                Op::GreaterEq => (Some(lower.clone()), None),
                Op::Less => (None, Some(lower.clone())),
                Op::LessEq => (None, Some(partial_upper()?)),
                Op::Tilde => (
                    Some(lower.clone()),
                    Some(if let Some(minor) = c.minor {
                        Version::new(c.major, minor.checked_add(1)?, 0)
                    } else {
                        Version::new(c.major.checked_add(1)?, 0, 0)
                    }),
                ),
                Op::Caret => (
                    Some(lower.clone()),
                    Some(if c.major > 0 || c.minor.is_none() {
                        Version::new(c.major.checked_add(1)?, 0, 0)
                    } else if c.minor.unwrap_or(0) > 0 || c.patch.is_none() {
                        Version::new(0, c.minor.unwrap_or(0).checked_add(1)?, 0)
                    } else {
                        Version::new(0, 0, c.patch.unwrap_or(0).checked_add(1)?)
                    }),
                ),
                _ => return None,
            };
            if let Some(lo) = lo {
                interval.lower = interval.lower.max(lo);
            }
            if let Some(hi) = hi {
                interval.upper = Some(interval.upper.map_or(hi.clone(), |old| old.min(hi)));
            }
        }
    }
    Some(interval)
}
