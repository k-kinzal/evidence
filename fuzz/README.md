# fuzz

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the parsers that read
user-written input. This is a separate Cargo workspace: it is excluded from the main one,
because fuzzing needs a nightly toolchain and libFuzzer.

## Targets

| Target | Checks |
|--------|--------|
| `evid_parse` | `evidence_core::format::parse` never panics on arbitrary text. When a file parses, rewriting its `[state]` keeps it parseable |
| `selectors` | `span_select` never panics on any selector or document, including `find_unique`. A resolved span stays within the text and on character boundaries |

## Running

The nightly toolchain must be at least the MSRV (1.92):

```sh
cargo +nightly fuzz run evid_parse   -- -max_total_time=60
cargo +nightly fuzz run selectors    -- -max_total_time=60
```

On macOS, add `-s none` (`cargo +nightly fuzz run -s none …`): AddressSanitizer deadlocks
at startup there. Every crate forbids `unsafe`, so little is lost.

Seed inputs can go into `corpus/<target>/`; the evidence files in
`crates/evidence-core/tests/fixtures/` are good seeds. `corpus/`, `artifacts/`, and
`target/` are git-ignored. A crash is saved under `artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> <file>`.
