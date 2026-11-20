#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use span_select::{LineIndex, Selector, find_unique};

proptest! {
    /// Any line range inside the document extracts exactly those lines.
    #[test]
    fn line_ranges_extract_exact_lines(lines in proptest::collection::vec("[a-z ]{0,10}", 1..30), a in 0usize..30, b in 0usize..30) {
        let text: String = lines.iter().flat_map(|l| [l.as_str(), "\n"]).collect();
        let n = lines.len();
        let (start, end) = (a % n + 1, b % n + 1);
        let (start, end) = (start.min(end), start.max(end));
        let sel = Selector::Lines(format!("{start}-{end}").parse().unwrap());
        let got = sel.extract(&text).unwrap();
        let want: String = lines[start - 1..end].iter().flat_map(|l| [l.as_str(), "\n"]).collect();
        prop_assert_eq!(got, want);
    }

    /// JSON Pointer spans parse back to the same value serde_json finds.
    #[test]
    fn json_pointer_matches_serde(value in arb_json()) {
        let text = serde_json::to_string_pretty(&value).unwrap();
        for (pointer, expected) in pointers(&value, "") {
            let got = Selector::JsonPointer(pointer.clone()).extract(&text).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(got).unwrap();
            prop_assert_eq!(&parsed, &expected, "pointer {}", pointer);
        }
    }

    /// Re-anchoring finds a unique snippet again after unrelated lines are inserted before it.
    #[test]
    fn reanchor_after_insertions(prefix in proptest::collection::vec("[a-m]{1,6}", 0..10)) {
        let snippet = "UNIQUE SNIPPET\nsecond line\n";
        let head: String = prefix.iter().flat_map(|l| [l.as_str(), "\n"]).collect();
        let doc = format!("{head}{snippet}tail\n");
        let span = find_unique(&doc, snippet).unwrap();
        prop_assert_eq!(span.text(&doc), snippet);
        prop_assert_eq!(*span.lines.start(), prefix.len() + 1);
    }

    #[test]
    fn line_index_is_consistent(text in "[a-z\n]{0,200}") {
        let idx = LineIndex::new(&text);
        prop_assert_eq!(idx.line_count(), text.lines().count());
        for n in 1..=idx.line_count() {
            let r = idx.line_range(n);
            prop_assert_eq!(idx.line_of(r.start), n);
        }
    }
}

fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i32>().prop_map(serde_json::Value::from),
        "[a-z/~\"\\\\é]{0,6}".prop_map(serde_json::Value::from),
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            proptest::collection::btree_map("[a-z/~]{1,4}", inner, 0..4)
                .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
        ]
    })
}

fn pointers(value: &serde_json::Value, prefix: &str) -> Vec<(String, serde_json::Value)> {
    let mut out = vec![(prefix.to_owned(), value.clone())];
    match value {
        serde_json::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                out.extend(pointers(v, &format!("{prefix}/{i}")));
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let escaped = k.replace('~', "~0").replace('/', "~1");
                out.extend(pointers(v, &format!("{prefix}/{escaped}")));
            }
        }
        _ => {}
    }
    out
}
