//! A read-only property graph over the loaded evidence definitions.

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;

use anyhow::{Context, Result};
use evidence_core::{Project, format::Reference};
use frogql::Value as Gql;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::{PathValue, format_date, format_datetime};
use frogql::runtime::{engine::Runtime, result::QueryResult};
use frogql::syntax::{expr::Expr, query::ReturnItem};
use serde::Serialize;
use serde_json::{Value, json};

use crate::output::Out;
use crate::warn_line;

#[derive(Debug, Serialize)]
struct Node {
    id: String,
    labels: [&'static str; 1],
    props: Value,
}

#[derive(Debug, Serialize)]
struct Edge {
    id: String,
    labels: [&'static str; 1],
    endpoints: [String; 2],
    directionality: &'static str,
    props: Value,
}

#[derive(Debug, Default, Serialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    #[serde(skip)]
    by_id: HashMap<String, usize>,
}

impl Graph {
    fn node(&mut self, id: &str, label: &'static str, props: Value) {
        self.by_id.insert(id.to_owned(), self.nodes.len());
        self.nodes.push(Node {
            id: id.to_owned(),
            labels: [label],
            props,
        });
    }

    fn edge(&mut self, from: &str, to: &str, label: &'static str, props: Value) {
        self.edges.push(Edge {
            id: format!("e{}", self.edges.len()),
            labels: [label],
            endpoints: [from.to_owned(), to.to_owned()],
            directionality: "->",
            props,
        });
    }

    fn reference(&mut self, from: &str, reference: &Reference, label: &'static str) -> Result<()> {
        let target = reference.id();
        if !self.by_id.contains_key(target) {
            self.node(target, "Unresolved", json!({ "id": target }));
        }
        let props = match reference {
            Reference::Id(_) => json!({ "ref": target }),
            Reference::Detailed(detail) => serde_json::to_value(detail)?,
        };
        self.edge(from, target, label, props);
        Ok(())
    }

    fn load(project: &Project) -> Result<Self> {
        let mut graph = Self::default();
        for pair in project.pairs() {
            let mut props = serde_json::to_value(&pair.file)?;
            if pair.file.subject.is_none() {
                props["subject"] = json!(pair.subject_rel);
            }
            props["resolved_subject"] = json!(pair.subject_rel);
            props["evid"] = json!(pair.evid_rel);
            props["content"] = json!(pair.file.content_mode().as_str());
            graph.node(&pair.id, "Evidence", props);
        }
        for pair in project.pairs() {
            for production in &pair.file.productions {
                let id = format!("{}#production:{}", pair.id, production.name);
                graph.node(&id, "Production", serde_json::to_value(production)?);
                graph.edge(&pair.id, &id, "HAS_PRODUCTION", json!({}));
                for input in &production.inputs {
                    graph.reference(&id, input, "INPUT")?;
                }
            }
            for claim in &pair.file.claims {
                let id = format!("{}#claim:{}", pair.id, claim.id);
                graph.node(&id, "Claim", serde_json::to_value(claim)?);
                graph.edge(&pair.id, &id, "HAS_CLAIM", json!({}));
                for reference in &claim.evidence {
                    graph.reference(&id, reference, "CITES")?;
                }
            }
            for requirement in &pair.file.requirements {
                let id = format!("{}#requirement:{}", pair.id, requirement.id);
                graph.node(&id, "Requirement", serde_json::to_value(requirement)?);
                graph.edge(&pair.id, &id, "HAS_REQUIREMENT", json!({}));
            }
            if let Some(member) = &pair.membership {
                graph.edge(
                    &pair.id,
                    &member.parent,
                    "MEMBER_OF",
                    json!({ "key": member.key }),
                );
            }
        }
        Ok(graph)
    }

    fn full(&self) -> Selection {
        Selection {
            nodes: (0..self.nodes.len()).collect(),
            edges: (0..self.edges.len()).collect(),
        }
    }

