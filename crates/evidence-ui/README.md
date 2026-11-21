# evidence-ui

`evidence-ui` is a desktop knowledge browser for Evidence projects, built with GPUI. It works
like a wiki: open a project and see what is known, where it came from, and how fresh it is.

**Role:** it is a read-only viewer over `evidence-core`. It reads the same files as the
CLI, does not need the SQLite index, and never changes anything. Changes go through the CLI
or an editor; the window reloads by itself.

## Usage

```sh
evidence-ui [PROJECT_DIR] [ID_OR_PATH]
```

Without arguments, it opens the project containing the current directory and shows the
overview.

### Sidebar

- A filter box, which accepts IME input.
- Filters: all, needs attention, one content mode (editable or produced), runnable (has a
  production), with claims, or with diagnostics.
- Every pair, with a health dot.

### Overview

- Counts per health.
- Problems in evidence files.
- "Needs attention", showing each pair's most severe reason.

### Pair page

The header shows the content mode, health, reasons, and notices. The page has four tabs:

| Tab | Shows |
|-----|-------|
| **Content** | The subject with line numbers, per-line coverage colors and claim labels, and a coverage bar, for any pair. When a produced subject was modified outside a run, the working file is shown without the adopted version's coverage overlay |
| **Evidence** | Productions and their last runs; claims with their status, reasons, notices, and re-anchoring hints; references fixed to a revision, with a diff from that revision to the current version; the evidence file itself |
| **Graph** | Upstream, downstream, and transitive impact. Click to navigate; ⌘[ goes back |
| **History** | Runs, pins, and migrations. Click one to diff its snapshot against the previous one |

### Shortcuts and appearance

- ⌘R reloads.
- ⌘F focuses the filter.
- ⌘[ goes back.
- ⌘Q quits.
- The window follows the system light or dark appearance.
- It reloads automatically when files change, checking a cheap mtime fingerprint every 2 s.

## Structure

| Part | Responsibility |
|------|----------------|
| `src/lib.rs` | The browsing layer, with no GUI dependency, on top of one `evidence_core::Project`: `Browser` (rows, filters, counts, problems), `Detail` (content with per-line coverage, history, neighbors, impact), snapshot diffs. All tests live here |
| `src/main.rs` | GPUI rendering only (feature `gui`) |
| `src/input.rs` | A single-line text input with IME support, adapted from the GPUI example (Apache-2.0) |
| `src/theme.rs` | Light and dark palettes; colors for health and coverage |

## Features

| Feature | Default | Effect |
|---------|---------|--------|
| `gui` | on | The GPUI front end and the `evidence-ui` binary. Without it, only the tested browsing layer is built |

GPUI needs platform libraries on Linux. CI builds the GUI on macOS and checks the browsing
layer everywhere.

License: MIT OR Apache-2.0.
