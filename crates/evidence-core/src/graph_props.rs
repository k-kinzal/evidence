//! Property tests of graph planning on random dependency graphs written as evidence files.
//! (A unit test: topological order and levels are internal to the crate.)

use std::collections::{BTreeSet, HashMap};

use crate::layout::Layout;
use crate::workspace::Workspace;
use proptest::prelude::*;

/// Node `i` may depend on any node `j < i` (so the generated graph is acyclic). Ids are
/// assigned in reverse so that alphabetical order never coincides with dependency order.
fn graph() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    (2usize..10).prop_flat_map(|n| {
        let pairs: Vec<(usize, usize)> = (0..n).flat_map(|i| (0..i).map(move |j| (i, j))).collect();
        let len = pairs.len();
        (Just(n), proptest::collection::vec(any::<bool>(), len)).prop_map(move |(n, keep)| {
            let edges = pairs
                .iter()
                .zip(keep)
                .filter(|(_, k)| *k)
                .map(|(e, _)| *e)
                .collect();
            (n, edges)
        })
    })
}

fn id(n: usize, i: usize) -> String {
    format!("p{:02}", n - 1 - i)
}

fn write_project(n: usize, edges: &[(usize, usize)]) -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().unwrap();
    let project = Layout::init(dir.path()).unwrap();
    for i in 0..n {
        let deps: Vec<String> = edges
            .iter()
            .filter(|(a, _)| *a == i)
            .map(|(_, b)| format!("\"{}\"", id(n, *b)))
            .collect();
        let text = if deps.is_empty() {
            format!(
                "version = 1\nkind = \"source\"\nid = \"{}\"\n[source]\nrun = [\"true\"]\n",
                id(n, i)
            )
        } else {
            format!(
                "version = 1\nkind = \"derived\"\nid = \"{}\"\ninputs = [{}]\n[transform]\nrun = [\"true\"]\n",
                id(n, i),
                deps.join(", ")
            )
        };
        std::fs::write(dir.path().join(format!("{}.json.evid", id(n, i))), text).unwrap();
    }
    (dir, project)
}

fn reachable(n: usize, edges: &[(usize, usize)], from: usize) -> BTreeSet<String> {
    // Edge (a, b): a depends on b, so b's change reaches a.
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(x) = stack.pop() {
        for (a, _) in edges.iter().filter(|(_, b)| *b == x) {
            if seen.insert(id(n, *a)) {
                stack.push(*a);
            }
        }
    }
    seen
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn planning_respects_dependencies((n, edges) in graph()) {
        let (_dir, project) = write_project(n, &edges);
        let ws = Workspace::load(&project).unwrap();
        prop_assert!(ws.diagnostics.is_empty(), "{:?}", ws.diagnostics);

        let order: HashMap<String, usize> = ws
            .topological_order()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), i))
            .collect();
        prop_assert_eq!(order.len(), n);
        let level: HashMap<String, usize> = ws
            .levels()
            .unwrap()
            .iter()
            .enumerate()
            .flat_map(|(l, ps)| ps.iter().map(move |p| (p.id.clone(), l)))
            .collect();
        for (a, b) in &edges {
            let (a, b) = (id(n, *a), id(n, *b));
            prop_assert!(order[&b] < order[&a], "{b} must come before {a}");
            prop_assert!(level[&b] < level[&a], "{b} must be on a lower level than {a}");
        }
        for i in 0..n {
            let got: BTreeSet<String> = ws.downstream(&id(n, i)).iter().map(|p| p.id.clone()).collect();
            prop_assert_eq!(got, reachable(n, &edges, i));
        }
    }

    #[test]
    fn back_edges_are_cycles((n, edges) in graph()) {
        // Pick a dependency path a -> ... -> b and add b -> a.
        let Some(&(a, b)) = edges.first() else { return Ok(()) };
        let mut with_cycle = edges.clone();
        with_cycle.push((b, a));
        let (_dir, project) = write_project(n, &with_cycle);
        let ws = Workspace::load(&project).unwrap();
        prop_assert!(ws.diagnostics.iter().any(|d| d.code == "cycle"), "{:?}", ws.diagnostics);
        prop_assert!(ws.topological_order().is_err());
    }
}
