/**
 * In-memory engineering session.
 * One document owns the feature graph, the KCL view, the design contract,
 * and the verification report. Undo and redo walk an oplog of those snapshots.
 * Parameter edits are semantic commands, not source-span splices.
 */
import { applyLocalGraphPatch } from "../../features/feature-tree/graphPatchBuilder.ts";
import { INITIAL_DEMO_KCL } from "../../data/demoWorkbenchState.ts";
import type { DesignContract } from "../../types/scene.ts";
import type {
  CanonicalGraph,
  GraphPatch,
  IrNode,
  IrType,
  IrValue,
  NodeState,
  ParameterDef,
  UnitNodeKind,
} from "../../types/graphIr.ts";
import {
  buildVerificationReport,
  VERIFICATION_LAYERS,
  type VerificationReport,
} from "../../types/verification.ts";
import {
  projectParameterIntoKcl as projectParameterIntoKclFallback,
  syncGraphParametersFromKcl,
} from "./kclProjectionFallback.ts";

export const MAIN_BRANCH = "main";
export const NO_DESIGN_CONTRACT_SUMMARY = "No design contract attached";
export const ATTACHED_CONTRACT_SUMMARY = "Design contract attached; verification has not run.";
export const STALE_VERIFICATION_SUMMARY = "Document changed; verification has not been rerun.";

const MAX_OPLOG_ENTRIES = 50;

const EMPTY_VERIFICATION_REPORT: VerificationReport = buildVerificationReport({
  checks: [],
  kernel_identity: { provider: "none", version: "0", authoritative: false },
  summary: NO_DESIGN_CONTRACT_SUMMARY,
});

export class SessionCommandError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "SessionCommandError";
    this.code = code;
  }
}

export interface ParameterDiff {
  readonly name: string;
  readonly before: string;
  readonly after: string;
}

export interface FeatureDiff {
  readonly nodeId: string;
  readonly change: "added" | "removed" | "modified" | "renamed" | "suppressed" | "unsuppressed";
  readonly summary: string;
}

/** Candidate branch shown in Change Preview. Confirm applies `commands`; discard drops them. */
export interface ChangePreview {
  readonly previewBranch: string;
  readonly returnBranch: string;
  readonly planSummary: string;
  readonly parameterDiffs: readonly ParameterDiff[];
  readonly featureDiffs: readonly FeatureDiff[];
  readonly volumeDeltaMm3: number;
  readonly verificationReport: VerificationReport;
  readonly commands: readonly WorkbenchSessionCommand[];
  readonly projectPath?: string | null;
  readonly candidateSource?: string;
  readonly mode?: "generate" | "edit";
}

export interface ChangePreviewInput {
  readonly previewBranch: string;
  readonly planSummary: string;
  readonly parameterDiffs?: readonly ParameterDiff[];
  readonly featureDiffs?: readonly FeatureDiff[];
  readonly volumeDeltaMm3?: number;
  readonly verificationReport: VerificationReport;
  readonly commands?: readonly WorkbenchSessionCommand[];
  readonly projectPath?: string | null;
  readonly candidateSource?: string;
  readonly mode?: "generate" | "edit";
}

export interface EngineeringSessionState {
  readonly revision: number;
  readonly activeBranch: string;
  /** `null` after a foreign KCL file replaces the demo document. */
  readonly graphIr: CanonicalGraph | null;
  readonly kclSource: string;
  readonly verificationReport: VerificationReport;
  readonly designContract: DesignContract | null;
  readonly selectedTopoRefs: readonly string[];
  readonly selectedNodeIds: readonly string[];
  readonly selectedNodeId: string | null;
  readonly preview: ChangePreview | null;
}

/**
 * Studio UI / in-memory session commands (selection, preview lifecycle, KCL view
 * sync, and local document edits). Authoritative backend geometry writes use
 * `CadApiSemanticCommand` (`src/types/apiCommands.ts`) via `cadApiAdapter.ts`.
 */
