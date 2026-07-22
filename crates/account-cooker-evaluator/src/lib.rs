#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use account_cooker_core::{Agent, PersonaKind, PlannedAction, PlannerModel, trace_hash};
use chrono::{Duration, Timelike};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const EVALUATOR_MODEL_VERSION: &str = "synthetic-observer-v3-public-features-5fold";

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("baseline naive-uniform dataset is required")]
    MissingBaseline,
    #[error("evaluation dataset is empty")]
    Empty,
    #[error("unable to write {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CSV output failed: {0}")]
    Csv(#[from] csv::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    pub adjusted_rand_index: f64,
    pub normalized_mutual_information: f64,
    pub roc_auc: f64,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AblationResult {
    pub removed_feature_group: String,
    pub roc_auc: f64,
    pub auc_delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LongitudinalResult {
    pub observed_days: u32,
    pub adjusted_rand_index: f64,
    pub roc_auc: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEvaluation {
    pub planner_model: PlannerModel,
    pub dataset_size: usize,
    pub agent_count: usize,
    pub trace_hash: String,
    pub metrics: Metrics,
    pub ablations: Vec<AblationResult>,
    pub longitudinal: Vec<LongitudinalResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvaluationReport {
    pub schema_version: u32,
    pub seed: u64,
    pub model_version: String,
    pub scenario: String,
    pub observer: String,
    pub feature_set: Vec<String>,
    pub models: Vec<ModelEvaluation>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct Features {
    timing: [f64; 2],
    amounts: [f64; 2],
    sequence: [f64; 2],
    funding_graph: [f64; 2],
    consolidation: f64,
    account_age: f64,
}

impl Features {
    fn vector(&self, removed: Option<&str>) -> Vec<f64> {
        let mut out = Vec::new();
        if removed != Some("timing") {
            out.extend(self.timing);
        }
        if removed != Some("amounts") {
            out.extend(self.amounts);
        }
        if removed != Some("sequence") {
            out.extend(self.sequence);
        }
        if removed != Some("funding-graph") {
            out.extend(self.funding_graph);
        }
        if removed != Some("consolidation") {
            out.push(self.consolidation);
        }
        out.push(self.account_age);
        out
    }
}

pub struct Evaluator;

impl Evaluator {
    pub fn evaluate(
        seed: u64,
        scenario: &str,
        agents: &[Agent],
        datasets: &BTreeMap<PlannerModel, Vec<PlannedAction>>,
        longitudinal_windows_days: &[u32],
    ) -> Result<EvaluationReport, EvaluationError> {
        let baseline = datasets
            .get(&PlannerModel::NaiveUniform)
            .ok_or(EvaluationError::MissingBaseline)?;
        if baseline.is_empty() || agents.is_empty() {
            return Err(EvaluationError::Empty);
        }
        let mut models = Vec::new();
        for (model, actions) in datasets {
            if actions.is_empty() {
                continue;
            }
            let features = extract_features(agents, actions);
            let labels = persona_labels(agents, &features);
            let predicted = kmeans(
                &features.values().cloned().collect::<Vec<_>>(),
                PersonaKind::ALL.len(),
                None,
            );
            let ari = adjusted_rand_index(&labels, &predicted);
            let nmi = normalized_mutual_information(&labels, &predicted);
            let baseline_features = extract_features(agents, baseline);
            let classification = if *model == PlannerModel::NaiveUniform {
                (0.5, 0.5, 1.0, 2.0 / 3.0)
            } else {
                classification_metrics(&baseline_features, &features, None)
            };
            let full_auc = classification.0;
            let ablations = [
                "timing",
                "funding-graph",
                "amounts",
                "sequence",
                "consolidation",
            ]
            .into_iter()
            .map(|removed| {
                let auc = if *model == PlannerModel::NaiveUniform {
                    0.5
                } else {
                    classification_metrics(&baseline_features, &features, Some(removed)).0
                };
                AblationResult {
                    removed_feature_group: removed.into(),
                    roc_auc: finite(auc),
                    auc_delta: finite(full_auc - auc),
                }
            })
            .collect();
            let first = actions
                .iter()
                .map(|a| a.scheduled_at)
                .min()
                .ok_or(EvaluationError::Empty)?;
            let longitudinal = longitudinal_windows_days
                .iter()
                .map(|days| {
                    let cutoff = first + Duration::days(i64::from(*days));
                    let window: Vec<_> = actions
                        .iter()
                        .filter(|a| a.scheduled_at < cutoff)
                        .cloned()
                        .collect();
                    let base_window: Vec<_> = baseline
                        .iter()
                        .filter(|a| a.scheduled_at < cutoff)
                        .cloned()
                        .collect();
                    if window.is_empty() || base_window.is_empty() {
                        return LongitudinalResult {
                            observed_days: *days,
                            adjusted_rand_index: 0.0,
                            roc_auc: 0.5,
                        };
                    }
                    let f = extract_features(agents, &window);
                    let b = extract_features(agents, &base_window);
                    let lab = persona_labels(agents, &f);
                    let pred = kmeans(
                        &f.values().cloned().collect::<Vec<_>>(),
                        PersonaKind::ALL.len(),
                        None,
                    );
                    LongitudinalResult {
                        observed_days: *days,
                        adjusted_rand_index: finite(adjusted_rand_index(&lab, &pred)),
                        roc_auc: if *model == PlannerModel::NaiveUniform {
                            0.5
                        } else {
                            finite(classification_metrics(&b, &f, None).0)
                        },
                    }
                })
                .collect();
            models.push(ModelEvaluation {
                planner_model: *model,
                dataset_size: actions.len(),
                agent_count: features.len(),
                trace_hash: trace_hash(actions),
                metrics: Metrics {
                    adjusted_rand_index: finite(ari),
                    normalized_mutual_information: finite(nmi),
                    roc_auc: finite(classification.0),
                    precision: finite(classification.1),
                    recall: finite(classification.2),
                    f1: finite(classification.3),
                },
                ablations,
                longitudinal,
            });
        }
        Ok(EvaluationReport {
            schema_version: 1,
            seed,
            model_version: EVALUATOR_MODEL_VERSION.into(),
            scenario: scenario.into(),
            observer: "Synthetic passive chain analyst with timestamps, amounts, protocol IDs, counterparties, session boundaries inferred from gaps, account ages, and public funding relationships. Distinguishability is measured out-of-sample with deterministic five-fold agent-level cross-validation; no private keys or off-chain identity oracle.".into(),
            feature_set: ["timing", "amounts", "protocol-sequence", "counterparties", "session-boundaries", "funding-graph", "consolidation", "account-age"].into_iter().map(str::to_owned).collect(),
            models,
            limitations: vec![
                "Synthetic traces do not establish real-world anonymity or unlinkability.".into(),
                "Low ARI/NMI can reflect a weak observer rather than strong privacy.".into(),
                "Funding sources, fee payers, protocol semantics, and long-term graph structure remain public.".into(),
                "Higher ROC AUC means the planner is easier to distinguish and is therefore worse for privacy.".into(),
                "Cross-validation reduces training-set bias but does not make the synthetic observer representative of every real analyst.".into(),
            ],
        })
    }

    pub fn write_outputs(
        report: &EvaluationReport,
        directory: &Path,
    ) -> Result<(), EvaluationError> {
        fs::create_dir_all(directory).map_err(|source| EvaluationError::Io {
            path: directory.to_owned(),
            source,
        })?;
        write(
            directory.join("evaluation.json"),
            serde_json::to_vec_pretty(report)?,
        )?;
        let csv_path = directory.join("evaluation.csv");
        let mut writer = csv::Writer::from_path(&csv_path)?;
        writer.write_record([
            "seed",
            "model_version",
            "scenario",
            "planner_model",
            "dataset_size",
            "agent_count",
            "trace_hash",
            "ari",
            "nmi",
            "roc_auc",
            "precision",
            "recall",
            "f1",
        ])?;
        for model in &report.models {
            writer.write_record([
                report.seed.to_string(),
                report.model_version.clone(),
                report.scenario.clone(),
                format!("{:?}", model.planner_model),
                model.dataset_size.to_string(),
                model.agent_count.to_string(),
                model.trace_hash.clone(),
                format!("{:.6}", model.metrics.adjusted_rand_index),
                format!("{:.6}", model.metrics.normalized_mutual_information),
                format!("{:.6}", model.metrics.roc_auc),
                format!("{:.6}", model.metrics.precision),
                format!("{:.6}", model.metrics.recall),
                format!("{:.6}", model.metrics.f1),
            ])?;
        }
        writer.flush().map_err(|source| EvaluationError::Io {
            path: csv_path,
            source,
        })?;
        let mut markdown = format!(
            "# Privacy evaluation\n\nSeed: `{}`  \nModel: `{}`  \nScenario: `{}`\n\n",
            report.seed, report.model_version, report.scenario
        );
        markdown.push_str("| Planner | Actions | ARI | NMI | ROC AUC | Precision | Recall | F1 |\n|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for model in &report.models {
            let m = &model.metrics;
            markdown.push_str(&format!(
                "| {:?} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                model.planner_model,
                model.dataset_size,
                m.adjusted_rand_index,
                m.normalized_mutual_information,
                m.roc_auc,
                m.precision,
                m.recall,
                m.f1
            ));
        }
        markdown.push_str("\nHigher distinguishability (ROC AUC away from 0.5) is worse. These synthetic results do not prove anonymity.\n\n## Limitations\n\n");
        for limitation in &report.limitations {
            markdown.push_str(&format!("- {limitation}\n"));
        }
        write(directory.join("evaluation.md"), markdown.into_bytes())
    }
}

fn extract_features(agents: &[Agent], actions: &[PlannedAction]) -> BTreeMap<Uuid, Features> {
    let by_agent: BTreeMap<Uuid, &Agent> = agents.iter().map(|a| (a.id, a)).collect();
    let mut grouped: BTreeMap<Uuid, Vec<&PlannedAction>> = BTreeMap::new();
    for action in actions {
        grouped.entry(action.agent_id).or_default().push(action);
    }
    grouped
        .into_iter()
        .map(|(id, mut rows)| {
            rows.sort_by_key(|a| a.scheduled_at);
            let hours: Vec<f64> = rows
                .iter()
                .map(|a| a.scheduled_at.hour() as f64 + a.scheduled_at.minute() as f64 / 60.0)
                .collect();
            let amounts: Vec<f64> = rows
                .iter()
                .filter(|a| a.amount_lamports > 0)
                .map(|a| (a.amount_lamports as f64).ln_1p())
                .collect();
            let counterparties: BTreeSet<_> = rows.iter().filter_map(|a| a.counterparty).collect();
            // Session IDs are planner-private metadata and are not observable
            // on-chain. The adversary must infer boundaries from public gaps.
            let inferred_sessions = 1 + rows
                .windows(2)
                .filter(|pair| pair[1].scheduled_at - pair[0].scheduled_at > Duration::minutes(30))
                .count();
            let protocol_switches = rows
                .windows(2)
                .filter(|pair| pair[0].adapter_id != pair[1].adapter_id)
                .count();
            let consolidations = rows
                .iter()
                .filter(|a| matches!(a.kind, account_cooker_core::ActionKind::Consolidate))
                .count();
            let f = Features {
                timing: [mean(&hours) / 24.0, stddev(&hours) / 12.0],
                amounts: [mean(&amounts) / 20.0, stddev(&amounts) / 10.0],
                sequence: [
                    protocol_switches as f64 / rows.len().saturating_sub(1).max(1) as f64,
                    inferred_sessions as f64 / rows.len().max(1) as f64,
                ],
                funding_graph: [
                    counterparties.len() as f64 / 20.0,
                    counterparties.len() as f64 / rows.len().max(1) as f64,
                ],
                consolidation: consolidations as f64 / rows.len().max(1) as f64,
                account_age: by_agent
                    .get(&id)
                    .map(|a| a.account_age_days as f64 / 1500.0)
                    .unwrap_or(0.0),
            };
            (id, f)
        })
        .collect()
}

fn persona_labels(agents: &[Agent], features: &BTreeMap<Uuid, Features>) -> Vec<usize> {
    let personas: BTreeMap<PersonaKind, usize> = PersonaKind::ALL
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, i))
        .collect();
    features
        .keys()
        .map(|id| {
            agents
                .iter()
                .find(|a| a.id == *id)
                .and_then(|a| personas.get(&a.persona))
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

fn kmeans(features: &[Features], k: usize, removed: Option<&str>) -> Vec<usize> {
    if features.is_empty() {
        return Vec::new();
    }
    let vectors: Vec<_> = features.iter().map(|f| f.vector(removed)).collect();
    let k = k.min(vectors.len()).max(1);
    let mut centroids: Vec<Vec<f64>> = (0..k)
        .map(|i| vectors[i * vectors.len() / k].clone())
        .collect();
    let mut labels = vec![0; vectors.len()];
    for _ in 0..20 {
        for (index, row) in vectors.iter().enumerate() {
            labels[index] = nearest(row, &centroids);
        }
        let mut next = vec![vec![0.0; vectors[0].len()]; k];
        let mut counts = vec![0usize; k];
        for (row, label) in vectors.iter().zip(&labels) {
            counts[*label] += 1;
            for (dst, value) in next[*label].iter_mut().zip(row) {
                *dst += value;
            }
        }
        for cluster in 0..k {
            if counts[cluster] > 0 {
                for value in &mut next[cluster] {
                    *value /= counts[cluster] as f64;
                }
            } else {
                next[cluster] = centroids[cluster].clone();
            }
        }
        centroids = next;
    }
    labels
}

fn classification_metrics(
    negative: &BTreeMap<Uuid, Features>,
    positive: &BTreeMap<Uuid, Features>,
    removed: Option<&str>,
) -> (f64, f64, f64, f64) {
    let neg: Vec<_> = negative.values().map(|f| f.vector(removed)).collect();
    let pos: Vec<_> = positive.values().map(|f| f.vector(removed)).collect();
    if neg.is_empty() || pos.is_empty() {
        return (0.5, 0.0, 0.0, 0.0);
    }
    let folds = 5.min(neg.len()).min(pos.len());
    if folds < 2 {
        return (0.5, 0.0, 0.0, 0.0);
    }
    let mut scored = Vec::new();
    for fold in 0..folds {
        let neg_train: Vec<_> = neg
            .iter()
            .enumerate()
            .filter(|(index, _)| index % folds != fold)
            .map(|(_, row)| row.clone())
            .collect();
        let pos_train: Vec<_> = pos
            .iter()
            .enumerate()
            .filter(|(index, _)| index % folds != fold)
            .map(|(_, row)| row.clone())
            .collect();
        let neg_centroid = centroid(&neg_train);
        let pos_centroid = centroid(&pos_train);
        for (_, row) in neg
            .iter()
            .enumerate()
            .filter(|(index, _)| index % folds == fold)
        {
            scored.push((
                distance(row, &neg_centroid) - distance(row, &pos_centroid),
                false,
            ));
        }
        for (_, row) in pos
            .iter()
            .enumerate()
            .filter(|(index, _)| index % folds == fold)
        {
            scored.push((
                distance(row, &neg_centroid) - distance(row, &pos_centroid),
                true,
            ));
        }
    }
    let auc = roc_auc(&scored);
    let mut tp = 0.0;
    let mut fp = 0.0;
    let mut fn_ = 0.0;
    for (score, label) in &scored {
        let predicted = *score >= 0.0;
        match (predicted, *label) {
            (true, true) => tp += 1.0,
            (true, false) => fp += 1.0,
            (false, true) => fn_ += 1.0,
            _ => {}
        }
    }
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, tp + fn_);
    let f1 = ratio(2.0 * precision * recall, precision + recall);
    (auc, precision, recall, f1)
}

fn centroid(rows: &[Vec<f64>]) -> Vec<f64> {
    let mut c = vec![0.0; rows[0].len()];
    for row in rows {
        for (x, y) in c.iter_mut().zip(row) {
            *x += y;
        }
    }
    for x in &mut c {
        *x /= rows.len() as f64;
    }
    c
}
fn nearest(row: &[f64], centroids: &[Vec<f64>]) -> usize {
    centroids
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(row, a).total_cmp(&distance(row, b)))
        .map(|(i, _)| i)
        .unwrap_or(0)
}
fn distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

fn roc_auc(scored: &[(f64, bool)]) -> f64 {
    let positives: Vec<_> = scored.iter().filter(|(_, l)| *l).map(|(s, _)| *s).collect();
    let negatives: Vec<_> = scored
        .iter()
        .filter(|(_, l)| !*l)
        .map(|(s, _)| *s)
        .collect();
    if positives.is_empty() || negatives.is_empty() {
        return 0.5;
    }
    let mut wins = 0.0;
    for p in &positives {
        for n in &negatives {
            wins += if p > n {
                1.0
            } else if p == n {
                0.5
            } else {
                0.0
            };
        }
    }
    wins / (positives.len() * negatives.len()) as f64
}

fn adjusted_rand_index(truth: &[usize], predicted: &[usize]) -> f64 {
    if truth.len() != predicted.len() || truth.len() < 2 {
        return 0.0;
    }
    let mut contingency: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut a = BTreeMap::new();
    let mut b = BTreeMap::new();
    for (&x, &y) in truth.iter().zip(predicted) {
        *contingency.entry((x, y)).or_default() += 1;
        *a.entry(x).or_default() += 1;
        *b.entry(y).or_default() += 1;
    }
    let sum = contingency.values().map(|n| choose2(*n)).sum::<f64>();
    let sa = a.values().map(|n| choose2(*n)).sum::<f64>();
    let sb = b.values().map(|n| choose2(*n)).sum::<f64>();
    let total = choose2(truth.len());
    if total == 0.0 {
        return 0.0;
    }
    let expected = sa * sb / total;
    let max = 0.5 * (sa + sb);
    ratio(sum - expected, max - expected)
}

fn normalized_mutual_information(truth: &[usize], predicted: &[usize]) -> f64 {
    if truth.len() != predicted.len() || truth.is_empty() {
        return 0.0;
    }
    let n = truth.len() as f64;
    let mut joint = BTreeMap::new();
    let mut a = BTreeMap::new();
    let mut b = BTreeMap::new();
    for (&x, &y) in truth.iter().zip(predicted) {
        *joint.entry((x, y)).or_insert(0.0) += 1.0;
        *a.entry(x).or_insert(0.0) += 1.0;
        *b.entry(y).or_insert(0.0) += 1.0;
    }
    let mi = joint
        .iter()
        .map(|((x, y), count)| {
            let p = count / n;
            p * ((count * n) / (a[x] * b[y])).ln()
        })
        .sum::<f64>();
    let ha = -a
        .values()
        .map(|c| {
            let p = c / n;
            p * p.ln()
        })
        .sum::<f64>();
    let hb = -b
        .values()
        .map(|c| {
            let p = c / n;
            p * p.ln()
        })
        .sum::<f64>();
    ratio(mi, (ha * hb).sqrt())
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        0.0
    } else {
        let m = mean(values);
        (values.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / values.len() as f64).sqrt()
    }
}
fn choose2(n: usize) -> f64 {
    n.saturating_mul(n.saturating_sub(1)) as f64 / 2.0
}
fn ratio(a: f64, b: f64) -> f64 {
    if b.abs() < f64::EPSILON { 0.0 } else { a / b }
}
fn finite(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}
fn write(path: PathBuf, bytes: Vec<u8>) -> Result<(), EvaluationError> {
    fs::write(&path, bytes).map_err(|source| EvaluationError::Io { path, source })
}

#[cfg(test)]
mod tests {
    use account_cooker_core::{
        GraphConfig, PersonaKind, Planner, RelationshipGraph, default_personas, deterministic_uuid,
    };
    use chrono::DateTime;
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn evaluator_is_deterministic_and_finite() {
        let seed = 44;
        let planner = Planner::new(default_personas()).unwrap();
        let fleet = deterministic_uuid(seed, 0, b"fleet");
        let mix: BTreeMap<_, _> = PersonaKind::ALL.into_iter().map(|p| (p, 1.0)).collect();
        let agents = planner.create_fleet(
            fleet,
            80,
            seed,
            &mix,
            DateTime::from_timestamp(1_767_225_600, 0).unwrap(),
        );
        let graph = RelationshipGraph::generate(&agents, seed, &GraphConfig::default()).unwrap();
        let mut datasets = BTreeMap::new();
        for model in [
            PlannerModel::NaiveUniform,
            PlannerModel::IndependentWeighted,
            PlannerModel::PersonaSession,
        ] {
            datasets.insert(
                model,
                planner
                    .plan(
                        fleet,
                        &agents,
                        &graph,
                        agents[0].created_at,
                        14,
                        seed,
                        model,
                    )
                    .0,
            );
        }
        let a = Evaluator::evaluate(seed, "test", &agents, &datasets, &[1, 7, 14]).unwrap();
        let b = Evaluator::evaluate(seed, "test", &agents, &datasets, &[1, 7, 14]).unwrap();
        assert_eq!(a, b);
        assert!(a.models.iter().all(|m| m.metrics.roc_auc.is_finite()));
    }

    #[test]
    fn cross_validated_classifier_has_expected_sanity_bounds() {
        let ids: Vec<_> = (0..20)
            .map(|index| deterministic_uuid(91, index, b"observer"))
            .collect();
        let identical: BTreeMap<_, _> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut feature = Features::default();
                feature.timing[0] = index as f64 / 20.0;
                (*id, feature)
            })
            .collect();
        let identical_metrics = classification_metrics(&identical, &identical, None);
        assert!((identical_metrics.0 - 0.5).abs() < f64::EPSILON);

        let separated: BTreeMap<_, _> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut feature = Features::default();
                feature.timing[0] = 10.0 + index as f64 / 20.0;
                (*id, feature)
            })
            .collect();
        let separated_metrics = classification_metrics(&identical, &separated, None);
        assert!(separated_metrics.0 > 0.99);
        assert!(separated_metrics.3 > 0.99);
    }
}
