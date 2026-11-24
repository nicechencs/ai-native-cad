import { useState } from "react";
import { experimentalTracksEnabled } from "../../core/session/experimentalTracks.ts";
import {
  dragSubdVertexHandle,
  evaluateTpmsPreviewSample,
  setSubdEdgeSharpness,
  setSubdVertexCreaseWeight,
  updateTpmsFieldPreview,
  type SubdFieldViewportState,
} from "./subdFieldViewportModel.ts";

export interface SubdAndFieldOverlayProps {
  readonly state: SubdFieldViewportState;
  readonly onChangeState: (next: SubdFieldViewportState) => void;
  readonly onCommitSubdPatch?: (state: SubdFieldViewportState) => void;
}

const MODE_OPTIONS: readonly {
  readonly value: SubdFieldViewportState["mode"];
  readonly label: string;
}[] = [
  { value: "brep_only", label: "B-Rep" },
  { value: "subd_cage", label: "SubD Cage" },
  { value: "hybrid_field", label: "Hybrid SDF Field" },
];

const TPMS_VARIANTS: readonly {
  readonly value: SubdFieldViewportState["fieldPreview"]["variant"];
  readonly label: string;
}[] = [
  { value: "gyroid", label: "Gyroid" },
  { value: "schwarz_p", label: "Schwarz P" },
  { value: "diamond", label: "Diamond" },
  { value: "lidinoid", label: "Lidinoid" },
];

type CoordinateAxis = 0 | 1 | 2;
const COORDINATE_LABELS = ["X", "Y", "Z"] as const;
const NUDGE_STEP_MM = 0.5;

const controlStyle = {
  width: "100%",
  padding: "3px 6px",
  borderRadius: "var(--radius-xs)",
  border: "1px solid var(--border-default)",
  background: "var(--bg-input)",
  color: "var(--text-primary)",
  fontSize: "11.5px",
  boxSizing: "border-box" as const,
};

const sectionStyle = {
  display: "flex",
  flexDirection: "column" as const,
  gap: "6px",
  width: "100%",
};