export type WorkbenchSessionCommand =
  | { readonly op: "upsert_parameter"; readonly value: ParameterDef }
  | { readonly op: "remove_parameter"; readonly name: string }
  | { readonly op: "rename_node"; readonly nodeId: string; readonly name: string }
  | { readonly op: "set_node_suppressed"; readonly nodeId: string; readonly suppressed: boolean }
  | { readonly op: "set_design_contract"; readonly contract: DesignContract | null }
  | { readonly op: "set_verification_report"; readonly report: VerificationReport }
  | { readonly op: "select"; readonly nodeId: string | null; readonly topoRefs?: readonly string[]; readonly nodeIds?: readonly string[] }
  | { readonly op: "sync_kcl_view"; readonly source: string; readonly recordHistory?: boolean }
  | { readonly op: "load_kcl_document"; readonly source: string }
  | { readonly op: "apply_graph_patch"; readonly patch: GraphPatch }
  | { readonly op: "set_active_branch"; readonly branch: string }
  | { readonly op: "open_preview"; readonly preview: ChangePreviewInput }
  | { readonly op: "commit_preview" }
  | { readonly op: "discard_preview" }
  | { readonly op: "reset_document" };

/**
 * @deprecated Renamed to {@link WorkbenchSessionCommand} to avoid colliding with
 * Rust `cad-api::SemanticCommand` (`CadApiSemanticCommand` in `src/types/apiCommands.ts`).
 */
export type SemanticCommand = WorkbenchSessionCommand;

export interface SessionOplogEntry {
  readonly seq: number;
  readonly summary: string;
  readonly commands: readonly WorkbenchSessionCommand[];
  readonly before: EngineeringSessionState;
  readonly after: EngineeringSessionState;
}

export interface EngineeringSessionStore {
  readonly state: EngineeringSessionState;
  readonly past: readonly SessionOplogEntry[];
  readonly future: readonly SessionOplogEntry[];
  readonly nextSeq: number;
}

export function emptyVerificationReport(): VerificationReport {
  return EMPTY_VERIFICATION_REPORT;
}

export function createEngineeringSession(
  overrides?: Partial<EngineeringSessionState>,
): EngineeringSessionStore {
  return {
    state: { ...createInitialState(), ...overrides },
    past: [],
    future: [],
    nextSeq: 1,
  };
}

export function createMechanicalSessionGraph(): CanonicalGraph {
  const flange = featureNode(
    "flange",
    "法兰底盘",
    "primitive_cylinder",
    "solid",
    { status: "valid" },
    {
      diameter: lengthMm(110),
      height: lengthMm(10),
    },
  );
  const hub = featureNode(
    "hub",
    "轴承座",
    "primitive_cylinder",
    "solid",
    { status: "valid" },
    {
      diameter: lengthMm(58),
      height: lengthMm(48),
    },
  );
  const bore = featureNode(
    "bearing-bore",
    "轴承内孔",
    "hole",
    "solid",
    { status: "valid" },
    {
      target: nodeRef("hub"),
      diameter: lengthMm(42),
    },
  );

  return {
    ir_schema_version: 1,
    graph_id: "lightweight-bearing-mount",
    units: { length: "mm", angle: "deg", force: "N", pressure: "MPa", mass: "kg" },
    parameters: {
      bore_diameter: lengthParameter("bore_diameter", 42),
      wall_thickness: lengthParameter("wall_thickness", 8),
      flange_od: lengthParameter("flange_od", 110),
      base_thickness: lengthParameter("base_thickness", 10),
      hub_height: lengthParameter("hub_height", 48),
      rib_count: {
        name: "rib_count",
        ir_type: "int",
        value: { type: "int", value: 6 },
        formula: null,
      },
      axial_load: {
        name: "axial_load",
        ir_type: "force",
        value: { type: "quantity", value: { dimension: "force", value: 1500, unit: "N" } },
        formula: null,
      },
    },
    blobs: {},
    nodes: { flange, hub, "bearing-bore": bore },
    outputs: ["bearing-bore"],
  };
}

