//! Evidence file parsing (TOML → schema → serde → semantic checks) must never panic, and
//! rewriting the state of a valid file must keep it parseable.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let parsed = evidence_core::format::parse(text, Path::new("fuzz.json.evid"));
    if let Some(file) = parsed.file {
        let state = file.state.clone().unwrap_or_default();
        if let Ok(rewritten) = evidence_core::format::replace_state(text, &state) {
            let again = evidence_core::format::parse(&rewritten, Path::new("fuzz.json.evid"));
            assert!(again.file.is_some(), "state rewrite broke a valid file:\n{rewritten}");
        }
    }
});
