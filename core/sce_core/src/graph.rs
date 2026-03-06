use crate::{paths, policy_registry};
use anyhow::{anyhow, ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageGraph {
    pub schema_version: String,
    pub nodes: Vec<String>,
    pub edges: Vec<LineageEdge>,
}

pub fn link(project_root: &Path, from: &str, to: &str) -> anyhow::Result<LineageGraph> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    ensure!(!from.trim().is_empty(), "graph from must be non-empty");
    ensure!(!to.trim().is_empty(), "graph to must be non-empty");

    if from == to {
        return Err(anyhow!("self-link rejected: {from} -> {to}"));
    }

    let mut graph = load_graph(&canonical_root)?;
    let edge = LineageEdge {
        from: from.to_string(),
        to: to.to_string(),
    };
    if graph.edges.contains(&edge) {
        return Ok(graph);
    }

    if let Some(path) = path_between(&graph.edges, to, from) {
        let cycle = std::iter::once(from.to_string())
            .chain(path)
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(anyhow!("cycle rejected: {cycle}"));
    }

    graph.edges.push(edge);
    normalize_graph(&mut graph);
    policy_registry::write_atomic_json(&paths::graph_lineage_path(&canonical_root), &graph)?;
    Ok(graph)
}

pub fn export_dot(project_root: &Path) -> anyhow::Result<String> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let graph = load_graph(&canonical_root)?;

    let mut dot = String::from("digraph StemGraph {\n");
    for node in &graph.nodes {
        dot.push_str(&format!("  \"{}\";\n", escape_dot(node)));
    }
    for edge in &graph.edges {
        dot.push_str(&format!(
            "  \"{}\" -> \"{}\";\n",
            escape_dot(&edge.from),
            escape_dot(&edge.to)
        ));
    }
    dot.push_str("}\n");
    Ok(dot)
}

pub fn write_operator_output(project_root: &Path) -> anyhow::Result<(LineageGraph, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let graph = load_graph(&canonical_root)?;
    let output_path = paths::stemgraph_operator_output_path(&canonical_root);
    policy_registry::write_atomic_json(&output_path, &graph)?;
    Ok((graph, output_path))
}

fn load_graph(project_root: &Path) -> anyhow::Result<LineageGraph> {
    let graph_path = paths::graph_lineage_path(project_root);
    if !graph_path.exists() {
        return Ok(LineageGraph {
            schema_version: SCHEMA_VERSION.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        });
    }

    let mut graph: LineageGraph = serde_json::from_str(
        &fs::read_to_string(&graph_path)
            .with_context(|| format!("read graph lineage: {}", graph_path.display()))?,
    )
    .with_context(|| format!("parse graph lineage: {}", graph_path.display()))?;
    graph.schema_version = SCHEMA_VERSION.to_string();
    normalize_graph(&mut graph);
    Ok(graph)
}

fn normalize_graph(graph: &mut LineageGraph) {
    let mut node_set = graph.nodes.iter().cloned().collect::<BTreeSet<_>>();
    for edge in &graph.edges {
        node_set.insert(edge.from.clone());
        node_set.insert(edge.to.clone());
    }

    graph.nodes = node_set.into_iter().collect();
    graph.edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
    });
    graph.edges.dedup();
}

fn path_between(edges: &[LineageEdge], start: &str, goal: &str) -> Option<Vec<String>> {
    let mut adjacency = BTreeMap::<String, Vec<String>>::new();
    for edge in edges {
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
    }

    let mut queue = VecDeque::<Vec<String>>::new();
    queue.push_back(vec![start.to_string()]);
    let mut seen = BTreeSet::<String>::new();
    seen.insert(start.to_string());

    while let Some(path) = queue.pop_front() {
        let current = path.last().expect("path has current");
        if current == goal {
            return Some(path);
        }

        for next in adjacency.get(current).into_iter().flatten() {
            if seen.insert(next.clone()) {
                let mut next_path = path.clone();
                next_path.push(next.clone());
                queue.push_back(next_path);
            }
        }
    }

    None
}

fn escape_dot(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::{export_dot, link};

    #[test]
    fn duplicate_link_is_noop_and_dot_is_deterministic() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        let first = link(&project_root, "stem-a", "mix-a").expect("first link");
        let second = link(&project_root, "stem-a", "mix-a").expect("duplicate link");

        assert_eq!(first, second);
        let dot = export_dot(&project_root).expect("dot");
        assert_eq!(
            dot,
            "digraph StemGraph {\n  \"mix-a\";\n  \"stem-a\";\n  \"stem-a\" -> \"mix-a\";\n}\n"
        );
    }

    #[test]
    fn self_link_is_rejected() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let err = link(&project_root, "stem-a", "stem-a").expect_err("self link");
        assert!(err.to_string().contains("self-link rejected"));
    }

    #[test]
    fn cycle_rejection_includes_cycle_path() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        link(&project_root, "a", "b").expect("a->b");
        link(&project_root, "b", "c").expect("b->c");
        let err = link(&project_root, "c", "a").expect_err("cycle");

        assert!(err.to_string().contains("cycle rejected: c -> a -> b -> c"));
    }
}