export function parameterFromDimension(name: string, value: number, unit: string): ParameterDef {
  if (!Number.isFinite(value)) {
    throw new SessionCommandError("NON_FINITE_VALUE", `参数 ${name} 的值必须是有限数字。`);
  }
  const trimmed = name.trim();
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(trimmed)) {
    throw new SessionCommandError("INVALID_PARAMETER_NAME", `参数名无效：${name}`);
  }
  const quantity = quantityFromUnit(value, unit);
  if (quantity) {
    return { name: trimmed, ir_type: quantity.irType, value: quantity.value, formula: null };
  }
  return {
    name: trimmed,
    ir_type: "scalar",
    value: { type: "scalar", value },
    formula: null,
  };
}

export function holeRadiusParameterName(faceId: string): string {
  const safe = faceId.replace(/[^A-Za-z0-9_]/g, "_").replace(/^_+/, "");
  const ident = safe.length > 0 && /^[A-Za-z_]/.test(safe) ? safe : `face_${safe || "hole"}`;
  return `hole_radius_${ident}`;
}

/** `null` means the radius did not change, so the session should stay put. */
export function holeRadiusCommand(
  faceId: string,
  radiusMm: number,
  deltaMm: number,
): WorkbenchSessionCommand | null {
  if (!Number.isFinite(deltaMm) || !Number.isFinite(radiusMm)) {
    throw new SessionCommandError("NON_FINITE_VALUE", "孔半径偏移必须是有限数字。");
  }
  if (deltaMm === 0) return null;
  const next = radiusMm + deltaMm;
  if (radiusMm <= 0 || next <= 0) {
    throw new SessionCommandError("INVALID_HOLE_RADIUS", "孔半径必须大于 0 mm。");
  }
  return {
    op: "upsert_parameter",
    value: parameterFromDimension(holeRadiusParameterName(faceId), next, "mm"),
  };
}

/**
 * @deprecated Local regex fallback only; import directly from `./kclProjectionFallback.ts`.
 */
export const projectParameterIntoKcl = projectParameterIntoKclFallback;

export function dispatchSessionCommand(
  store: EngineeringSessionStore,
  command: WorkbenchSessionCommand,
  summary?: string,
): EngineeringSessionStore {
  return applySessionCommands(store, [command], summary ?? commandSummary(command));
}

/** One oplog entry and one revision for the whole batch. */
export function applySessionCommands(
  store: EngineeringSessionStore,
  commands: readonly WorkbenchSessionCommand[],
  summary = "批量语义命令",
): EngineeringSessionStore {
  if (commands.length === 0) return store;
  let next = store.state;
  const recorded: WorkbenchSessionCommand[] = [];
  let recordHistory = true;
  for (const command of commands) {
    if (command.op === "select" || command.op === "open_preview" || command.op === "discard_preview") {
      next = reduceDocument(next, command);
      continue;
    }
    if (command.op === "sync_kcl_view" && command.recordHistory === false) recordHistory = false;
    next = reduceDocument(next, command);
    recorded.push(command);
  }
  if (recorded.length === 0) {
    return sameSession(store.state, next) ? store : { ...store, state: next };
  }
  return commitDocument(store, next, recorded, summary, recordHistory);
}

export function undoSession(store: EngineeringSessionStore): {
  readonly store: EngineeringSessionStore;
  readonly undone?: SessionOplogEntry;
} {
  const entry = store.past[store.past.length - 1];
  if (!entry) return { store };
  return {
    store: {
      ...store,
      state: entry.before,
      past: store.past.slice(0, -1),
      future: [entry, ...store.future],
    },
    undone: entry,
  };
}

export function redoSession(store: EngineeringSessionStore): {
  readonly store: EngineeringSessionStore;
  readonly redone?: SessionOplogEntry;
} {
  const entry = store.future[0];
  if (!entry) return { store };
  return {
    store: {
      ...store,
      state: entry.after,
      past: [...store.past, entry],
      future: store.future.slice(1),
    },
    redone: entry,
  };
}

export function canUndoSession(store: EngineeringSessionStore): boolean {
  return store.past.length > 0;
}

export function canRedoSession(store: EngineeringSessionStore): boolean {
  return store.future.length > 0;
}

function createInitialState(): EngineeringSessionState {
  return {
    revision: 0,
    activeBranch: MAIN_BRANCH,
    graphIr: createMechanicalSessionGraph(),
    kclSource: INITIAL_DEMO_KCL,
    verificationReport: emptyVerificationReport(),
    designContract: null,
    selectedTopoRefs: [],
    selectedNodeIds: [],
    selectedNodeId: null,
    preview: null,
  };
}

