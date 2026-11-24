import { useState } from "react";
import { loadWorkbenchUiPrefs, updateWorkbenchUiPrefs } from "../core/layout/workbenchPrefsStore.ts";
import type { ExportKind, WorkbenchBackend } from "../sync/workbenchBackend.ts";
import { MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import type {
  DesignContract,
  DiagnosticsReport,
  MaterialPreset,
  SceneMeshPayload,
  SelectionTarget,
} from "../types/scene.ts";
import type { CanonicalGraph, GraphPatch, ParameterDef } from "../types/graphIr.ts";
import type { VerificationReport } from "../types/verification.ts";
import { massGramsFromVolume } from "../sync/materialAnnotation.ts";
import { computeEnvelopeStatus } from "../utils/metrics.ts";
import { FeaturePropertyInspector } from "../features/feature-tree/FeaturePropertyInspector.tsx";
import { ExplainModelPanel } from "../features/feature-tree/ExplainModelPanel.tsx";
import { ValidationPanel } from "../features/validation/ValidationPanel.tsx";
import type { ValidationJumpTarget } from "../features/validation/validationPanelModel.ts";
import { exportBlockedDetail, geometryAvailabilityBadge } from "../hooks/quickFeatureOutcome.ts";
import { CadIcon } from "../ui/icons/CadIcons.tsx";
import { WorkbenchTabPanel, WorkbenchTabs } from "./WorkbenchTabs.tsx";

const RIGHT_TABS = [
  { id: "properties", label: "属性检查" },
  { id: "export", label: "工程导出" },
] as const;

const PROPERTY_SUB_TABS = [
  { id: "geometry", label: "上下文与指标" },
  { id: "material", label: "材料与工艺" },
] as const;

type PropertySubTab = (typeof PROPERTY_SUB_TABS)[number]["id"];

export type RightWorkbenchTab = (typeof RIGHT_TABS)[number]["id"];

export interface RightPanelProps {
  readonly activeTab: RightWorkbenchTab | null;
  readonly onSelectTab: (tab: RightWorkbenchTab) => void;
  readonly embedded?: boolean;
  readonly onExport: (kind: ExportKind) => void;
  readonly backendKind: WorkbenchBackend["kind"];
  readonly canExport: boolean;
  readonly compilePending?: boolean;
  readonly showingLastAccepted?: boolean;
  readonly hostModel: boolean;
  readonly busyFormat?: ExportKind;
  readonly exportMessage?: string;
  readonly contract: DesignContract | null;
  readonly verificationReport: VerificationReport;
  readonly diagnosticsReport: DiagnosticsReport;
  readonly sceneMesh: SceneMeshPayload;
  readonly selectedMaterial: MaterialPreset;
  readonly onSelectMaterial: (material: MaterialPreset) => void;
  readonly selectedTarget?: SelectionTarget;
  readonly selectedCount?: number;
  readonly graphIr?: CanonicalGraph | null;
  readonly selectedNodeId?: string | null;
  readonly onRenameNode?: (nodeId: string, name: string) => void;
  readonly onSetNodeSuppressed?: (nodeId: string, suppressed: boolean) => void;
  readonly onUpsertParameter?: (value: ParameterDef) => void;
  readonly onEmitGraphPatch?: (patch: GraphPatch) => void;
  readonly onFocusSource?: () => void;
  readonly onTagSelected?: () => void;
  readonly onJumpToViolation?: (target: ValidationJumpTarget) => void;
}

/**
 * Unified Context-Aware Right Inspector & Export Drawer.
 * - Eliminates the duplicate right Assembly tab (moved into Left Assembly Navigator)
 * - Eliminates duplicate Material cards
 * - Displays selected Feature Node parameters/lineage OR selected Face/Edge topology metadata
 */
export function RightPanel({
  activeTab,
  onSelectTab,
  embedded = false,
  onExport,
  backendKind,
  canExport,
  compilePending = false,
  showingLastAccepted = false,
  hostModel,
  busyFormat,
  exportMessage,
  contract,
  verificationReport,
  diagnosticsReport,
  sceneMesh,
  selectedMaterial,
  onSelectMaterial,
  selectedTarget = null,
  selectedCount = 0,
  graphIr = null,
  selectedNodeId = null,
  onRenameNode,
  onSetNodeSuppressed,
  onUpsertParameter,
  onEmitGraphPatch,
  onFocusSource,
  onTagSelected,
  onJumpToViolation,
}: RightPanelProps) {
  const [propertySubTab, setPropertySubTab] = useState<PropertySubTab>(() => loadWorkbenchUiPrefs().propertySubTab);
  const selectPropertySubTab = (tab: PropertySubTab) => {
    setPropertySubTab(tab);
    updateWorkbenchUiPrefs({ propertySubTab: tab });
  };

  const blockedDetail = exportBlockedDetail({
    pending: compilePending,
    faceCount: sceneMesh.faces.length,
    diagnosticsStatus: diagnosticsReport.status,
    showingLastAccepted,
  });
  const exportLog = busyFormat
    ? "正在导出模型…"
    : exportMessage ??
      (!canExport
        ? (blockedDetail ?? "当前源码没有可导出的网格。")
        : !hostModel
          ? "离线预览仅支持网格边线 DXF、预览工程图 SVG 和网格 JSON。"
          : "选择格式导出当前编译模型。");

  const metrics = diagnosticsReport.metrics;
  const volumeCm3 = metrics.volume_mm3 / 1000;
  const massGrams = massGramsFromVolume(metrics.volume_mm3, selectedMaterial.density_g_cm3);
  const envelope = contract
    ? computeEnvelopeStatus(metrics.bbox_mm, contract.keep_in_bbox_mm)
    : null;
  const estimatedUnitCostUsd =
    14.5 + volumeCm3 * 0.12 * (100 / selectedMaterial.machinability_index);

  const selectedGraphNode = graphIr && selectedNodeId ? graphIr.nodes[selectedNodeId] ?? null : null;

  return (
    <div className={embedded ? "panel-embedded" : "panel panel-right workbench-side"} aria-label="属性检查与工程导出">
      {!embedded && (
        <WorkbenchTabs
          prefix="right"
          label="属性检查与工程导出"
          tabs={RIGHT_TABS}
          active={activeTab ?? "properties"}
          onSelect={onSelectTab}
          className="workbench-side-tabs"
        />
      )}

      <WorkbenchTabPanel prefix="right" id="properties" active={activeTab === "properties"} className="workbench-side-tabpanel">
        <WorkbenchTabs
          prefix="right-props-sub"
          label="属性子视图"
          tabs={PROPERTY_SUB_TABS}
          active={propertySubTab}
          onSelect={selectPropertySubTab}
          className="panel-subtabs"
          tabClassName={(_tab, active) => `panel-subtab${active ? " is-active" : ""}`}
        />

        <WorkbenchTabPanel
          prefix="right-props-sub"
          id="geometry"
          active={propertySubTab === "geometry"}
          className="panel-subtabpanel"
        >
          {/* Context card 1: Selected 3D Topology Entity */}
          {selectedTarget && (
            <section className="card inspector-context-card" aria-label="选中几何拓扑属性">
              <div className="card-header">
                <span className="card-title">
                  {selectedTarget.kind === "face" ? "选中实体面" : "选中拓扑边"}
                </span>
                <span className="mini-badge">
                  {selectedCount > 1 ? `共选中 ${selectedCount} 项` : selectedTarget.kind.toUpperCase()}
                </span>
              </div>
              <dl className="contract-grid compact">
                <div>
                  <dt>Topo ID</dt>
                  <dd>
                    <code>{selectedTarget.kind === "face" ? selectedTarget.face.face_id : selectedTarget.edge.edge_id}</code>
                  </dd>
                </div>
                <div>
                  <dt>几何类型</dt>
                  <dd>{selectedTarget.kind === "face" ? selectedTarget.face.surface_type : selectedTarget.edge.curve_type}</dd>
                </div>
                <div>
                  <dt>血缘标签</dt>
                  <dd>
                    {(selectedTarget.kind === "face"
                      ? selectedTarget.face.provenance_tags
                      : selectedTarget.edge.provenance_tags
                    ).join(", ") || "无"}
                  </dd>
                </div>
                <div>
                  <dt>KCL 位置</dt>
                  <dd>
                    L{selectedTarget.kind === "face"
                      ? `${selectedTarget.face.ast_span.start_line}..${selectedTarget.face.ast_span.end_line}`
                      : `${selectedTarget.edge.ast_span.start_line}..${selectedTarget.edge.ast_span.end_line}`}
                  </dd>
                </div>
              </dl>
              <div className="pill-group" style={{ marginTop: 6 }}>
                {onFocusSource && (
                  <button type="button" className="pill-btn" onClick={onFocusSource}>
                    <CadIcon name="code" size={12} />
                    <span>定位源码 (F12)</span>
                  </button>
                )}
                {onTagSelected && (
                  <button type="button" className="pill-btn" onClick={onTagSelected}>
                    <CadIcon name="tag" size={12} />
                    <span>语义打标</span>
                  </button>
                )}
              </div>
            </section>
          )}

          {/* Context card 2: Selected Parametric Feature Node (moved from left tree bottom) */}
          {graphIr && selectedGraphNode && onEmitGraphPatch && (
            <>
              <FeaturePropertyInspector
                graph={graphIr}
                node={selectedGraphNode}
                onRenameNode={onRenameNode}
                onSetNodeSuppressed={onSetNodeSuppressed}
                onUpsertParameter={onUpsertParameter}
                onEmitPatch={onEmitGraphPatch}
              />
              <ExplainModelPanel
                graph={graphIr}
                nodeId={selectedNodeId}
                contract={contract}
              />
            </>
          )}

          {/* Part-level mass & verification metrics */}
          <section className="card">
            <div className="card-header">
              <span className="card-title">零件质量与验证</span>
              <span
                className={`badge ${
                  compilePending || diagnosticsReport.status === "error" || !metrics.watertight
                    ? "badge-err"
                    : metrics.kernel_authoritative === false
                      ? "badge-warn"
                      : "badge-ok"
                }`}
              >
                {geometryAvailabilityBadge({
                  pending: compilePending,
                  faceCount: sceneMesh.faces.length,
                  diagnosticsStatus: diagnosticsReport.status,
                  watertight: metrics.watertight,
                  kernelAuthoritative: metrics.kernel_authoritative,
                })}
              </span>
            </div>

            <div className="kpi-grid">
              <div className="kpi-box">
                <span className="kpi-label">净体积</span>
                <strong className="kpi-value">{volumeCm3.toFixed(2)} cm³</strong>
                <span className="kpi-sub">{metrics.volume_mm3.toFixed(0)} mm³</span>
              </div>
              <div className="kpi-box">
                <span className="kpi-label">估算质量</span>
                <strong className="kpi-value">{massGrams.toFixed(1)} g</strong>
                <span className="kpi-sub">{(massGrams / 1000).toFixed(3)} kg</span>
              </div>
              <div className="kpi-box">
                <span className="kpi-label">预估单件加工</span>
                <strong className="kpi-value">${estimatedUnitCostUsd.toFixed(1)}</strong>
                <span className="kpi-sub">{contract?.manufacturing_method ?? verificationReport.overall_status}</span>
              </div>
              <div className="kpi-box">
                <span className="kpi-label">拓扑面 / 边</span>
                <strong className="kpi-value">
                  {metrics.face_count}F / {metrics.edge_count}E
                </strong>
                <span className="kpi-sub">
                  {typeof metrics.open_edge_count === "number"
                    ? `Open Edges: ${metrics.open_edge_count}`
                    : "TNP Provenance 100%"}
                </span>
              </div>
            </div>

            {envelope ? (
              <div className="envelope-meter">
                <div className="envelope-meter-header">
                  <span>
                    Keep-In 包络占用率
                    {envelope.isExceeded ? "（已超限）" : ""}
                  </span>
                  <span>
                    {envelope.bboxSizeX.toFixed(1)} × {envelope.bboxSizeY.toFixed(1)} ×{" "}
                    {envelope.bboxSizeZ.toFixed(1)} mm ({envelope.envelopeRatio}%)
                  </span>
                </div>
                <div className="progress-track">
                  <div
                    className={`progress-fill ${envelope.isExceeded ? "over" : "normal"}`}
                    style={{ width: `${envelope.barWidthPercent}%` }}
                  />
                </div>
              </div>
            ) : (
              <div className="envelope-meter">
                <div className="envelope-meter-header">
                  <span>UNKNOWN</span>
                  <span>No design contract attached</span>
                </div>
              </div>
            )}
            <ValidationPanel report={verificationReport} onJump={onJumpToViolation} />
          </section>
        </WorkbenchTabPanel>

        <WorkbenchTabPanel
          prefix="right-props-sub"
          id="material"
          active={propertySubTab === "material"}
          className="panel-subtabpanel"
        >
          <section className="card">
            <div className="card-header">
              <span className="card-title">材料与工艺属性</span>
              <span className="mini-badge">{contract?.interfaces.length ?? 0} 接口</span>
            </div>
            <label className="field-label" htmlFor="material-select">
              工程材料牌号（SSOT）
            </label>
            <select
              id="material-select"
              className="select-input"
              value={selectedMaterial.id}
              onChange={(e) => {
                const next = MATERIAL_PRESETS.find((m) => m.id === e.target.value);
                if (next) {
                  onSelectMaterial(next);
                }
              }}
            >
              {MATERIAL_PRESETS.map((mat) => (
                <option key={mat.id} value={mat.id}>
                  {mat.id} — {mat.name}
                </option>
              ))}
            </select>
            <dl className="contract-grid compact">
              <div>
                <dt>密度</dt>
                <dd>{selectedMaterial.density_g_cm3} g/cm³</dd>
              </div>
              <div>
                <dt>屈服强度</dt>
                <dd>{selectedMaterial.yield_strength_mpa} MPa</dd>
              </div>
              <div>
                <dt>可加工性指数</dt>
                <dd>{selectedMaterial.machinability_index} / 100</dd>
              </div>
              <div>
                <dt>轴向设计载荷</dt>
                <dd>
                  {contract?.loads[0]?.magnitude ?? 0}{" "}
                  {contract?.loads[0]?.unit ?? "N"}
                </dd>
              </div>
              <div>
                <dt>配合接口</dt>
                <dd>
                  {contract
                    ? contract.interfaces.map((item) => item.tolerance ?? item.interface_id).join(" · ") || "无"
                    : "No design contract attached"}
                </dd>
              </div>
            </dl>
          </section>
        </WorkbenchTabPanel>
      </WorkbenchTabPanel>

      <WorkbenchTabPanel prefix="right" id="export" active={activeTab === "export"} className="workbench-side-tabpanel">
        <section className="card">
          <div className="card-header">
            <span className="card-title">工程交付与格式导出</span>
          </div>
          <div className="export-actions">
            <button
              type="button"
              className="btn-primary"
              disabled={!canExport || !hostModel || busyFormat !== undefined}
              onClick={() => onExport("step")}
            >
              {busyFormat === "step" ? "导出中..." : backendKind === "tauri" ? "导出 STEP AP242 (含 PMI)" : "下载 STEP AP242"}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={!canExport || busyFormat !== undefined}
              onClick={() => onExport("dxf")}
            >
              {busyFormat === "dxf" ? "导出中..." : backendKind === "http" && hostModel ? "下载 DXF" : "下载网格边线 DXF（仅预览）"}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={!canExport || busyFormat !== undefined}
              onClick={() => onExport("drawing")}
            >
              {busyFormat === "drawing" ? "导出中..." : backendKind === "http" && hostModel ? "下载 2D 工程图 SVG" : "下载预览工程图 SVG"}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={!canExport || !hostModel || busyFormat !== undefined}
              onClick={() => onExport("3mf")}
            >
              {busyFormat === "3mf" ? "导出中..." : backendKind === "tauri" ? "导出 3MF" : "下载 3MF"}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={!canExport || busyFormat !== undefined}
              onClick={() => onExport("mesh")}
            >
              {busyFormat === "mesh" ? "导出中..." : "下载 Scene-Mesh JSON"}
            </button>
          </div>
          <pre className="export-log" role="status">{exportLog}</pre>
        </section>
      </WorkbenchTabPanel>
    </div>
  );
}
