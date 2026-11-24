# `cad-agent` — L4 Multi-Agent CAD Orchestration Service

[Documentation / 文档](../../docs/en/agent-and-mcp-guide.md) | [中文指南](../../docs/zh/agent-and-mcp-guide.md)

Independent Python 3.12+ service implementing the **V2 Semantic CAD Runtime** orchestration and **Best-of-N / GRPO** candidate branch exploration for AI-Native CAD:

1. **`LLMSemanticPlanner` + `stdlib_digest`**: Translates a `DesignContract` and Point-and-Prompt viewport selections into validated `SemanticCommand` batches with on-demand fastener, bearing, ISO 286, and `schemas/materials.json` digests.
2. **`SemanticAgent` + `RuntimeToolClient`**: Drives `cad-cli session / api / query / verify` on isolated SQLite preview branches (`agent/...`), enforces local edit boundaries (`editable_node_ids` & `allowed_parameters`), supports `--context-asset` external mating via `v2_context_adapter.py`, and records zero-trust L0–L4 tri-state verdicts (`PASS` / `FAIL` / `UNKNOWN`).
3. **`BranchPatchExplorer` + `grpo_dataset_pipeline`**: Runs Best-of-N candidate rollouts on lightweight SQLite branches (`ai/cand-1..N`), scores every candidate against real L0–L4 verification reports, activates the best passing branch (or rolls back cleanly to `main`), and exports group-relative JSONL datasets.
4. **`SpecParserAgent` & `VisualGeometricCritic`**: Structured contract parsing with bounded repair and deterministic L0–L3 geometry gates plus advisory offscreen VLM semantic masks.

> Hardcoded offline geometry templates and blind regex KCL rewrite fallbacks have been retired. All LLM generation and editing paths require a real LLM provider (`OPENAI_API_KEY` or `GEMINI_API_KEY`), or `--provider deterministic` for offline smoke runs.

## Zero-Dependency Core

All production modules (`semantic_agent.py`, `llm_semantic_planner.py`, `branch_explorer.py`, `grpo_dataset_pipeline.py`, `runtime_client.py`, `llm.py`) use the **Python 3.12+ standard library only**.

## Quick Start

Build `cad-cli` from the repository root, then install and run `cad-agent`:

```sh
cargo build -p cad-cli --locked
python -m pip install -e services/cad-agent

# Run V2 semantic planning against an .aicad project (requires OPENAI_API_KEY or GEMINI_API_KEY)
python -m cad_agent.cli \
  --semantic-v2 \
  --project part.aicad \
  --workspace-root . \
  --prompt "80 x 60 x 10 mm aluminum mounting plate with 4 M6 counterbored corner holes" \
  --json
```

If `cad-cli` is located elsewhere (for example an OCCT-enabled build), point `CAD_CLI_BIN` to that executable.

## Module Layout

- `src/cad_agent/llm_semantic_planner.py` & `stdlib_digest.py` — V2 `LLMSemanticPlanner` and on-demand standard-parts/materials digest injection.
- `src/cad_agent/semantic_agent.py` & `runtime_client.py` — V2 `SemanticAgent` 0→1 creation, 1→N `--target` local edit, preview branch isolation, and `cad-cli` runtime bridge.
- `src/cad_agent/v2_context_adapter.py` — External reference asset (`--context-asset`) mating extraction with untrusted label isolation and interference verification.
- `src/cad_agent/branch_explorer.py` — Best-of-N multi-branch rollout explorer (`explore` and `explore_semantic_candidates`).
- `src/cad_agent/grpo_dataset_pipeline.py` — Zero-trust L0–L4 reward mapping, group advantage normalization, and atomic JSONL export.
- `src/cad_agent/contracts.py` & `critic.py` — `DesignContract`, tri-state `VerificationReport`, and L0–L4 verification contracts.
- `src/cad_agent/cli.py` — `python -m cad_agent.cli` JSON & `--progress-stderr` CLI entry point.