    fn query(&self, query: &str) -> Result<(Selection, Option<Table>)> {
        let compiled = frogql::compile_query_with_diagnostics(query)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        for warning in &compiled.warnings {
            warn_line!("warning: {warning}");
        }
        let query = compiled.query;
        let mut selected = Selection::default();
        let mut table = query.returns.as_ref().map(|items| Table {
            columns: items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    if let Some(alias) = item.alias() {
                        alias.to_owned()
                    } else if let ReturnItem::Expr {
                        expr: Expr::Var(name),
                        ..
                    } = item
                    {
                        name.clone()
                    } else {
                        format!("col{index}")
                    }
                })
                .collect(),
            rows: Vec::new(),
        });
        if !compiled.guaranteed_empty {
            let store = MemoryGraphStore::from_json_value(&serde_json::to_value(self)?)
                .context("building the evidence graph")?;
            let runtime = Runtime::new(&store);
            match runtime.run_query(&query, 0) {
                QueryResult::Raw(matches) => {
                    for path in matches.rows.into_iter().flat_map(|row| row.paths) {
                        for element in path.0 {
                            match element {
                                PathValue::Node(id) => {
                                    selected.nodes.insert(id as usize);
                                }
                                PathValue::EdgeDirectional(id)
                                | PathValue::EdgeUndirectional(id) => {
                                    self.select_edge(&mut selected, id as usize);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                QueryResult::Projected(rows) => {
                    if let Some(table) = &mut table {
                        table.rows = rows
                            .iter()
                            .map(|row| {
                                row.iter()
                                    .map(|value| self.value(value, &mut selected))
                                    .collect()
                            })
                            .collect();
                    }
                }
            }
        }
        Ok((selected, table))
    }

    fn select_edge(&self, selected: &mut Selection, index: usize) {
        if let Some(edge) = self.edges.get(index) {
            selected.edges.insert(index);
            selected
                .nodes
                .extend(edge.endpoints.iter().map(|id| self.by_id[id]));
        }
    }

    fn value(&self, value: &Gql, selected: &mut Selection) -> Value {
        match value {
            Gql::Node(id) => self.nodes.get(*id as usize).map_or(Value::Null, |node| {
                selected.nodes.insert(*id as usize);
                json!({ "node": node.id })
            }),
            Gql::Edge(id) => self.edges.get(*id as usize).map_or(Value::Null, |edge| {
                self.select_edge(selected, *id as usize);
                json!({ "edge": edge.id })
            }),
            Gql::List(values) | Gql::Path(values) => {
                Value::Array(values.iter().map(|v| self.value(v, selected)).collect())
            }
            Gql::Record(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.value(v, selected)))
                    .collect(),
            ),
            Gql::Null => Value::Null,
            Gql::Bool(value) => json!(value),
            Gql::Int(value) => json!(value),
            Gql::Float(value) => json!(value),
            Gql::Str(value) => json!(value),
            Gql::Date(value) => json!(format_date(*value)),
            Gql::LocalDatetime(value) => json!(format_datetime(*value)),
        }
    }
}

#[derive(Debug, Default)]
struct Selection {
    nodes: BTreeSet<usize>,
    edges: BTreeSet<usize>,
}

#[derive(Debug, Serialize)]
struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

pub(crate) fn run(project: &Project, out: Out, query: Option<&str>) -> Result<()> {
    let graph = Graph::load(project)?;
    let (selected, table) = match query {
        Some(query) => graph.query(query)?,
        None => (graph.full(), None),
    };
    let nodes: Vec<_> = selected.nodes.iter().map(|&i| &graph.nodes[i]).collect();
    let edges: Vec<_> = selected.edges.iter().map(|&i| &graph.edges[i]).collect();
    if out.json {
        return out.emit(
            "graph",
            &json!({ "nodes": nodes, "edges": edges, "result": table }),
        );
    }
    let mut stdout = std::io::stdout().lock();
    if table.is_none() || !nodes.is_empty() {
        writeln!(stdout, "digraph evidence {{")?;
        for node in nodes {
            writeln!(
                stdout,
                "  {} [label={}];",
                json!(node.id),
                json!(format!("{}: {}", node.labels[0], node.id))
            )?;
        }
        for edge in edges {
            writeln!(
                stdout,
                "  {} -> {} [label={}];",
                json!(edge.endpoints[0]),
                json!(edge.endpoints[1]),
                json!(edge.labels[0])
            )?;
        }
        writeln!(stdout, "}}")?;
    } else if let Some(table) = table {
        writeln!(stdout, "{}", table.columns.join("\t"))?;
        for row in table.rows {
            let cells: Vec<_> = row.iter().map(Value::to_string).collect();
            writeln!(stdout, "{}", cells.join("\t"))?;
        }
    }
    Ok(())
}
