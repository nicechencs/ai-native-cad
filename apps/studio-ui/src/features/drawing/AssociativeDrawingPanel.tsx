/**
 * Preview of one associative drawing sheet, plus the dimension and GD&T list.
 * Zoom, pan, and filters live in `drawingSheetModel` so the view stays easy to test.
 */
import { useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import {
  MAX_DRAWING_ZOOM,
  MIN_DRAWING_ZOOM,
  createInitialDrawingViewerState,
  filterDrawingDimensions,
  panDrawingViewer,
  resetDrawingView,
  sanitizeDrawingSvg,
  summarizeDimensionHealth,
  zoomDrawingViewer,
  type AssociativeDrawingSheetData,
  type DimensionBindingStatus,
  type DrawingDimensionItem,
  type DrawingViewKind,
  type DrawingViewerState,
} from "./drawingSheetModel.ts";

const ZOOM_IN_FACTOR = 1.25;
const ZOOM_OUT_FACTOR = 0.8;

const EMPTY_DIMENSIONS: readonly DrawingDimensionItem[] = [];
const EMPTY_VIEWS: readonly DrawingViewKind[] = [];

const PLACEHOLDER_SHEET: AssociativeDrawingSheetData = {
  sheetId: "",
  title: "",
  scaleLabel: "",
  svgMarkup: "",
  views: EMPTY_VIEWS,
  dimensions: EMPTY_DIMENSIONS,
};

const VIEW_FILTERS: readonly { readonly id: DrawingViewerState["activeView"]; readonly label: string }[] = [
  { id: "all", label: "全部" },
  { id: "front", label: "主视" },
  { id: "top", label: "俯视" },
  { id: "right", label: "右视" },
  { id: "iso", label: "轴测" },
  { id: "section_aa", label: "剖面 A-A" },
];

const STATUS_LABEL: Readonly<Record<DimensionBindingStatus, string>> = {
  resolved: "已关联",
  dangling: "悬空失效",
  ambiguous: "歧义",
};

const STATUS_CLASS: Readonly<Record<DimensionBindingStatus, string>> = {
  resolved: "mini-badge is-ok",
  dangling: "status-pill is-error",
  ambiguous: "status-pill is-warn",
};

export interface AssociativeDrawingPanelProps {
  readonly sheet: AssociativeDrawingSheetData | null;
  readonly onSelectTopoRef?: (topoRefId: string) => void;
  readonly onExportSvg?: (sheet: AssociativeDrawingSheetData) => void;
  readonly onExportDxf?: (sheet: AssociativeDrawingSheetData) => void;
}

interface DragPoint {
  readonly pointerId: number;
  x: number;
  y: number;
}

export function AssociativeDrawingPanel({
  sheet,
  onSelectTopoRef,
  onExportSvg,
  onExportDxf,
}: AssociativeDrawingPanelProps) {
  const sheetKey = sheet?.sheetId ?? "";
  const [viewer, setViewer] = useState(() => createInitialDrawingViewerState(sheet ?? PLACEHOLDER_SHEET));
  const [trackedSheetId, setTrackedSheetId] = useState(sheetKey);
  const drag = useRef<DragPoint | null>(null);

  if (trackedSheetId !== sheetKey) {
    setTrackedSheetId(sheetKey);
    setViewer(createInitialDrawingViewerState(sheet ?? PLACEHOLDER_SHEET));
  }

  const dimensions = sheet?.dimensions ?? EMPTY_DIMENSIONS;
  const health = useMemo(() => summarizeDimensionHealth(dimensions), [dimensions]);
  const visibleDimensions = useMemo(
    () => filterDrawingDimensions(dimensions, viewer.activeView, viewer.showOnlyDangling),
    [dimensions, viewer.activeView, viewer.showOnlyDangling],
  );
  const svgMarkup = sheet?.svgMarkup ?? "";
  const svg = useMemo(() => sanitizeDrawingSvg(svgMarkup), [svgMarkup]);

  const selectDimension = (dimensionId: string) => {
    setViewer((current) => {
      if (current.selectedDimensionId === dimensionId) return current;
      return { ...current, selectedDimensionId: dimensionId };
    });
  };

  const selectTopoRef = (item: DrawingDimensionItem) => {
    selectDimension(item.id);
    if (item.topoRefId.trim() === "") return;
    onSelectTopoRef?.(item.topoRefId);
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    drag.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const origin = drag.current;
    if (origin === null || origin.pointerId !== event.pointerId) return;
    const dx = event.clientX - origin.x;
    const dy = event.clientY - origin.y;
    if (dx === 0 && dy === 0) return;
    origin.x = event.clientX;
    origin.y = event.clientY;
    setViewer((current) => panDrawingViewer(current, dx, dy));
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
  };

  const healthClass = health.allHealthy
    ? "status-pill is-ok"
    : health.dangling > 0
      ? "status-pill is-error"
      : "status-pill is-warn";
  const title = sheet !== null && sheet.title.trim() !== "" ? sheet.title : "工程图纸";
  const scaleLabel = sheet !== null && sheet.scaleLabel.trim() !== "" ? sheet.scaleLabel : "—";

  return (
    <section className="panel-stack associative-drawing-panel" aria-label="关联工程图">
      <div className="panel-card">
        <header className="panel-card-header" style={headerStyle}>
          <h2 className="panel-card-title" title={sheet?.sheetId || undefined}>{title}</h2>
          <span className="mini-badge">比例 {scaleLabel}</span>
          <span className={healthClass} role="status" aria-live="polite" title={healthTitle(health)}>
            {health.resolved} 已关联 / {health.dangling} 悬空
          </span>
          {health.ambiguous > 0 ? (
            <span className="status-pill is-warn">{health.ambiguous} 歧义</span>
          ) : null}
          <span style={spacerStyle} />
          <button
            type="button"
            className="tool-btn is-compact"
            disabled={sheet === null}
            onClick={() => {
              if (sheet) onExportSvg?.(sheet);
            }}
          >
            导出 SVG
          </button>
          <button
            type="button"
            className="tool-btn is-compact"
            disabled={sheet === null}
            onClick={() => {
              if (sheet) onExportDxf?.(sheet);
            }}
          >
            导出 DXF
          </button>
        </header>

        <div style={toolbarStyle}>
          <div role="toolbar" aria-label="视图筛选" style={tierStyle}>
            {VIEW_FILTERS.map((filter) => (
              <button
                key={filter.id}
                type="button"
                className={`mini-badge${viewer.activeView === filter.id ? " is-ok" : ""}`}
                aria-pressed={viewer.activeView === filter.id}
                onClick={() => setViewer((current) => (
                  current.activeView === filter.id ? current : { ...current, activeView: filter.id }
                ))}
                style={tierButtonStyle}
              >
                {filter.label}
              </button>
            ))}
          </div>
          <div role="toolbar" aria-label="图纸缩放与复位" style={tierStyle}>
            <button
              type="button"
              className="tool-btn is-compact"
              disabled={viewer.zoom <= MIN_DRAWING_ZOOM}
              onClick={() => setViewer((current) => zoomDrawingViewer(current, ZOOM_OUT_FACTOR))}
            >
              缩小
            </button>
            <span className="mini-badge" data-zoom={viewer.zoom}>{formatZoom(viewer.zoom)}</span>
            <button
              type="button"
              className="tool-btn is-compact"
              disabled={viewer.zoom >= MAX_DRAWING_ZOOM}
              onClick={() => setViewer((current) => zoomDrawingViewer(current, ZOOM_IN_FACTOR))}
            >
              放大
            </button>
            <button
              type="button"
              className="tool-btn is-compact"
              onClick={() => setViewer((current) => resetDrawingView(current))}
            >
              复位视图
            </button>
          </div>
        </div>

        {sheet === null ? (
          <p style={emptyStyle}>当前未打开工程图纸。</p>
        ) : svg.trim() === "" ? (
          <p style={emptyStyle}>当前图纸暂无矢量预览。</p>
        ) : (
          <div
            aria-label="工程图纸预览区"
            data-sheet-id={sheet.sheetId}
            data-pan-x={viewer.panX}
            data-pan-y={viewer.panY}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
            onDragStart={(event) => event.preventDefault()}
            style={viewportStyle}
          >
            <div
              style={sheetFrameStyle(viewer)}
              dangerouslySetInnerHTML={{ __html: svg }}
            />
          </div>
        )}
      </div>

      <div className="panel-card">
        <header className="panel-card-header" style={headerStyle}>
          <h3 className="panel-card-title">关联尺寸与形位公差 (GD&T)</h3>
          <span className="mini-badge">{visibleDimensions.length}</span>
          <button
            type="button"
            className={`tool-btn is-compact${viewer.showOnlyDangling ? " is-active" : ""}`}
            aria-pressed={viewer.showOnlyDangling}
            onClick={() => setViewer((current) => ({ ...current, showOnlyDangling: !current.showOnlyDangling }))}
          >
            仅看悬空尺寸
          </button>
        </header>
        {visibleDimensions.length === 0 ? (
          <p style={emptyStyle}>
            {dimensions.length === 0 ? "当前图纸没有标注尺寸。" : "没有符合筛选条件的尺寸。"}
          </p>
        ) : (
          <ul aria-label="关联尺寸列表" style={listStyle}>
            {visibleDimensions.map((item) => {
              const selected = item.id === viewer.selectedDimensionId;
              return (
                <li
                  key={item.id}
                  data-dimension-id={item.id}
                  data-status={item.status}
                  data-view={item.view}
                  aria-current={selected ? "true" : undefined}
                  style={{
                    ...rowStyle,
                    background: selected ? "var(--bg-active)" : "transparent",
                    borderColor: selected ? "var(--accent-primary)" : "var(--border-subtle)",
                  }}
                >
                  <button
                    type="button"
                    onClick={() => selectDimension(item.id)}
                    style={rowMainButtonStyle}
                  >
                    <span style={nameStyle}>{item.label || item.id}</span>
                    <span style={opStyle}>{formatNominalMm(item.nominalMm)}</span>
                    <span style={opStyle}>{item.toleranceText.trim() || "—"}</span>
                    <span className={STATUS_CLASS[item.status]}>{STATUS_LABEL[item.status]}</span>
                  </button>
                  <button
                    type="button"
                    className="topo-chip"
                    data-topo-ref={item.topoRefId}
                    disabled={item.topoRefId.trim() === ""}
                    onClick={() => selectTopoRef(item)}
                  >
                    {item.topoRefId || "无拓扑引用"}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}

function healthTitle(health: { resolved: number; dangling: number; ambiguous: number; total: number }): string {
  return `${health.total} total, ${health.resolved} resolved, ${health.dangling} dangling, ${health.ambiguous} ambiguous`;
}

function formatZoom(zoom: number): string {
  if (!Number.isFinite(zoom)) return "—";
  return `${Math.round(zoom * 100)}%`;
}

function formatNominalMm(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return `${value.toFixed(2)} mm`;
}

function sheetFrameStyle(viewer: DrawingViewerState) {
  return {
    transform: `translate(${viewer.panX}px, ${viewer.panY}px) scale(${viewer.zoom})`,
    transformOrigin: "0 0",
    width: "max-content",
    background: "#f8fafc",
    padding: 8,
  } as const;
}

const headerStyle = { flexWrap: "wrap" } as const;
const toolbarStyle = { display: "flex", flexWrap: "wrap", alignItems: "center", gap: 8 } as const;
const tierStyle = { display: "inline-flex", flexWrap: "wrap", gap: 4 } as const;
const tierButtonStyle = { border: 0, cursor: "pointer", font: "inherit" } as const;
const spacerStyle = { flex: "1 1 8px" } as const;
const viewportStyle = {
  position: "relative",
  overflow: "hidden",
  minHeight: 240,
  maxHeight: 420,
  border: "1px solid var(--border-subtle)",
  borderRadius: "var(--radius-md)",
  background: "var(--bg-viewport)",
  cursor: "grab",
  touchAction: "none",
} as const;
const listStyle = {
  listStyle: "none",
  margin: 0,
  padding: 0,
  display: "flex",
  flexDirection: "column",
  gap: 6,
  maxHeight: 360,
  overflow: "auto",
} as const;
const rowStyle = {
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: 6,
  border: "1px solid var(--border-subtle)",
  borderRadius: "var(--radius-sm)",
  padding: "6px 8px",
} as const;
const rowMainButtonStyle = {
  flex: "1 1 180px",
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: 6,
  textAlign: "left",
  border: 0,
  background: "transparent",
  color: "inherit",
  font: "inherit",
  cursor: "pointer",
  padding: 0,
} as const;
const nameStyle = { fontWeight: 600, color: "var(--text-primary)" } as const;
const opStyle = { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)" } as const;
const emptyStyle = { margin: 0, color: "var(--text-muted)", fontSize: 12 } as const;
