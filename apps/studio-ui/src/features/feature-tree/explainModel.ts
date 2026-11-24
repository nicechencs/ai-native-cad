/**
 * Explain a selected feature or dimension.
 * The chain is the parameter formula graph, the upstream nodes, the attached contract clauses,
 * and the TopoRef lineage recorded on that feature.
 */
import type { DesignContract } from "../../types/scene.ts";
import type { CanonicalGraph, IrNode, IrValue, ParameterDef } from "../../types/graphIr.ts";
import { failedTopoRefsOf, readRecoveryHint } from "./nodeFailure.ts";

export interface FormulaLink {
  readonly name: string;
  readonly formula: string | null;
  readonly valueText: string;
  readonly dependsOn: readonly string[];
}

export interface UpstreamNode {
  readonly id: string;
  readonly name: string;
}

export interface ContractClause {
  readonly clauseId: string;
  readonly kind: "summary" | "interface" | "load" | "keep_out";
  readonly text: string;
}

export interface TopoLineageEntry {
  readonly topoRef: string;
  readonly role: "rollback" | "input" | "metadata";
  readonly detail: string;
}

export interface ExplainModel {
  readonly nodeId: string | null;
  readonly nodeName: string | null;
  readonly parameterName: string | null;
  readonly formulaChain: readonly FormulaLink[];
  readonly upstreamNodes: readonly UpstreamNode[];
  readonly contractClauses: readonly ContractClause[];
  readonly topoLineage: readonly TopoLineageEntry[];
  readonly lineageSummary: string;
}

export function explainModel(input: {
  readonly graph: CanonicalGraph;
  readonly nodeId?: string | null;
  readonly parameterName?: string | null;
  readonly contract?: DesignContract | null;
}): ExplainModel {
  const nodeId = input.nodeId ?? null;
  const node = nodeId ? input.graph.nodes[nodeId] ?? null : null;
  const parameterName = input.parameterName?.trim() || null;
  const touched = new Set<string>();
  if (parameterName && input.graph.parameters[parameterName]) touched.add(parameterName);
  if (node) {
    for (const name of parametersTouchingNode(input.graph, node)) touched.add(name);
  }
  const formulaChain = formulaChainFor([...touched], input.graph.parameters);
  const upstreamNodes = node ? upstreamNodesOf(input.graph, node) : [];
  const topoLineage = node ? topoLineageOf(node) : [];
  return {
    nodeId: node ? node.id : null,
    nodeName: node ? node.name || node.id : null,
    parameterName,
    formulaChain,
    upstreamNodes,
    contractClauses: contractClausesOf(input.contract ?? null),
    topoLineage,
    lineageSummary: topoLineage.map((entry) => `${entry.role}:${entry.topoRef}`).join("; "),
  };
}

export function formatIrValueText(value: IrValue): string {
  switch (value.type) {
    case "quantity": {
      const unit = "unit" in value.value ? value.value.unit : "";
      return `${formatNumber(value.value.value)}${unit}`;
    }
    case "scalar":
    case "int":
      return formatNumber(value.value);
    case "bool":
      return value.value ? "true" : "false";
    case "string":
    case "blob_ref":
    case "external_ref":
    case "topo_ref_expr":
      return value.value;
    case "vec3":
      return value.value.join(", ");
    case "node_ref":
      return value.value.node_id;
    case "list":
      return value.value.map((item) => formatIrValueText(item)).join(", ");
    case "map":
      return Object.entries(value.value).map(([key, item]) => `${key}=${formatIrValueText(item)}`).join(", ");
    default: {
      const unreachable: never = value;
      return unreachable;
    }
  }
}

function formulaChainFor(
  roots: readonly string[],
  parameters: Readonly<Record<string, ParameterDef>>,
): FormulaLink[] {
  const ordered: FormulaLink[] = [];
  const seen = new Set<string>();
  const visiting = new Set<string>();
  for (const name of [...roots].sort(compareText)) walkFormula(name, parameters, visiting, ordered, seen);
  return ordered;
}

function walkFormula(
  name: string,
  parameters: Readonly<Record<string, ParameterDef>>,
  visiting: Set<string>,
  ordered: FormulaLink[],
  seen: Set<string>,
): void {
  if (seen.has(name) || visiting.has(name)) return;
  const parameter = parameters[name];
  if (!parameter) return;
  visiting.add(name);
  const dependsOn = formulaDependsOn(parameter.formula, parameters, name);
  for (const dependency of dependsOn) walkFormula(dependency, parameters, visiting, ordered, seen);
  visiting.delete(name);
  seen.add(name);
  ordered.push({
    name,
    formula: parameter.formula ?? null,
    valueText: formatIrValueText(parameter.value),
    dependsOn,
  });
}