function reduceDocument(state: EngineeringSessionState, command: WorkbenchSessionCommand): EngineeringSessionState {
  switch (command.op) {
    case "select":
      return {
        ...state,
        selectedNodeId: command.nodeId,
        selectedNodeIds: selectionIds(command.nodeId, command.nodeIds),
        selectedTopoRefs: [...(command.topoRefs ?? [])],
      };
    case "rename_node":
      return renameNode(state, command.nodeId, command.name);
    case "set_node_suppressed":
      return setNodeSuppressed(state, command.nodeId, command.suppressed);
    case "open_preview":
      return openPreview(state, command.preview);
    case "commit_preview":
      return commitPreview(state);
    case "discard_preview":
      return discardPreview(state);
    case "set_active_branch": {
      const branch = command.branch.trim();
      if (!branch) throw new SessionCommandError("INVALID_BRANCH", "分支名不能为空。");
      return { ...state, activeBranch: branch };
    }
    case "set_design_contract":
      return {
        ...state,
        designContract: command.contract,
        verificationReport: command.contract
          ? unknownReport(state.verificationReport, ATTACHED_CONTRACT_SUMMARY)
          : emptyVerificationReport(),
      };
    case "set_verification_report":
      if (!state.designContract) return { ...state, verificationReport: emptyVerificationReport() };
      return { ...state, verificationReport: command.report };
    case "reset_document":
      return { ...createInitialState(), revision: state.revision };
    case "load_kcl_document":
      if (command.source === INITIAL_DEMO_KCL) {
        return { ...createInitialState(), revision: state.revision };
      }
      return {
        ...state,
        kclSource: command.source,
        graphIr: null,
        selectedNodeId: null,
        selectedNodeIds: [],
        selectedTopoRefs: [],
        preview: null,
      };
    case "sync_kcl_view":
      return {
        ...state,
        activeBranch: state.preview ? state.preview.returnBranch : state.activeBranch,
        preview: null,
        kclSource: command.source,
        graphIr: state.graphIr
          ? syncGraphParametersFromKcl(state.graphIr, command.source)
          : null,
      };
    case "upsert_parameter": {
      const graph = sessionGraph(state);
      const existing = graph.parameters[command.value.name];
      const kclSource = projectParameterIntoKclFallback(state.kclSource, command.value);
      if (sameParameter(existing, command.value) && kclSource === state.kclSource) return state;
      return {
        ...state,
        graphIr: applyLocalGraphPatch(graph, {
          patch_id: `upsert-parameter:${command.value.name}:${state.revision + 1}`,
          description: `Update parameter ${command.value.name}`,
          ops: [{ op: "upsert_parameter", value: command.value }],
        }),
        kclSource,
      };
    }
    case "remove_parameter":
      return {
        ...state,
        graphIr: applyLocalGraphPatch(sessionGraph(state), {
          patch_id: `remove-parameter:${command.name}:${state.revision + 1}`,
          description: `Remove parameter ${command.name}`,
          ops: [{ op: "remove_parameter", value: { name: command.name } }],
        }),
      };
    case "apply_graph_patch": {
      let graphIr = applyLocalGraphPatch(sessionGraph(state), command.patch);
      let kclSource = state.kclSource;
      for (const op of command.patch.ops) {
        if (op.op === "upsert_parameter") kclSource = projectParameterIntoKclFallback(kclSource, op.value);
      }
      const selected = reconcileSelection(graphIr, state.selectedNodeId, state.selectedTopoRefs);
      if (graphIr === state.graphIr && kclSource === state.kclSource && selected.selectedNodeId === state.selectedNodeId) {
        graphIr = state.graphIr;
      }
      return { ...state, graphIr, kclSource, ...selected };
    }
    default: {
      const unreachable: never = command;
      return unreachable;
    }
  }
}

