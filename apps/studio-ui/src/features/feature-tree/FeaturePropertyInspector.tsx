/**
 * Property inspector for the selected feature.
 * Rename, suppress, and parameter edits are semantic commands. The graph patch is only a fallback.
 */
import { useState } from "react";
import type { CanonicalGraph, GraphPatch, IrNode, ParameterDef } from "../../types/graphIr.ts";
import { buildUpdateParameterPatch } from "./graphPatchBuilder.ts";
import { formatIrValueText } from "./explainModel.ts";

export interface FeaturePropertyInspectorProps {
  readonly graph: CanonicalGraph;
  readonly node: IrNode | null;
  readonly onRenameNode?: (nodeId: string, name: string) => void;
  readonly onSetNodeSuppressed?: (nodeId: string, suppressed: boolean) => void;
  readonly onUpsertParameter?: (value: ParameterDef) => void;
  readonly onEmitPatch: (patch: GraphPatch) => void;
}

export function FeaturePropertyInspector({
  graph,
  node,
  onRenameNode,
  onSetNodeSuppressed,
  onUpsertParameter,
  onEmitPatch,
}: FeaturePropertyInspectorProps) {
  if (!node) return null;
  const suppressed = node.state.status === "suppressed";
  const parameters = Object.values(graph.parameters).sort((left, right) => compareText(left.name, right.name));
  return (
    <section className="panel-card feature-property-inspector" aria-label="特征属性">
      <header className="panel-card-header">
        <h3 className="panel-card-title">属性</h3>
        <span className="mini-badge">{node.id}</span>
      </header>
      <label className="ai-config-field">
        <span>名称</span>
        <NameField
          key={`${node.id}:${node.name}`}
          nodeId={node.id}
          name={node.name || node.id}
          onRename={(name) => renameNode(node.id, name, onRenameNode, onEmitPatch)}
        />
      </label>
      <button
        type="button"
        className="tool-btn is-compact"
        aria-pressed={suppressed}
        onClick={() => toggleSuppressed(node, suppressed, onSetNodeSuppressed, onEmitPatch)}
      >
        {suppressed ? "取消抑制" : "抑制"}
      </button>
      <h4 className="explain-heading">参数</h4>
      {parameters.length === 0 ? (
        <p className="workbench-panel-empty">当前特征图没有参数。</p>
      ) : (
        <ul className="explain-list" aria-label="参数编辑">
          {parameters.map((parameter) => (
            <li key={parameter.name}>
              <ParameterField
                parameter={parameter}
                onCommit={(next) => upsertParameter(next, onUpsertParameter, onEmitPatch)}
              />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function NameField({
  nodeId,
  name,
  onRename,
}: {
  readonly nodeId: string;
  readonly name: string;
  readonly onRename: (name: string) => void;
}) {
  const [draft, setDraft] = useState(name);
  return (
    <input
      className="text-input"
      aria-label={`重命名 ${nodeId}`}
      value={draft}
      onFocus={() => setDraft(name)}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={() => {
        const next = draft.trim();
        if (next && next !== name) onRename(next);
        else setDraft(name);
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          event.currentTarget.blur();
        }
      }}
    />
  );
}

function ParameterField({
  parameter,
  onCommit,
}: {
  readonly parameter: ParameterDef;
  readonly onCommit: (parameter: ParameterDef) => void;
}) {
  const numeric = parameterNumber(parameter);
  const [draft, setDraft] = useState(numeric === null ? formatIrValueText(parameter.value) : String(numeric));
  const unit = parameterUnit(parameter);
  return (
    <label className="ai-config-field">
      <span>
        {parameter.name}
        {parameter.formula ? <code> {parameter.formula}</code> : null}
      </span>
      <input
        className="text-input"
        aria-label={`参数 ${parameter.name}`}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          try {
            const next = nextParameter(parameter, draft);
            if (next) onCommit(next);
          } catch {
            const current = parameterNumber(parameter);
            setDraft(current === null ? formatIrValueText(parameter.value) : String(current));
          }
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            event.currentTarget.blur();
          }
        }}
      />
      {unit ? <span className="mini-badge">{unit}</span> : null}
    </label>
  );
}

function renameNode(
  nodeId: string,
  name: string,
  onRenameNode: FeaturePropertyInspectorProps["onRenameNode"],
  onEmitPatch: (patch: GraphPatch) => void,
): void {
  if (onRenameNode) {
    onRenameNode(nodeId, name);
    return;
  }
  onEmitPatch({
    patch_id: `rename-node:${nodeId}`,
    description: `Rename ${nodeId}`,
    ops: [{ op: "update_node", value: { id: nodeId, name, inputs: null, kind: null } }],
  });
}

function toggleSuppressed(
  node: IrNode,
  suppressed: boolean,
  onSetNodeSuppressed: FeaturePropertyInspectorProps["onSetNodeSuppressed"],
  onEmitPatch: (patch: GraphPatch) => void,
): void {
  if (onSetNodeSuppressed) {
    onSetNodeSuppressed(node.id, !suppressed);
    return;
  }
  onEmitPatch({
    patch_id: `toggle-suppress:${node.id}`,
    description: `${suppressed ? "Unsuppress" : "Suppress"} ${node.id}`,
    ops: [{
      op: "set_node_state",
      value: { id: node.id, state: suppressed ? { status: "stale" } : { status: "suppressed" } },
    }],
  });
}

function upsertParameter(
  parameter: ParameterDef,
  onUpsertParameter: FeaturePropertyInspectorProps["onUpsertParameter"],
  onEmitPatch: (patch: GraphPatch) => void,
): void {
  if (onUpsertParameter) {
    onUpsertParameter(parameter);
    return;
  }
  onEmitPatch({
    patch_id: `update-parameter:${parameter.name}`,
    description: `Update parameter ${parameter.name}`,
    ops: [{ op: "upsert_parameter", value: parameter }],
  });
}

function nextParameter(parameter: ParameterDef, draft: string): ParameterDef | null {
  if (parameter.value.type === "bool") {
    const normalized = draft.trim().toLowerCase();
    const value = normalized === "true" || normalized === "1";
    if (normalized !== "true" && normalized !== "false" && normalized !== "1" && normalized !== "0") return null;
    if (parameter.value.value === value) return null;
    return parameterFromPatch(parameter, value);
  }
  if (parameter.value.type === "string") {
    if (draft === parameter.value.value) return null;
    return parameterFromPatch(parameter, draft);
  }
  const numeric = Number(draft);
  if (!Number.isFinite(numeric)) return null;
  const current = parameterNumber(parameter);
  if (current === numeric) return null;
  return parameterFromPatch(parameter, numeric);
}

function parameterFromPatch(parameter: ParameterDef, value: unknown): ParameterDef | null {
  const patch = buildUpdateParameterPatch(parameter, value, `inspector:${parameter.name}`);
  const op = patch.ops[0];
  return op?.op === "upsert_parameter" ? op.value : null;
}

function parameterNumber(parameter: ParameterDef): number | null {
  const value = parameter.value;
  if (value.type === "quantity") return value.value.value;
  if (value.type === "scalar" || value.type === "int") return value.value;
  return null;
}

function parameterUnit(parameter: ParameterDef): string {
  const value = parameter.value;
  if (value.type === "quantity" && "unit" in value.value) return value.value.unit;
  return "";
}

function compareText(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}