function formulaDependsOn(
  formula: string | null | undefined,
  parameters: Readonly<Record<string, ParameterDef>>,
  selfName: string,
): string[] {
  if (!formula) return [];
  const found = new Set<string>();
  for (const match of formula.matchAll(/[A-Za-z_][A-Za-z0-9_]*/g)) {
    const name = match[0];
    if (!name || name === selfName || !Object.hasOwn(parameters, name)) continue;
    found.add(name);
  }
  return [...found].sort(compareText);
}

function parametersTouchingNode(graph: CanonicalGraph, node: IrNode): string[] {
  const names = new Set<string>();
  if (node.kind.kind === "parameter_ref") names.add(node.kind.param_id);
  const haystack = nodeText(node);
  for (const name of Object.keys(graph.parameters)) {
    if (mentionsIdent(haystack, name)) names.add(name);
  }
  return [...names].sort(compareText);
}

function nodeText(node: IrNode): string {
  const parts: string[] = [node.name];
  if (node.kind.kind === "parameter_ref") parts.push(node.kind.param_id);
  for (const [key, value] of Object.entries(node.inputs)) {
    parts.push(key, formatIrValueText(value));
  }
  for (const [key, value] of Object.entries(node.metadata)) parts.push(key, value);
  return parts.join(" ");
}

function mentionsIdent(haystack: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|[^A-Za-z0-9_])${escaped}(?:$|[^A-Za-z0-9_])`).test(haystack);
}

function upstreamNodesOf(graph: CanonicalGraph, node: IrNode): UpstreamNode[] {
  const ids = new Set<string>();
  for (const value of Object.values(node.inputs)) collectNodeIds(value, ids);
  ids.delete(node.id);
  return [...ids].sort(compareText).map((id) => ({
    id,
    name: graph.nodes[id]?.name || id,
  }));
}

function collectNodeIds(value: IrValue, ids: Set<string>): void {
  switch (value.type) {
    case "node_ref":
      ids.add(value.value.node_id);
      return;
    case "list":
      for (const item of value.value) collectNodeIds(item, ids);
      return;
    case "map":
      for (const item of Object.values(value.value)) collectNodeIds(item, ids);
      return;
    default:
      return;
  }
}

function topoLineageOf(node: IrNode): TopoLineageEntry[] {
  const entries: TopoLineageEntry[] = [];
  const seen = new Set<string>();
  const push = (role: TopoLineageEntry["role"], topoRef: string, detail: string) => {
    const ref = topoRef.trim();
    if (!ref) return;
    const key = `${role}:${ref}`;
    if (seen.has(key)) return;
    seen.add(key);
    entries.push({ topoRef: ref, role, detail });
  };
  const hint = readRecoveryHint(node);
  for (const topoRef of failedTopoRefsOf(node)) {
    const reason = node.state.status === "failed_with_rollback" ? node.state.reason : "";
    push("rollback", topoRef, [reason, hint].filter(Boolean).join(" · "));
  }
  for (const [key, value] of Object.entries(node.inputs)) collectTopoExprs(value, key, (topoRef, detail) => {
    push("input", topoRef, detail);
  });
  for (const [key, value] of Object.entries(node.metadata)) {
    if (key === "recovery_hint") continue;
    if (key.includes("topo") || key.includes("lineage") || /^(face|edge|vertex|cell):/.test(value)) {
      push("metadata", value, key);
    }
  }
  return entries;
}

function collectTopoExprs(
  value: IrValue,
  path: string,
  push: (topoRef: string, detail: string) => void,
): void {
  switch (value.type) {
    case "topo_ref_expr":
      push(value.value, path);
      return;
    case "list":
      value.value.forEach((item, index) => collectTopoExprs(item, `${path}[${index}]`, push));
      return;
    case "map":
      for (const [key, item] of Object.entries(value.value)) collectTopoExprs(item, `${path}.${key}`, push);
      return;
    default:
      return;
  }
}

function contractClausesOf(contract: DesignContract | null): ContractClause[] {
  if (!contract) return [];
  const clauses: ContractClause[] = [];
  if (contract.summary?.trim()) {
    clauses.push({ clauseId: "summary", kind: "summary", text: contract.summary.trim() });
  }
  for (const item of contract.interfaces) {
    clauses.push({
      clauseId: item.interface_id,
      kind: "interface",
      text: [item.kind, item.tolerance, item.roughness, item.notes].filter(Boolean).join(" · "),
    });
  }
  for (const load of contract.loads) {
    const magnitude = load.magnitude === undefined ? "" : String(load.magnitude);
    clauses.push({
      clauseId: load.load_id,
      kind: "load",
      text: [load.kind, magnitude, load.unit, load.target_interface].filter(Boolean).join(" · "),
    });
  }
  for (const zone of contract.keep_out_zones) {
    clauses.push({
      clauseId: zone.zone_id,
      kind: "keep_out",
      text: zone.description?.trim() || zone.kind,
    });
  }
  return clauses;
}

function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return "0";
  const rounded = Math.round(value * 1e6) / 1e6;
  return String(rounded);
}

function compareText(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}