function commitDocument(
  store: EngineeringSessionStore,
  next: EngineeringSessionState,
  commands: readonly WorkbenchSessionCommand[],
  summary: string,
  recordHistory: boolean,
): EngineeringSessionStore {
  const invalidated = commands.some(commandChangesDocument);
  const withReport: EngineeringSessionState = invalidated
    ? { ...next, verificationReport: verificationAfterEdit(next) }
    : next;
  if (sameSession(store.state, withReport)) return store;
  const after: EngineeringSessionState = { ...withReport, revision: store.state.revision + 1 };
  if (!recordHistory) return { ...store, state: after };
  // When committing an open preview, store the clean main-branch snapshot in `before`
  // so a single Undo returns directly to the pre-preview document rather than re-opening the preview card.
  const before: EngineeringSessionState =
    store.state.preview && commands.some((command) => command.op === "commit_preview")
      ? {
          ...store.state,
          activeBranch: store.state.preview.returnBranch,
          preview: null,
        }
      : store.state;
  const entry: SessionOplogEntry = {
    seq: store.nextSeq,
    summary,
    commands,
    before,
    after,
  };
  const past = [...store.past, entry];
  return {
    state: after,
    past: past.length > MAX_OPLOG_ENTRIES ? past.slice(past.length - MAX_OPLOG_ENTRIES) : past,
    future: [],
    nextSeq: store.nextSeq + 1,
  };
}

function commandChangesDocument(command: WorkbenchSessionCommand): boolean {
  return command.op === "upsert_parameter"
    || command.op === "remove_parameter"
    || command.op === "rename_node"
    || command.op === "set_node_suppressed"
    || command.op === "sync_kcl_view"
    || command.op === "load_kcl_document"
    || command.op === "apply_graph_patch"
    || command.op === "reset_document";
}

function verificationAfterEdit(state: EngineeringSessionState): VerificationReport {
  if (!state.designContract) return emptyVerificationReport();
  if (
    state.verificationReport.summary === ATTACHED_CONTRACT_SUMMARY
    && state.verificationReport.overall_status === "UNKNOWN"
  ) {
    return state.verificationReport;
  }
  return unknownReport(state.verificationReport, STALE_VERIFICATION_SUMMARY);
}

function unknownReport(previous: VerificationReport, summary: string): VerificationReport {
  return buildVerificationReport({
    checks: [],
    kernel_identity: previous.kernel_identity.provider === "none"
      ? { provider: "occt", version: "7.9", authoritative: true }
      : previous.kernel_identity,
    summary,
  });
}

function sameSession(left: EngineeringSessionState, right: EngineeringSessionState): boolean {
  return left.activeBranch === right.activeBranch
    && left.kclSource === right.kclSource
    && left.graphIr === right.graphIr
    && left.designContract === right.designContract
    && sameReport(left.verificationReport, right.verificationReport)
    && left.selectedNodeId === right.selectedNodeId
    && sameRefs(left.selectedNodeIds, right.selectedNodeIds)
    && sameRefs(left.selectedTopoRefs, right.selectedTopoRefs)
    && samePreview(left.preview, right.preview);
}

function sameReport(left: VerificationReport, right: VerificationReport): boolean {
  return left === right
    || (
      left.overall_status === right.overall_status
      && left.summary === right.summary
      && VERIFICATION_LAYERS.every((layer) => left.layers[layer] === right.layers[layer])
    );
}

function sameParameter(existing: ParameterDef | undefined, next: ParameterDef): boolean {
  return existing !== undefined && JSON.stringify(existing) === JSON.stringify(next);
}

function sameRefs(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((item, index) => item === right[index]);
}

function samePreview(left: ChangePreview | null, right: ChangePreview | null): boolean {
  if (left === right) return true;
  if (!left || !right) return false;
  return left.previewBranch === right.previewBranch
    && left.returnBranch === right.returnBranch
    && left.planSummary === right.planSummary
    && left.volumeDeltaMm3 === right.volumeDeltaMm3
    && left.projectPath === right.projectPath
    && left.candidateSource === right.candidateSource
    && left.mode === right.mode
    && left.verificationReport === right.verificationReport
    && JSON.stringify(left.parameterDiffs) === JSON.stringify(right.parameterDiffs)
    && JSON.stringify(left.featureDiffs) === JSON.stringify(right.featureDiffs)
    && JSON.stringify(left.commands) === JSON.stringify(right.commands);
}