function finiteNumber(value: string): number | null {
  if (value.trim() === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function SubdAndFieldOverlay({
  state,
  onChangeState,
  onCommitSubdPatch,
}: SubdAndFieldOverlayProps) {
  const selectedVertex = state.vertices.find((vertex) => vertex.vertexIndex === state.selectedVertexIndex) ?? null;
  const selectedVertexIndex = selectedVertex?.vertexIndex ?? null;
  const [periodDraft, setPeriodDraft] = useState<string | null>(null);
  const [activeEdgeId, setActiveEdgeId] = useState(() => state.creases[0]?.edgeId ?? "");

  const incidentCreases =
    selectedVertexIndex === null
      ? []
      : state.creases.filter((edge) => edge.v0 === selectedVertexIndex || edge.v1 === selectedVertexIndex);
  const selectedEdgeOptions = incidentCreases.length > 0 ? incidentCreases : state.creases;
  const activeEdge = selectedEdgeOptions.find((edge) => edge.edgeId === activeEdgeId) ?? selectedEdgeOptions[0] ?? null;

  const publishSubdState = (next: SubdFieldViewportState) => {
    if (next === state) return;
    onChangeState(next);
    onCommitSubdPatch?.(next);
  };

  const selectMode = (mode: SubdFieldViewportState["mode"]) => {
    onChangeState({ ...state, mode });
  };

  const selectVertex = (vertexIndex: number) => {
    onChangeState({
      ...state,
      selectedVertexIndex: vertexIndex,
      vertices: state.vertices.map((vertex) => {
        const selected = vertex.vertexIndex === vertexIndex;
        return vertex.selected === selected ? vertex : { ...vertex, selected };
      }),
    });
  };

  const nudgeVertex = (axis: CoordinateAxis, amountMm: number) => {
    if (selectedVertexIndex === null) return;
    const delta: [number, number, number] = [0, 0, 0];
    delta[axis] = amountMm;
    publishSubdState(dragSubdVertexHandle(state, selectedVertexIndex, delta));
  };

  const setCreaseWeight = (weight: number) => {
    if (selectedVertexIndex === null) return;
    publishSubdState(setSubdVertexCreaseWeight(state, selectedVertexIndex, weight));
  };

  const updateField = (patch: Partial<SubdFieldViewportState["fieldPreview"]>) => {
    onChangeState(updateTpmsFieldPreview(state, patch));
  };

  const commitPeriod = () => {
    if (periodDraft === null) return;
    const periodMm = finiteNumber(periodDraft);
    if (periodMm === null) {
      setPeriodDraft(null);
      return;
    }
    updateField({ periodMm: Math.max(0.5, periodMm) });
    setPeriodDraft(null);
  };

  const samplePoint = selectedVertex?.positionMm ?? ([0, 0, 0] as const);
  const fieldSample = evaluateTpmsPreviewSample(
    state.fieldPreview.variant,
    samplePoint,
    state.fieldPreview.periodMm,
  );

  if (!experimentalTracksEnabled()) return null;

  return (
    <div
      className="viewport-hud-sliders"
      style={{ left: "12px", right: "auto", top: "56px", width: "min(390px, calc(100% - 24px))", maxHeight: "calc(100% - 70px)", overflowY: "auto" }}
    >
      <section className="hud-slider-card" style={sectionStyle} aria-label="SubD 与场预览控制">
        <div
          role="group"
          aria-label="视口显示模式"
          style={{ display: "flex", gap: "3px", padding: "3px", borderRadius: "var(--radius-sm)", background: "var(--bg-surface-2)" }}
        >
          {MODE_OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              aria-pressed={state.mode === option.value}
              className={`hud-seg-btn${state.mode === option.value ? " is-active" : ""}`}
              style={{ flex: 1, justifyContent: "center", paddingInline: "6px", fontSize: "11px" }}
              onClick={() => selectMode(option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
      </section>

      <section className="hud-slider-card" style={sectionStyle} aria-label="SubD 控制笼手柄">
        <span className="hud-slider-title">SubD Control Cage</span>

        {state.vertices.length > 0 ? (
          <label className="field-label" style={{ gap: "2px", fontSize: "10.5px" }}>
            Control vertex
            <select
              value={selectedVertexIndex === null ? "" : String(selectedVertexIndex)}
              style={controlStyle}
              aria-label="选择 SubD 控制顶点"
              onChange={(event) => {
                const vertexIndex = Number(event.target.value);
                if (Number.isFinite(vertexIndex)) selectVertex(vertexIndex);
              }}
            >
              <option value="" disabled>
                Select a handle
              </option>
              {state.vertices.map((vertex) => (
                <option key={vertex.vertexIndex} value={vertex.vertexIndex}>
                  V{vertex.vertexIndex} · {vertex.positionMm.map((value) => value.toFixed(2)).join(", ")} mm
                </option>
              ))}
            </select>
          </label>
        ) : (
          <span style={{ color: "var(--text-muted)", fontSize: "11px" }}>No control vertices in this object</span>
        )}

        {selectedVertex ? (
          <>
            <div style={{ display: "grid", gridTemplateColumns: "repeat(3, minmax(0, 1fr))", gap: "5px" }}>
              {COORDINATE_LABELS.map((label, axis) => (
                <label key={label} className="field-label" style={{ gap: "2px", fontSize: "10.5px" }}>
                  {label}
                  <input
                    type="text"
                    value={selectedVertex.positionMm[axis].toFixed(2)}
                    aria-label={`顶点 V${selectedVertex.vertexIndex} ${label} 坐标（毫米）`}
                    style={controlStyle}
                    readOnly
                  />
                </label>
              ))}
            </div>

            <div style={{ display: "flex", alignItems: "center", gap: "4px" }} aria-label="顶点坐标微调">
              <span style={{ fontSize: "10.5px", color: "var(--text-secondary)", marginRight: "2px" }}>Nudge</span>
              {COORDINATE_LABELS.map((label, axis) => (
                <span key={label} style={{ display: "inline-flex", gap: "2px" }}>
                  <button type="button" className="icon-ghost-btn" onClick={() => nudgeVertex(axis as CoordinateAxis, -NUDGE_STEP_MM)} aria-label={`沿 ${label} 轴减少 ${NUDGE_STEP_MM} 毫米`}>
                    −{label}
                  </button>
                  <button type="button" className="icon-ghost-btn" onClick={() => nudgeVertex(axis as CoordinateAxis, NUDGE_STEP_MM)} aria-label={`沿 ${label} 轴增加 ${NUDGE_STEP_MM} 毫米`}>
                    +{label}
                  </button>
                </span>
              ))}
            </div>

            <label className="field-label" style={{ gap: "3px" }}>
              <span style={{ display: "flex", justifyContent: "space-between" }}>
                <span>Crease weight</span>
                <span className="hud-slider-val">{selectedVertex.creaseWeight.toFixed(1)}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                step={0.1}
                value={selectedVertex.creaseWeight}
                aria-label={`顶点 V${selectedVertex.vertexIndex} crease weight`}
                onChange={(event) => setCreaseWeight(Number(event.target.value))}
              />
            </label>
          </>
        ) : (
          <span style={{ color: "var(--text-muted)", fontSize: "11px" }}>Select a handle to inspect coordinates</span>
        )}

        {state.creases.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: "5px", borderTop: "1px solid var(--border-subtle)", paddingTop: "6px" }}>
            <label className="field-label" style={{ gap: "2px", fontSize: "10.5px" }}>
              {incidentCreases.length > 0 ? "Incident crease edges" : "Crease edge"}
              <select
                value={activeEdge?.edgeId ?? ""}
                style={controlStyle}
                aria-label="选择 crease edge"
                onChange={(event) => setActiveEdgeId(event.target.value)}
              >
                {selectedEdgeOptions.map((edge) => (
                  <option key={edge.edgeId} value={edge.edgeId}>
                    E {edge.edgeId} · V{edge.v0}–V{edge.v1}
                  </option>
                ))}
              </select>
            </label>
            {activeEdge && (
              <label key={activeEdge.edgeId} style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr) 1.6fr 34px", gap: "6px", alignItems: "center", fontSize: "10.5px" }}>
                <span title={activeEdge.edgeId}>Sharpness</span>
                <input
                  type="range"
                  min={0}
                  max={10}
                  step={0.1}
                  value={activeEdge.sharpness}
                  aria-label={`边 ${activeEdge.edgeId} sharpness`}
                  onChange={(event) => publishSubdState(setSubdEdgeSharpness(state, activeEdge.edgeId, Number(event.target.value)))}
                />
                <span className="hud-slider-val">{activeEdge.sharpness.toFixed(1)}</span>
              </label>
            )}
          </div>
        )}
      </section>

      <section className="hud-slider-card" style={sectionStyle} aria-label="TPMS 场预览">
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "8px" }}>
          <label style={{ display: "inline-flex", alignItems: "center", gap: "6px", fontWeight: 600, fontSize: "12px" }}>
            <input
              type="checkbox"
              checked={state.fieldPreview.enabled}
              onChange={(event) => updateField({ enabled: event.target.checked })}
            />
            TPMS field preview
          </label>
          <span className="hud-slider-val" title="Sample at selected handle or origin">
            f(p) {fieldSample.toFixed(3)}
          </span>
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1.1fr 0.9fr", gap: "6px" }}>
          <label className="field-label" style={{ gap: "2px", fontSize: "10.5px" }}>
            Variant
            <select
              value={state.fieldPreview.variant}
              style={controlStyle}
              aria-label="TPMS variant"
              onChange={(event) => {
                const variant = TPMS_VARIANTS.some((option) => option.value === event.target.value)
                  ? (event.target.value as SubdFieldViewportState["fieldPreview"]["variant"])
                  : "gyroid";
                updateField({ variant });
              }}
            >
              {TPMS_VARIANTS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field-label" style={{ gap: "2px", fontSize: "10.5px" }}>
            Period (mm)
            <input
              type="number"
              min={0.5}
              step={0.5}
              value={periodDraft ?? String(state.fieldPreview.periodMm)}
              style={controlStyle}
              aria-label="TPMS period in millimeters"
              onChange={(event) => setPeriodDraft(event.target.value)}
              onBlur={commitPeriod}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  commitPeriod();
                }
              }}
            />
          </label>
        </div>

        <label className="field-label" style={{ gap: "3px" }}>
          <span style={{ display: "flex", justifyContent: "space-between" }}>
            <span>Iso level</span>
            <span className="hud-slider-val">{state.fieldPreview.isoLevel.toFixed(2)}</span>
          </span>
          <input
            type="range"
            min={-1}
            max={1}
            step={0.01}
            value={state.fieldPreview.isoLevel}
            aria-label="TPMS iso level"
            onChange={(event) => updateField({ isoLevel: Number(event.target.value) })}
          />
        </label>

        <label style={{ display: "inline-flex", alignItems: "center", gap: "6px", fontSize: "11px", color: "var(--text-secondary)" }}>
          <input
            type="checkbox"
            checked={state.fieldPreview.blendBrepDepth}
            onChange={(event) => updateField({ blendBrepDepth: event.target.checked })}
          />
          Blend B-Rep depth
        </label>
      </section>
    </div>
  );
}
