/**
 * NX Part Navigator / Creo Model Tree style Fault-Tolerant Feature Tree.
 *
 * Features:
 * - Compact single-line industrial rows with status icon, feature icon, and hover actions
 * - Datum planes (XY / XZ / YZ) quick-sketch strip & quick search/filter bar
 * - Parent / child dependency highlighting when selecting or hovering a feature
 * - Creo-style "Insert Here" timeline rollback divider
 * - Fault-tolerant rollback diagnostics with TopoRef chips and one-click AI repair trigger
 */
import { useId, useMemo, useRef, useState } from "react";
import { experimentalTracksEnabled, isExperimentalBlob, isExperimentalFeatureNode } from "../../core/session/experimentalTracks.ts";
import type { DatumPlane } from "../../types/apiCommands.ts";
import { CadIcon, type CadIconName } from "../../ui/icons/CadIcons.tsx";
import { buildRollbackHighlightSummary, buildToggleSuppressPatch } from "./graphPatchBuilder.ts";
import { FeaturePropertyInspector } from "./FeaturePropertyInspector.tsx";
import { failedTopoRefsOf, readRecoveryHint } from "./nodeFailure.ts";
import type {
  BlobKind,
  BlobManifestEntry,
  CacheStatus,
  CanonicalGraph,
  GraphPatch,
  IrNode,
  IrType,
  IrValue,
  NodeState,
  ParameterDef,
} from "../../types/graphIr.ts";

export interface FaultTolerantFeatureTreeProps {
  readonly graph: CanonicalGraph;
  /** Shown next to `graph.graph_id` when the project name differs from the graph id. */
  readonly projectId?: string;
  readonly cacheStatus?: CacheStatus;
  readonly selectedNodeId?: string | null;
  readonly onEmitPatch: (patch: GraphPatch) => void;
  readonly onSelectNode: (nodeId: string, failedTopoRefs: readonly string[]) => void;
  readonly onRenameNode?: (nodeId: string, name: string) => void;
  readonly onSetNodeSuppressed?: (nodeId: string, suppressed: boolean) => void;
  readonly onUpsertParameter?: (value: ParameterDef) => void;
  /** Clicking a Datum plane launches Sketch on that plane (Creo/NX pattern). */
  readonly onStartSketchOnDatum?: (plane: DatumPlane) => void;
  /** Clicking "AI 修复" on a failed_with_rollback node invokes Point-and-Prompt repair. */
  readonly onAiRepairNode?: (nodeId: string, reason: string, failedTopoRefs: readonly string[]) => void;
  /** When false, hides bottom inline inspector because the unified Right Inspector displays it. */
  readonly showInlineInspector?: boolean;
}

type FeatureTrack = "Solid" | "Surface" | "SubD" | "Field" | "StdLib";

const TRACK_COLOR: Readonly<Record<FeatureTrack, string>> = {
  Solid: "var(--accent-primary)",
  Surface: "var(--accent-cyan)",
  SubD: "var(--accent-ai)",
  Field: "var(--status-ok)",
  StdLib: "var(--status-warn)",
};

