# assess

`assess` evaluates how well the citations of an Evidence claim support its selected
text. Install it from the workspace with `cargo install --path crates/assess`.

```sh
assess citation-correctness --provider ollama --model nimble report 'notes#release-date'
assess citation-correctness --provider jev --jobs 8 --json reports/
evidence show report
```

Targets can be Evidence IDs, paths, or directories. Append `#claim` to select one
claim. The command uses the current subject for editable evidence, the adopted
version for produced evidence, and the named version for fixed references. It
resolves each selector against the captured snapshot. Only text is assessed.

`assess` groups claims with identical citation text, in the same order, into one
request with a named question per claim. The input contains separate `claims`
and `citations` fields. Each question names its claim by index.
`--questions-per-request` limits how many questions share a request (default 16).
`--jobs` bounds concurrent independent requests (default 4). `--timeout` sets
each request's deadline in seconds (default 120). For each claim the evaluator
judges the selected citations together and
matches the subject, behavior, and conditions described by the claim.
`supported` means the citations establish every material part. `partial` means
they establish some parts and leave the rest open. `insufficient` means they
establish none of the material parts. An excerpt that shows the opposite of a
material part for the same subject and conditions takes precedence and yields
`contradicted`.

The terminal output includes the chosen label and `P(supported)`. Each successful
record includes all four probabilities and the provider's `confidence`, when
available. The meaning of `confidence` depends on the provider. Read
`probabilities.partial` as the probability of the `partial` class. To estimate how
many parts of a compound claim have support, select and assess those parts
individually.

An API refusal, failed request, or missing or malformed probability distribution
exits with code 1 after saving the outcome and any provider response. Input,
configuration, or storage errors exit with 2.

## Decision API adapters

Use `--provider`, `--model`, and `--endpoint` to select the provider, model, and full
API URL. `ASSESS_PROVIDER`, `ASSESS_MODEL`, and `ASSESS_ENDPOINT` supply their
defaults. The command defaults to `jev`. Secrets come from environment variables.
Use `--api-key-env` to read a different variable or authenticate a local endpoint.

| Provider | Model | Endpoint | Credential environment variable |
|---|---|---|---|
| `jev` | `jev-1.14.0` | `https://api.typesafe.ai/v1/systemone` | `TYPESAFE_API_KEY` |
| `openai` | `gpt-6-luna` | `https://api.openai.com/v1/decisions` | `OPENAI_API_KEY` |
| `gemini` | `gemini-decision-1` | `https://generativelanguage.googleapis.com/v1beta/decisions` | `GEMINI_API_KEY` |
| `foundry` | Deployment name | Required, ending in `/providers/microsoft/v1/systemone` | `AZURE_API_KEY` |
| `azure-openai` | Deployment name | Required, ending in `/openai/v1/decisions` | `AZURE_API_KEY` |
| `cloudflare` | Taken from the URL | Required, e.g. `https://api.cloudflare.com/client/v4/accounts/ACCOUNT/ai/run/@cf/cloudflare/clef` | `CLOUDFLARE_API_TOKEN` |
| `ollama` | `nimble` | `http://localhost:11434/v1/systemone` | Optional |
| `vllm` | Required | `http://localhost:8000/v1/systemone` | Optional |
| `llama-cpp` | Required | `http://localhost:8080/v1/systemone` | Optional |
| `system-one` | Required | Required | Optional |

Local servers need a model supported by their decision endpoint. API credentials
are passed in HTTP headers and are kept out of the run archive.

## Records and replay

The assessment sidecar replaces the final `.evid` suffix with `.asse`. For example,
`report.md.evid` has `report.md.asse`. A sidecar contains the Evidence ID, each run
ID, claim IDs, selected versions and ranges, and results. `evidence show --json`
exposes this data under `pair.assessments`. A run spanning several Evidence IDs
uses the same run ID in every sidecar.

The object store holds the complete run, including the original request and
response bodies, endpoint, specified and returned model identifiers, and any
request IDs returned by the provider. The archive uses the project's encryption
setting. Keep the local object store and its key with the `.asse` files to replay
those runs. The `.asse` files themselves contain the result values and their
references.

```sh
evidence cat sha256:RUN_DIGEST
assess replay report sha256:RUN_DIGEST
```

Replay resends every original request in the run with its original grouping and
model parameter, then appends the new results to the sidecars. For a custom
endpoint, pass the original URL again with `--endpoint`. Replaying a hosted model
requires that the provider still accepts the recorded model and request. Providers
can return different probabilities for the same request. Run
`citation-correctness` again to assess any edits to the selections or their sources.