function selectionIds(nodeId: string | null, nodeIds: readonly string[] | undefined): readonly string[] {
  if (nodeIds) return [...nodeIds];
  return nodeId ? [nodeId] : [];
}

function isPreviewControl(command: WorkbenchSessionCommand): boolean {
  return command.op === "open_preview" || command.op === "commit_preview" || command.op === "discard_preview";
}

function sessionGraph(state: EngineeringSessionState): CanonicalGraph {
  if (!state.graphIr) {
    throw new SessionCommandError("NO_SESSION_GRAPH", "当前文档没有会话特征图。");
  }
  return state.graphIr;
}

function renameNode(state: EngineeringSessionState, nodeId: string, rawName: string): EngineeringSessionState {
  const name = rawName.trim();
  if (!name) throw new SessionCommandError("INVALID_NODE_NAME", "特征名不能为空。");
  const graph = sessionGraph(state);
  const node = graph.nodes[nodeId];
  if (!node) throw new SessionCommandError("NODE_NOT_FOUND", `找不到特征 ${nodeId}。`);
  if (node.name === name) return state;
  const graphIr = applyLocalGraphPatch(graph, {
    patch_id: `rename-node:${nodeId}:${state.revision + 1}`,
    description: `Rename ${nodeId}`,
    ops: [{ op: "update_node", value: { id: nodeId, name, inputs: null, kind: null } }],
  });
  return { ...state, graphIr, ...reconcileSelection(graphIr, state.selectedNodeId, state.selectedTopoRefs) };
}

function setNodeSuppressed(state: EngineeringSessionState, nodeId: string, suppressed: boolean): EngineeringSessionState {
  const graph = sessionGraph(state);
  const node = graph.nodes[nodeId];
  if (!node) throw new SessionCommandError("NODE_NOT_FOUND", `找不到特征 ${nodeId}。`);
  const alreadySuppressed = node.state.status === "suppressed";
  if (suppressed === alreadySuppressed) return state;
  const nextState: NodeState = suppressed ? { status: "suppressed" } : { status: "stale" };
  const graphIr = applyLocalGraphPatch(graph, {
    patch_id: `set-suppressed:${nodeId}:${state.revision + 1}`,
    description: `${suppressed ? "Suppress" : "Unsuppress"} ${nodeId}`,
    ops: [{ op: "set_node_state", value: { id: nodeId, state: nextState } }],
  });
  return { ...state, graphIr, ...reconcileSelection(graphIr, state.selectedNodeId, state.selectedTopoRefs) };
}

function openPreview(state: EngineeringSessionState, input: ChangePreviewInput): EngineeringSessionState {
  const previewBranch = input.previewBranch.trim();
  if (!previewBranch) throw new SessionCommandError("INVALID_BRANCH", "预览分支名不能为空。");
  if (!Number.isFinite(input.volumeDeltaMm3 ?? 0)) {
    throw new SessionCommandError("NON_FINITE_VALUE", "体积变化必须是有限数字。");
  }
  const preview: ChangePreview = {
    previewBranch,
    returnBranch: state.preview?.returnBranch ?? state.activeBranch,
    planSummary: input.planSummary,
    parameterDiffs: [...(input.parameterDiffs ?? [])],
    featureDiffs: [...(input.featureDiffs ?? [])],
    volumeDeltaMm3: input.volumeDeltaMm3 ?? 0,
    verificationReport: input.verificationReport,
    commands: [...(input.commands ?? [])],
    ...(input.projectPath !== undefined ? { projectPath: input.projectPath } : {}),
    ...(input.candidateSource !== undefined ? { candidateSource: input.candidateSource } : {}),
    ...(input.mode !== undefined ? { mode: input.mode } : {}),
  };
  return { ...state, activeBranch: previewBranch, preview };
}

function commitPreview(state: EngineeringSessionState): EngineeringSessionState {
  const preview = state.preview;
  if (!preview) return state;
  let next = state;
  for (const command of preview.commands) {
    if (isPreviewControl(command)) continue;
    next = reduceDocument(next, command);
  }
  return {
    ...next,
    activeBranch: preview.returnBranch,
    preview: null,
    verificationReport: preview.verificationReport,
  };
}