export function FaultTolerantFeatureTree({
  graph,
  projectId,
  cacheStatus,
  selectedNodeId = null,
  onEmitPatch,
  onSelectNode,
  onRenameNode,
  onSetNodeSuppressed,
  onUpsertParameter,
  onStartSketchOnDatum,
  onAiRepairNode,
  showInlineInspector = true,
}: FaultTolerantFeatureTreeProps) {
  const showExperimental = experimentalTracksEnabled();
  const [filterText, setFilterText] = useState("");
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
  const [insertIndex, setInsertIndex] = useState<number | null>(null);

  const allNodes = useMemo(
    () => orderedFeatureNodes(graph).filter((node) => showExperimental || !isExperimentalFeatureNode(node)),
    [graph, showExperimental],
  );
  const dependencyMap = useMemo(() => buildDependencyMap(allNodes), [allNodes]);

  const nodes = useMemo(() => {
    const q = filterText.trim().toLowerCase();
    return allNodes.filter((node) => {
      if (onlyFailed && node.state.status !== "failed_with_rollback") return false;
      if (!q) return true;
      return `${node.name} ${node.id} ${operationLabel(node)}`.toLowerCase().includes(q);
    });
  }, [allNodes, filterText, onlyFailed]);

  const blobs = useMemo(
    () => orderedBlobs(graph).filter((blob) => showExperimental || !isExperimentalBlob(blob.kind)),
    [graph, showExperimental],
  );
  const parameters = useMemo(
    () => Object.values(graph.parameters).sort((a, b) => compareId(a.name, b.name)),
    [graph.parameters],
  );
  const summary = useMemo(() => buildRollbackHighlightSummary(graph), [graph]);
  const title = projectId && projectId !== graph.graph_id ? projectId : graph.graph_id;
  const showGraphId = title !== graph.graph_id;

  const focusNodeId = hoveredNodeId ?? selectedNodeId;
  const focusParents = focusNodeId ? dependencyMap.parents.get(focusNodeId) ?? EMPTY_SET : EMPTY_SET;
  const focusChildren = focusNodeId ? dependencyMap.children.get(focusNodeId) ?? EMPTY_SET : EMPTY_SET;
  const effectiveInsertIndex = insertIndex === null ? nodes.length : Math.min(insertIndex, nodes.length);

  const applyRollbackIndex = (nextIdx: number) => {
    const clamped = Math.max(1, Math.min(allNodes.length, nextIdx));
    setInsertIndex(clamped === allNodes.length ? null : clamped);
    // Reflect rollback visually and optionally suppress/unsuppress via single node click when requested.
  };

  return (
    <section className="panel-stack fault-tolerant-feature-tree" aria-label="容错特征树">
      {/* NX/Creo Tree Filter & Quick Datum Strip */}
      <div className="tree-toolbar-card">
        <div className="tree-search-row">
          <CadIcon name="search" size={12} />
          <input
            type="text"
            className="tree-search-input"
            aria-label="筛选特征节点"
            placeholder="过滤特征、类型或 ID…"
            value={filterText}
            onChange={(event) => setFilterText(event.target.value)}
          />
          {filterText && (
            <button
              type="button"
              className="icon-ghost-btn"
              aria-label="清空过滤"
              onClick={() => setFilterText("")}
            >
              <CadIcon name="close" size={11} />
            </button>
          )}
          {summary.failedNodeIds.length > 0 && (
            <button
              type="button"
              className={`tree-filter-pill${onlyFailed ? " is-active" : ""}`}
              title="仅显示故障回退节点"
              onClick={() => setOnlyFailed((cur) => !cur)}
            >
              异常 {summary.failedNodeIds.length}
            </button>
          )}
        </div>

        <details className="tree-section-details">
          <summary className="tree-section-summary">
            <CadIcon name="datumPlane" size={12} />
            <span>基准平面 (Datums)</span>
            <span className="mini-badge">3</span>
          </summary>
          <div className="tree-datum-chips">
            {(["xy", "xz", "yz"] as const).map((plane) => (
              <button
                key={plane}
                type="button"
                className="tree-datum-btn"
                title={`在 ${plane.toUpperCase()} 基准面上新建草图`}
                onClick={() => onStartSketchOnDatum?.(plane)}
              >
                <CadIcon name="datumPlane" size={11} />
                <span>{plane.toUpperCase()}_PLANE</span>
              </button>
            ))}
          </div>
        </details>

        {parameters.length > 0 && (
          <details className="tree-section-details">
            <summary className="tree-section-summary">
              <CadIcon name="properties" size={12} />
              <span>驱动参数 (fx)</span>
              <span className="mini-badge">{parameters.length}</span>
            </summary>
            <ul className="tree-param-list">
              {parameters.slice(0, 8).map((param) => (
                <li key={param.name} className="tree-param-row">
                  <span className="tree-param-name">{param.name}</span>
                  <code className="tree-param-val">
                    {param.value.type === "quantity"
                      ? `${param.value.value.value}${("unit" in param.value.value && param.value.value.unit) ? param.value.value.unit : ""}`
                      : param.value.type === "int" || param.value.type === "scalar"
                        ? String(param.value.value)
                        : param.ir_type}
                  </code>
                </li>
              ))}
            </ul>
          </details>
        )}
      </div>

      {/* Main Feature History Tree */}
      <div className="panel-card">
        <header className="panel-card-header">
          <h2 className="panel-card-title" title={graph.graph_id}>{title}</h2>
          <span className="mini-badge">{allNodes.length} 特征</span>
          {showGraphId ? <span className="mini-badge">{graph.graph_id}</span> : null}
          {cacheStatus === "kernel_version_drift" ? (
            <span className="status-pill is-warn">内核版本漂移</span>
          ) : null}
        </header>

        {summary.failedNodeIds.length > 0 ? (
          <p className="tree-rollback-hint">
            视口已保留上一次有效实体。点击节点或失效拓扑芯片可在 3D 视口标红定位。
          </p>
        ) : null}

        {nodes.length === 0 ? (
          <p className="tree-empty-hint">{filterText ? "没有匹配过滤条件的特征。" : "暂无特征节点。"}</p>
        ) : (
          <ol aria-label="特征节点列表" className="cad-feature-tree-list">
            {nodes.map((node, index) => {
              const relation: "parent" | "child" | null = focusParents.has(node.id)
                ? "parent"
                : focusChildren.has(node.id)
                  ? "child"
                  : null;
              const isRolledBack = effectiveInsertIndex < nodes.length && index >= effectiveInsertIndex;
              return (
                <div key={node.id}>
                  {index === effectiveInsertIndex && effectiveInsertIndex < nodes.length && (
                    <div className="tree-insert-bar is-active" role="separator" aria-label="Creo Insert Here 回滚标记">
                      <span>Insert Here（回滚位置 · 以下特征暂不参与）</span>
                      <button
                        type="button"
                        className="tree-insert-reset"
                        onClick={() => setInsertIndex(null)}
                      >
                        恢复末尾
                      </button>
                    </div>
                  )}
                  <FeatureRow
                    node={node}
                    selected={node.id === selectedNodeId}
                    relation={relation}
                    rolledBack={isRolledBack}
                    onHover={setHoveredNodeId}
                    onEmitPatch={onEmitPatch}
                    onSelectNode={onSelectNode}
                    onRenameNode={onRenameNode}
                    onSetNodeSuppressed={onSetNodeSuppressed}
                    onInsertAfter={() => applyRollbackIndex(index + 1)}
                    onAiRepairNode={onAiRepairNode}
                  />
                </div>
              );
            })}
            {effectiveInsertIndex === nodes.length && (
              <li className="tree-insert-bar" aria-label="特征时间轴末尾">
                <span>── Insert Here（特征时间轴末尾）──</span>
              </li>
            )}
          </ol>
        )}
      </div>

      {blobs.length > 0 && (
        <div className="panel-card">
          <header className="panel-card-header">
            <h3 className="panel-card-title">外部载荷块 (Blobs)</h3>
            <span className="mini-badge">{blobs.length} 项</span>
          </header>
          <ul aria-label="外部载荷块引用" className="cad-feature-tree-list">
            {blobs.map((blob) => (
              <BlobRow key={blob.handle} blob={blob} />
            ))}
          </ul>
        </div>
      )}

      {showInlineInspector && (
        <FeaturePropertyInspector
          graph={graph}
          node={selectedNodeId ? graph.nodes[selectedNodeId] ?? null : null}
          onRenameNode={onRenameNode}
          onSetNodeSuppressed={onSetNodeSuppressed}
          onUpsertParameter={onUpsertParameter}
          onEmitPatch={onEmitPatch}
        />
      )}
    </section>
  );
}

