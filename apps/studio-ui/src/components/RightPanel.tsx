import { useState } from "react";
import { createDesktopIpcClient, type ExportResult } from "../../../desktop/src/ipcBridge.ts";
import { MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import type {
  AssemblyComponentInfo,
  DesignContract,
  DiagnosticsReport,
  MaterialPreset,
  SceneMeshPayload,
} from "../types/scene.ts";
import { computeEnvelopeStatus } from "../utils/metrics.ts";
import {
  generateFallbackDrawingSvg,
  generateFallbackDxfFromMesh,
} from "../utils/studioHelpers.ts";

export interface RightPanelProps {
  readonly kclSource: string;
  readonly contract: DesignContract;
  readonly diagnosticsReport: DiagnosticsReport;
  readonly sceneMesh: SceneMeshPayload;
  readonly selectedMaterial: MaterialPreset;
  readonly onSelectMaterial: (material: MaterialPreset) => void;
  readonly dxfContent?: string;
  readonly drawingSvg?: string;
  readonly selectedComponentId?: string | null;
  readonly onSelectComponent?: (componentId: string | null) => void;
  readonly onExportComponent?: (componentId: string) => void;
}

function triggerBrowserDownload(
  filename: string,
  content: string,
  mimeType: string,
): void {
  if (typeof document === "undefined" || typeof URL === "undefined") {
    return;
  }
  const blob = new Blob([content], { type: mimeType });
  const a = document.createElement("a");
  const objectUrl = URL.createObjectURL(blob);
  a.href = objectUrl;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
}

export function RightPanel({
  kclSource,
  contract,
  diagnosticsReport,
  sceneMesh,
  selectedMaterial,
  onSelectMaterial,
  dxfContent,
  drawingSvg,
  selectedComponentId,
  onSelectComponent,
  onExportComponent,
}: RightPanelProps) {
  const [exportLog, setExportLog] = useState<string>(
    "演示模式：可下载网格边线 DXF、工程图 SVG 和 Scene-Mesh JSON；真实工艺文件请使用完整工作台。",
  );
  const [busyFormat, setBusyFormat] = useState<string | null>(null);

  const metrics = diagnosticsReport.metrics;
  const volumeCm3 = metrics.volume_mm3 / 1000;
  const massGrams = volumeCm3 * selectedMaterial.density_g_cm3;
  const {
    bboxSizeX,
    bboxSizeY,
    bboxSizeZ,
    envelopeRatio,
    barWidthPercent,
    isExceeded: isEnvelopeExceeded,
  } = computeEnvelopeStatus(metrics.bbox_mm, contract.keep_in_bbox_mm);

  const components: readonly AssemblyComponentInfo[] =
    sceneMesh.components ?? diagnosticsReport.components ?? [];

  const estimatedUnitCostUsd =
    14.5 + (volumeCm3 * 0.12) * (100 / selectedMaterial.machinability_index);

  const ipc = createDesktopIpcClient();

  const handleExport = async (kind: "step" | "dxf" | "drawing" | "mesh") => {
    setBusyFormat(kind);
    try {
      if (kind === "step") {
        const res: ExportResult = await ipc.exportStepAp242({
          file_name: `${sceneMesh.part_name}.kcl`,
          source: kclSource,
          out_path: `out/${sceneMesh.part_name}.step`,
          include_pmi: true,
        });
        setExportLog(`[STEP AP242] ${res.message} -> ${res.out_path}`);
      } else if (kind === "dxf") {
        const dxfText = dxfContent ?? generateFallbackDxfFromMesh(sceneMesh);
        triggerBrowserDownload(
          `${sceneMesh.part_name}.dxf`,
          dxfText,
          "application/dxf",
        );
        setExportLog(`[网格边线 DXF] 已下载 ${dxfText.length} 字节，仅供预览，不用于切割。`);
      } else if (kind === "drawing") {
        const svgText =
          drawingSvg ?? generateFallbackDrawingSvg(sceneMesh, metrics, {});
        triggerBrowserDownload(
          `${sceneMesh.part_name}_drawing.svg`,
          svgText,
          "image/svg+xml",
        );
        setExportLog(
          `[2D 工程制图 SVG] 已导出图框与标注图纸 (${svgText.length} 字节)`,
        );
      } else {
        const json = JSON.stringify(sceneMesh, null, 2);
        triggerBrowserDownload(`${sceneMesh.part_name}.mesh.json`, json, "application/json");
        setExportLog(`[Scene-Mesh ${sceneMesh.format_version}] 已下载网格 JSON。`);
      }
    } catch (err) {
      setExportLog(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyFormat(null);
    }
  };

  return (
    <aside className="panel panel-right" aria-label="物理估算、BOM 拆件、材料与导出仪表盘">
      {/* 1. 体积/质量/水密性物理仪表盘 */}
      <section className="card">
        <div className="card-header">
          <span className="card-title">工程量纲与质量估算</span>
          <span
            className={`badge ${
              metrics.watertight ? "badge-ok" : "badge-err"
            }`}
          >
            {metrics.watertight ? "L0 水密流形" : "几何超限"}
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
            <span className="kpi-sub">{contract.manufacturing_method}</span>
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

        <div className="envelope-meter">
          <div className="envelope-meter-header">
            <span>
              包围盒包络占用率
              {isEnvelopeExceeded ? "（已超限）" : ""}
            </span>
            <span>
              {bboxSizeX.toFixed(1)} × {bboxSizeY.toFixed(1)} ×{" "}
              {bboxSizeZ.toFixed(1)} mm ({envelopeRatio}%)
            </span>
          </div>
          <div className="progress-track">
            <div
              className={`progress-fill ${
                isEnvelopeExceeded ? "over" : "normal"
              }`}
              style={{ width: `${barWidthPercent}%` }}
            />
          </div>
        </div>
      </section>

      {/* 2. 装配体 BOM 拆件面板 */}
      {components.length > 0 && (
        <section className="card">
          <div className="card-header">
            <span className="card-title">装配体 BOM ({components.length})</span>
            {selectedComponentId && onSelectComponent && (
              <button
                type="button"
                className="pill-btn"
                onClick={() => onSelectComponent(null)}
              >
                全部显示
              </button>
            )}
          </div>
          <ul className="bom-list">
            {components.map((comp) => {
              const isSelected = selectedComponentId === comp.component_id;
              return (
                <li key={comp.component_id} className={`bom-item ${isSelected ? "active" : ""}`}>
                  <button
                    type="button"
                    className="bom-select-btn"
                    onClick={() =>
                      onSelectComponent?.(isSelected ? null : comp.component_id)
                    }
                  >
                    <strong>{comp.part_name}</strong>
                    <span>#{comp.component_id}</span>
                  </button>
                  {onExportComponent && (
                    <button
                      type="button"
                      className="pill-btn"
                      onClick={() => onExportComponent(comp.component_id)}
                    >
                      拆件导出
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      )}

      {/* 3. 材料选型与力学基线 */}
      <section className="card">
        <div className="card-header">
          <span className="card-title">材料与工艺属性</span>
        </div>
        <label className="field-label" htmlFor="material-select">
          工程材料牌号
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
              {contract.loads[0]?.magnitude ?? 0}{" "}
              {contract.loads[0]?.unit ?? "N"}
            </dd>
          </div>
        </dl>
      </section>

      {/* 4. STEP AP242 / 1:1 DXF / 2D 工程制图导出 */}
      <section className="card">
        <div className="card-header">
          <span className="card-title">工业标准导出控制</span>
        </div>
        <div className="export-actions">
          <button
            type="button"
            className="btn-primary"
            disabled={busyFormat !== null}
            onClick={() => void handleExport("step")}
          >
            {busyFormat === "step" ? "导出中..." : "导出 STEP AP242 (含 PMI)"}
          </button>
          <button
            type="button"
            className="btn-secondary"
            disabled={busyFormat !== null}
            onClick={() => void handleExport("dxf")}
          >
            {busyFormat === "dxf" ? "导出中..." : "下载网格边线 DXF（仅预览）"}
          </button>
          <button
            type="button"
            className="btn-secondary"
            disabled={busyFormat !== null}
            onClick={() => void handleExport("drawing")}
          >
            {busyFormat === "drawing" ? "导出中..." : "下载 2D 工程制图 (.svg)"}
          </button>
          <button
            type="button"
            className="btn-secondary"
            disabled={busyFormat !== null}
            onClick={() => void handleExport("mesh")}
          >
            下载 Scene-Mesh JSON
          </button>
        </div>
        <pre className="export-log">{exportLog}</pre>
      </section>
    </aside>
  );
}