function discardPreview(state: EngineeringSessionState): EngineeringSessionState {
  if (!state.preview) return state;
  return {
    ...state,
    activeBranch: state.preview.returnBranch,
    preview: null,
  };
}

function reconcileSelection(
  graph: CanonicalGraph,
  nodeId: string | null,
  topoRefs: readonly string[],
): Pick<EngineeringSessionState, "selectedNodeId" | "selectedNodeIds" | "selectedTopoRefs"> {
  if (nodeId === null) return { selectedNodeId: null, selectedNodeIds: [], selectedTopoRefs: topoRefs };
  const node = graph.nodes[nodeId];
  if (!node) return { selectedNodeId: null, selectedNodeIds: [], selectedTopoRefs: [] };
  const failed = node.state.status === "failed_with_rollback" ? node.state.failed_topo_refs : [];
  return {
    selectedNodeId: nodeId,
    selectedNodeIds: [nodeId],
    selectedTopoRefs: sameRefs(failed, topoRefs) ? topoRefs : [...failed],
  };
}

function commandSummary(command: WorkbenchSessionCommand): string {
  switch (command.op) {
    case "upsert_parameter":
      return `更新参数 ${command.value.name}`;
    case "remove_parameter":
      return `移除参数 ${command.name}`;
    case "rename_node":
      return `重命名 ${command.nodeId}`;
    case "set_node_suppressed":
      return command.suppressed ? `抑制 ${command.nodeId}` : `取消抑制 ${command.nodeId}`;
    case "open_preview":
      return `打开预览 ${command.preview.previewBranch.trim()}`;
    case "commit_preview":
      return "确认合入预览";
    case "discard_preview":
      return "放弃预览";
    case "set_design_contract":
      return command.contract ? `绑定设计契约 ${command.contract.part_name}` : "移除设计契约";
    case "set_verification_report":
      return "更新验证报告";
    case "select":
      return "更新选择";
    case "sync_kcl_view":
      return "同步 KCL 视图";
    case "load_kcl_document":
      return "载入 KCL 文档";
    case "apply_graph_patch":
      return command.patch.description || "应用特征图补丁";
    case "set_active_branch":
      return `切换分支 ${command.branch.trim()}`;
    case "reset_document":
      return "重置工程会话";
    default: {
      const unreachable: never = command;
      return unreachable;
    }
  }
}

function quantityFromUnit(
  value: number,
  unit: string,
): { readonly irType: IrType; readonly value: IrValue } | null {
  switch (unit) {
    case "mm":
    case "m":
    case "in":
    case "um":
    case "cm":
      return {
        irType: "length",
        value: { type: "quantity", value: { dimension: "length", value, unit } },
      };
    case "deg":
    case "rad":
      return {
        irType: "angle",
        value: { type: "quantity", value: { dimension: "angle", value, unit } },
      };
    case "N":
    case "kN":
      return {
        irType: "force",
        value: { type: "quantity", value: { dimension: "force", value, unit } },
      };
    case "MPa":
    case "Pa":
      return {
        irType: "pressure",
        value: { type: "quantity", value: { dimension: "pressure", value, unit } },
      };
    case "g":
    case "kg":
      return {
        irType: "mass",
        value: { type: "quantity", value: { dimension: "mass", value, unit } },
      };
    default:
      return null;
  }
}

function lengthParameter(name: string, value: number): ParameterDef {
  return { name, ir_type: "length", value: lengthMm(value), formula: null };
}

function lengthMm(value: number): IrValue {
  return { type: "quantity", value: { dimension: "length", value, unit: "mm" } };
}

function nodeRef(nodeId: string): IrValue {
  return { type: "node_ref", value: { node_id: nodeId, port: "out" } };
}

function featureNode(
  id: string,
  name: string,
  kind: UnitNodeKind,
  outputType: IrNode["output_type"],
  state: IrNode["state"],
  inputs: Readonly<Record<string, IrValue>> = {},
): IrNode {
  return {
    id,
    name,
    kind: { kind },
    inputs,
    output_type: outputType,
    state,
    kernel_op_version: null,
    metadata: {},
  };
}
