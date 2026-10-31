//! Typed L1 annotation contracts. A source is a namespace; patch maps contain
//! typed values, with null clearing a key and omitted keys left unchanged.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AnnotationTarget {
    /// Open enum; only session targets are supported in preview.
    pub kind: String,
    pub session_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum AnnotationValue {
    Number { v: f64 },
    String { v: String },
    Bool { v: bool },
    Duration { ms: u64 },
    Money { amount: String, currency: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub target: AnnotationTarget,
    pub key: String,
    /// Omitted by a gateway when the value exceeds its ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<AnnotationValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    pub sensitivity: String,
    pub writer: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AnnotationSetParams {
    pub target: AnnotationTarget,
    pub key: String,
    pub value: AnnotationValue,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    /// One-key shortcut TTL: 60 seconds through 24 hours.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationSetResult {
    pub annotation: Annotation,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationDeleteParams {
    pub target: AnnotationTarget,
    pub key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationDeleteResult {
    pub deleted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPatchParams {
    pub target: AnnotationTarget,
    #[serde(default)]
    pub source: Option<String>,
    pub values: BTreeMap<String, Option<AnnotationValue>>,
    /// Per touched key, from 1 millisecond through 24 hours.
    #[serde(default)]
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub seq: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationChangedEvent {
    pub target: AnnotationTarget,
    pub source: String,
    /// Resulting source map, including per-key writer, expiry and sensitivity.
    pub values: BTreeMap<String, Annotation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationPatchResult {
    /// False means the sequence was already accepted or is stale.
    pub applied: bool,
    pub target: AnnotationTarget,
    pub source: String,
    pub values: BTreeMap<String, Annotation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationRemovedEvent {
    pub target: AnnotationTarget,
    pub key: String,
    /// target_gone; expiry and explicit clears emit annotation.changed.
    pub reason: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationListParams {
    #[serde(default)]
    pub target: Option<AnnotationTarget>,
    #[serde(default)]
    pub namespace: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "engine", derive(schemars::JsonSchema))]
pub struct AnnotationListResult {
    /// Snapshot cursor; subscribe from it to close the list/watch race.
    pub cursor: String,
    pub annotations: Vec<Annotation>,
}
