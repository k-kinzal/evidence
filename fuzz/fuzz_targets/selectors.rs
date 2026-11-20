//! Selector parsing and resolution must never panic, and resolved spans must lie inside the
//! document on character boundaries.
#![no_main]

use libfuzzer_sys::fuzz_target;
use span_select::{Selector, find_unique};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    // First line: the selector argument; rest: the document.
    let (arg, doc) = text.split_once('\n').unwrap_or((text, ""));
    let selectors = [
        arg.parse().map(Selector::Lines).ok(),
        Some(Selector::Heading(arg.to_owned())),
        Some(Selector::JsonPointer(arg.to_owned())),
        Some(Selector::Marker(arg.to_owned())),
        Some(Selector::Whole(true)),
    ];
    for selector in selectors.into_iter().flatten() {
        if let Ok(span) = selector.resolve(doc) {
            assert!(span.bytes.end <= doc.len());
            assert!(doc.is_char_boundary(span.bytes.start) && doc.is_char_boundary(span.bytes.end));
        }
    }
    if !arg.is_empty() {
        let _ = find_unique(doc, arg);
    }
});
