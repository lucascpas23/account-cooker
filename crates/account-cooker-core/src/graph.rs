use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::Agent;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("household size must be at least two")]
    InvalidHouseholdSize,
    #[error("relationship graph contains a self-loop")]
    SelfLoop,
    #[error("relationship graph is disconnected")]
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    pub min_household_size: usize,
    pub max_household_size: usize,
    pub strong_edge_probability: f64,
    pub cross_group_probability: f64,
    pub require_connected: bool,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            min_household_size: 3,
            max_household_size: 8,
            strong_edge_probability: 0.62,
            cross_group_probability: 0.018,
            require_connected: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationshipEdge {
    pub a: Uuid,
    pub b: Uuid,
    pub strength: f64,
    pub household: u32,
    pub protocol_affinity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RelationshipGraph {
    pub edges: Vec<RelationshipEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphMetrics {
    pub node_count: usize,
    pub edge_count: usize,
    pub connected_components: usize,
    pub mean_degree: f64,
    pub density: f64,
}

impl RelationshipGraph {
    pub fn generate(agents: &[Agent], seed: u64, cfg: &GraphConfig) -> Result<Self, GraphError> {
        if cfg.min_household_size < 2 || cfg.max_household_size < cfg.min_household_size {
            return Err(GraphError::InvalidHouseholdSize);
        }
        if agents.len() < 2 {
            return Ok(Self::default());
        }
        let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0x006a_7261_7068);
        let mut edges = Vec::new();
        let mut seen = BTreeSet::new();
        let mut groups: Vec<&[Agent]> = Vec::new();
        let mut cursor = 0;
        while cursor < agents.len() {
            let size = rng
                .random_range(cfg.min_household_size..=cfg.max_household_size)
                .min(agents.len() - cursor);
            groups.push(&agents[cursor..cursor + size]);
            cursor += size;
        }
        for (household, group) in groups.iter().enumerate() {
            for pair in group.windows(2) {
                add_edge(
                    &mut edges,
                    &mut seen,
                    pair[0].id,
                    pair[1].id,
                    0.8,
                    household as u32,
                );
            }
            for i in 0..group.len() {
                for j in i + 2..group.len() {
                    if rng.random_bool(cfg.strong_edge_probability) {
                        add_edge(
                            &mut edges,
                            &mut seen,
                            group[i].id,
                            group[j].id,
                            rng.random_range(0.55..0.96),
                            household as u32,
                        );
                    }
                }
            }
        }
        for group_index in 1..groups.len() {
            add_edge(
                &mut edges,
                &mut seen,
                groups[group_index - 1][rng.random_range(0..groups[group_index - 1].len())].id,
                groups[group_index][rng.random_range(0..groups[group_index].len())].id,
                rng.random_range(0.08..0.32),
                u32::MAX,
            );
        }
        for i in 0..agents.len() {
            for j in i + 1..agents.len() {
                if rng.random_bool(cfg.cross_group_probability) {
                    add_edge(
                        &mut edges,
                        &mut seen,
                        agents[i].id,
                        agents[j].id,
                        rng.random_range(0.02..0.22),
                        u32::MAX,
                    );
                }
            }
        }
        let graph = Self { edges };
        graph.validate(agents, cfg.require_connected)?;
        Ok(graph)
    }

    pub fn validate(&self, agents: &[Agent], require_connected: bool) -> Result<(), GraphError> {
        if self.edges.iter().any(|edge| edge.a == edge.b) {
            return Err(GraphError::SelfLoop);
        }
        if require_connected && agents.len() > 1 && self.metrics(agents).connected_components != 1 {
            return Err(GraphError::Disconnected);
        }
        Ok(())
    }

    pub fn neighbors(&self, agent: Uuid) -> Vec<Uuid> {
        self.edges
            .iter()
            .filter_map(|edge| {
                if edge.a == agent {
                    Some(edge.b)
                } else if edge.b == agent {
                    Some(edge.a)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn metrics(&self, agents: &[Agent]) -> GraphMetrics {
        let mut adjacency: BTreeMap<Uuid, Vec<Uuid>> =
            agents.iter().map(|agent| (agent.id, Vec::new())).collect();
        for edge in &self.edges {
            adjacency.entry(edge.a).or_default().push(edge.b);
            adjacency.entry(edge.b).or_default().push(edge.a);
        }
        let mut unseen: BTreeSet<Uuid> = adjacency.keys().copied().collect();
        let mut components = 0;
        while let Some(start) = unseen.pop_first() {
            components += 1;
            let mut queue = VecDeque::from([start]);
            while let Some(node) = queue.pop_front() {
                for neighbor in adjacency.get(&node).into_iter().flatten() {
                    if unseen.remove(neighbor) {
                        queue.push_back(*neighbor);
                    }
                }
            }
        }
        let n = agents.len();
        let possible = n.saturating_mul(n.saturating_sub(1)) / 2;
        GraphMetrics {
            node_count: n,
            edge_count: self.edges.len(),
            connected_components: components,
            mean_degree: if n == 0 {
                0.0
            } else {
                2.0 * self.edges.len() as f64 / n as f64
            },
            density: if possible == 0 {
                0.0
            } else {
                self.edges.len() as f64 / possible as f64
            },
        }
    }
}

fn add_edge(
    edges: &mut Vec<RelationshipEdge>,
    seen: &mut BTreeSet<(Uuid, Uuid)>,
    a: Uuid,
    b: Uuid,
    strength: f64,
    household: u32,
) {
    if a == b {
        return;
    }
    let pair = if a < b { (a, b) } else { (b, a) };
    if seen.insert(pair) {
        edges.push(RelationshipEdge {
            a: pair.0,
            b: pair.1,
            strength,
            household,
            protocol_affinity: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use proptest::prelude::*;

    use super::*;
    use crate::{PersonaKind, Planner, default_personas, deterministic_uuid};

    proptest! {
        #[test]
        fn generated_graph_has_no_self_loops(count in 2usize..120, seed in any::<u64>()) {
            let planner = Planner::new(default_personas()).unwrap();
            let fleet = deterministic_uuid(seed, 0, b"fleet");
            let mix = [(PersonaKind::CasualHolder, 1.0)].into_iter().collect();
            let agents = planner.create_fleet(fleet, count, seed, &mix, Utc::now());
            let graph = RelationshipGraph::generate(&agents, seed, &GraphConfig::default()).unwrap();
            prop_assert!(graph.edges.iter().all(|edge| edge.a != edge.b));
            prop_assert_eq!(graph.metrics(&agents).connected_components, 1);
        }
    }
}
