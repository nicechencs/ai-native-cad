# Developer Platform Examples (`aicad` Python SDK + `cad-cli`)

End-to-end runnable examples demonstrating how to drive the AI-Native CAD V2
semantic runtime (`GraphEvaluator` / `.aicad` projects) from Python using the
official `aicad` SDK and a real `cad-cli` binary.

## Prerequisites

1. Build `cad-cli`:
   ```bash
   cargo build --locked -p cad-cli
   ```
2. Python 3.12+ (each script automatically adds `packages/python-sdk/src` to
   `sys.path` when executed from a repository checkout, or you can install the
   package with `pip install -e packages/python-sdk`).

## Locating `cad-cli`

All scripts resolve `cad-cli` in the following order via `aicad.find_cad_cli()`:

1. Explicit `--cad-cli <path>` flag
2. `CAD_CLI_BIN` environment variable
3. Repository checkout `target/debug/cad-cli(.exe)` or `target/release/cad-cli(.exe)`
4. `cad-cli` on `PATH`

## Examples

### 1. Inspect & Measure (`01_inspect_and_measure.py`)

Creates a parametric mounting plate with a center through-hole, queries session
status, feature outline, and node lineage (`explain`), and runs `cad-cli verify`
to measure total solid volume and verification layer statuses.

```bash
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
```

### 2. Parametric Commit & Verify (`02_parametric_commit_and_verify.py`)

Commits a baseline parametric spacer block, then applies a parameter update
guarded by optimistic revision locking (`expected_revision`) and verifies that
the revision and solid volume advance deterministically.

```bash
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
```

### 3. Branch Preview & Promote (`03_branch_preview_and_promote.py`)

Demonstrates safe candidate branch isolation: creates a baseline on `main`,
opens an `ai_candidate` preview branch, adds a mounting through-hole, inspects
the branch diff and review payload, verifies geometry, and promotes the branch
onto `main` with `commit_preview`.

```bash
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```

## Quick Verification

Run all three examples sequentially against the built `cad-cli` binary:

```bash
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```