const EMPTY_SET: ReadonlySet<string> = new Set();

function FeatureRow({
  node,
  selected,
  relation,
  rolledBack,
  onHover,
  onEmitPatch,
  onSelectNode,
  onRenameNode,
  onSetNodeSuppressed,
  onInsertAfter,
  onAiRepairNode,
}: {
  readonly node: IrNode;
  readonly selected: boolean;
  readonly relation: "parent" | "child" | null;
  readonly rolledBack: boolean;
  readonly onHover: (nodeId: string | null) => void;
  readonly onEmitPatch: (patch: GraphPatch) => void;
  readonly onSelectNode: (nodeId: string, failedTopoRefs: readonly string[]) => void;
  readonly onRenameNode?: (nodeId: string, name: string) => void;
  readonly onSetNodeSuppressed?: (nodeId: string, suppressed: boolean) => void;
  readonly onInsertAfter: () => void;
  readonly onAiRepairNode?: (nodeId: string, reason: string, failedTopoRefs: readonly string[]) => void;
}) {
  const rowId = useId();
  const track = featureTrackOf(node);
  const suppressed = node.state.status === "suppressed";
  const failed = node.state.status === "failed_with_rollback";
  const refs = failedTopoRefsOf(node);
  const recoveryHint = readRecoveryHint(node);
  const failureId = `${rowId}-failure`;
  const label = node.name || node.id;
  const [renaming, setRenaming] = useState(false);
  const [draftName, setDraftName] = useState(label);
  const renameClosed = useRef(false);

  const commitRename = () => {
    if (renameClosed.current) return;
    renameClosed.current = true;
    setRenaming(false);
    const next = draftName.trim();
    if (!next || next === label) return;
    if (onRenameNode) {
      onRenameNode(node.id, next);
      return;
    }
    onEmitPatch({
      patch_id: `rename-node:${node.id}`,
      description: `Rename ${node.id}`,
      ops: [{ op: "update_node", value: { id: node.id, name: next, inputs: null, kind: null } }],
    });
  };

  const toggleSuppress = () => {
    if (onSetNodeSuppressed) {
      onSetNodeSuppressed(node.id, !suppressed);
      return;
    }
    onEmitPatch(buildToggleSuppressPatch(node));
  };

  return (
    <li
      data-node-id={node.id}
      data-status={node.state.status}
      data-track={track}
      data-failed-badge={failed ? "true" : undefined}
      aria-current={selected ? "true" : undefined}
      className={`cad-tree-row${selected ? " is-selected" : ""}${failed ? " is-failed" : ""}${suppressed || rolledBack ? " is-suppressed" : ""}${relation ? ` is-${relation}` : ""}`}
      onMouseEnter={() => onHover(node.id)}
      onMouseLeave={() => onHover(null)}
    >
      <div className="cad-tree-row-main" aria-describedby={failed ? failureId : undefined}>
        <span
          className={`cad-tree-status-dot is-${node.state.status}`}
          title={statusLabel(node.state)}
        />
        <span className="cad-tree-kind-icon" style={{ color: TRACK_COLOR[track] }} title={operationLabel(node)}>
          <CadIcon name={featureIconFor(node)} size={13} />
        </span>

        {renaming ? (
          <input
            className="text-input cad-tree-rename-input"
            aria-label={`重命名 ${label}`}
            value={draftName}
            autoFocus
            onChange={(event) => setDraftName(event.target.value)}
            onBlur={commitRename}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commitRename();
              } else if (event.key === "Escape") {
                renameClosed.current = true;
                setRenaming(false);
                setDraftName(label);
              }
            }}
          />
        ) : (
          <button
            type="button"
            className="cad-tree-name-btn"
            title={`${label} (${operationLabel(node)}) · 单击选中，双击重命名`}
            onClick={() => onSelectNode(node.id, refs)}
            onDoubleClick={() => {
              renameClosed.current = false;
              setDraftName(label);
              setRenaming(true);
            }}
          >
            <span className="cad-tree-name-text">{label}</span>
            {relation && (
              <span className={`cad-tree-rel-pill is-${relation}`}>
                {relation === "parent" ? "父" : "子"}
              </span>
            )}
          </button>
        )}

        {failed ? (
          <span className="status-pill is-error" data-failed-badge="true">
            失效 {refs.length}
          </span>
        ) : suppressed ? (
          <span className="mini-badge">已抑制</span>
        ) : node.state.status === "stale" ? (
          <span className="status-pill is-warn">待重算</span>
        ) : null}

        <span className="cad-tree-row-actions">
          <button
            type="button"
            className="dp-icon-btn"
            title="将 Insert Here 回滚标记置于此特征之后"
            aria-label={`在此处插入 ${label}`}
            onClick={onInsertAfter}
          >
            <CadIcon name="chevronDown" size={11} />
          </button>
          <button
            type="button"
            className={`dp-icon-btn${suppressed ? " is-active" : ""}`}
            title={suppressed ? "取消抑制特征" : "抑制特征 (Suppress)"}
            aria-pressed={suppressed}
            aria-label={`${suppressed ? "取消抑制" : "抑制"} ${label}`}
            onClick={toggleSuppress}
          >
            <CadIcon name={suppressed ? "eyeOff" : "eye"} size={12} />
          </button>
        </span>
      </div>

      {failed ? (
        <details open className="cad-tree-failure-box">
          <summary>回退原因与失效拓扑</summary>
          <div id={failureId}>
            <p className="cad-tree-failure-reason">{node.state.reason}</p>
            {recoveryHint ? <p className="cad-tree-failure-hint">恢复提示：{recoveryHint}</p> : null}
            {refs.length > 0 && (
              <ul aria-label="失效拓扑引用" className="cad-tree-chip-list">
                {refs.map((topoRef, index) => (
                  <li key={`${topoRef}-${index}`}>
                    <button
                      type="button"
                      className="topo-chip"
                      onClick={() => onSelectNode(node.id, [topoRef])}
                    >
                      {topoRef}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {onAiRepairNode && (
              <button
                type="button"
                className="pill-btn cad-tree-ai-repair-btn"
                onClick={() =>
                  onAiRepairNode(
                    node.id,
                    node.state.status === "failed_with_rollback" ? node.state.reason : "",
                    refs,
                  )
                }
              >
                <CadIcon name="sparkles" size={12} />
                <span>AI 协助修复此特征</span>
              </button>
            )}
          </div>
        </details>
      ) : null}
    </li>
  );
}

function BlobRow({ blob }: { readonly blob: BlobManifestEntry }) {
  return (
    <li title={blob.relative_path} className="cad-tree-blob-row">
      <span className="mini-badge">{blobMention(blob.kind)}</span>
      <span className="cad-tree-name-text">{blob.handle}</span>
      <code>sha256 {sha256Prefix(blob.sha256)}</code>
      <span>{formatByteSize(blob.byte_len)}</span>
    </li>
  );
}

function featureIconFor(node: IrNode): CadIconName {
  switch (node.kind.kind) {
    case "sketch_2d":
      return "sketch";
    case "primitive_box":
      return "boxPrimitive";
    case "primitive_cylinder":
      return "cylinderPrimitive";
    case "extrude":
      return "extrude";
    case "revolve":
      return "revolve";
    case "hole":
    case "std_lib_call":
      return "throughHole";
    case "fillet":
      return "fillet";
    case "chamfer":
      return "chamfer";
    case "shell":
      return "shell";
    case "draft":
      return "draft";
    case "pattern_linear":
    case "pattern_polar":
      return "pattern";
    case "mirror":
      return "mirror";
    case "boolean_union":
    case "boolean_subtract":
    case "boolean_intersect":
      return "booleanOp";
    case "direct_face_edit":
      return "directEdit";
    case "assembly_component":
      return "assembly";
    case "assembly_mate":
      return "mate";
    case "tag_faces":
      return "tag";
    case "assert_gate":
      return "assertGate";
    default:
      return "cube";
  }
}

function buildDependencyMap(nodes: readonly IrNode[]): {
  readonly parents: ReadonlyMap<string, ReadonlySet<string>>;
  readonly children: ReadonlyMap<string, ReadonlySet<string>>;
} {
  const known = new Set(nodes.map((n) => n.id));
  const parents = new Map<string, Set<string>>();
  const children = new Map<string, Set<string>>();
  for (const node of nodes) {
    parents.set(node.id, new Set());
    children.set(node.id, new Set());
  }
  for (const node of nodes) {
    for (const dep of referencedNodeIds(node)) {
      if (dep !== node.id && known.has(dep)) {
        parents.get(node.id)?.add(dep);
        children.get(dep)?.add(node.id);
      }
    }
  }
  return { parents, children };
}

function orderedFeatureNodes(graph: CanonicalGraph): IrNode[] {
  const nodes = Object.values(graph.nodes);
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const indegree = new Map<string, number>();
  const children = new Map<string, string[]>();
  for (const node of nodes) {
    indegree.set(node.id, 0);
    children.set(node.id, []);
  }
  for (const node of nodes) {
    const deps = [...new Set(referencedNodeIds(node))].filter((id) => byId.has(id) && id !== node.id);
    indegree.set(node.id, deps.length);
    for (const dep of deps) {
      children.get(dep)?.push(node.id);
    }
  }

  const ready = [...indegree.keys()].filter((id) => indegree.get(id) === 0).sort(compareId);
  const ordered: IrNode[] = [];
  while (ready.length > 0) {
    const id = ready.shift();
    if (id === undefined) break;
    const node = byId.get(id);
    if (node) ordered.push(node);
    const nextIds = [...(children.get(id) ?? [])].sort(compareId);
    for (const next of nextIds) {
      const degree = (indegree.get(next) ?? 1) - 1;
      indegree.set(next, degree);
      if (degree === 0) {
        ready.push(next);
        ready.sort(compareId);
      }
    }
  }

  if (ordered.length < nodes.length) {
    const seen = new Set(ordered.map((node) => node.id));
    const rest = nodes.filter((node) => !seen.has(node.id)).sort((left, right) => compareId(left.id, right.id));
    ordered.push(...rest);
  }
  return ordered;
}

function referencedNodeIds(node: IrNode): string[] {
  const ids: string[] = [];
  for (const value of Object.values(node.inputs)) collectNodeIds(value, ids);
  return ids;
}

function collectNodeIds(value: IrValue, ids: string[]): void {
  switch (value.type) {
    case "node_ref":
      ids.push(value.value.node_id);
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

function orderedBlobs(graph: CanonicalGraph): BlobManifestEntry[] {
  return Object.values(graph.blobs).sort((left, right) => compareId(left.handle, right.handle));
}

function featureTrackOf(node: IrNode): FeatureTrack {
  if (node.kind.kind === "std_lib_call") return "StdLib";
  const fromOutput = trackFromOutput(node.output_type);
  if (fromOutput) return fromOutput;
  const kind = node.kind.kind;
  if (kind === "field_tpms" || kind.startsWith("field_") || kind.startsWith("implicit_")) return "Field";
  if (kind.startsWith("subd_")) return "SubD";
  if (kind.startsWith("surface_") || node.output_type === "sketch") return "Surface";
  return "Solid";
}

function trackFromOutput(outputType: IrType): FeatureTrack | undefined {
  switch (outputType) {
    case "solid":
      return "Solid";
    case "surface":
      return "Surface";
    case "subd":
      return "SubD";
    case "scalar_field":
    case "vector_field":
      return "Field";
    default:
      return undefined;
  }
}

function operationLabel(node: IrNode): string {
  if (node.kind.kind === "field_tpms") return `field_tpms:${node.kind.family}`;
  if (node.kind.kind === "parameter_ref") return `parameter_ref:${node.kind.param_id}`;
  return node.kind.kind;
}

function statusLabel(state: NodeState): string {
  switch (state.status) {
    case "valid":
      return "有效";
    case "failed_with_rollback":
      return "已回退";
    case "suppressed":
      return "已抑制";
    case "stale":
      return "待重算";
  }
}

function blobMention(kind: BlobKind): string {
  switch (kind) {
    case "spline":
      return "@spline";
    case "subd":
      return "@subd";
    case "field":
      return "@field";
    case "import":
      return "@import";
  }
}

function sha256Prefix(sha256: string): string {
  const compact = sha256.trim().toLowerCase();
  if (compact.length <= 12) return compact;
  return `${compact.slice(0, 12)}…`;
}

function formatByteSize(byteLen: number): string {
  if (!Number.isFinite(byteLen) || byteLen < 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB"] as const;
  let size = byteLen;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  const rounded = unitIndex === 0 || Number.isInteger(size) ? String(Math.round(size)) : size.toFixed(1);
  return `${rounded} ${units[unitIndex]}`;
}

function compareId(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}
