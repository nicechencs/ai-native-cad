import {
  SELECTION_FILTER_OPTIONS,
  type SelectionFilterMode,
} from "../../core/viewport/displayState.ts";
import {
  SELECTION_INTENT_OPTIONS,
  type SelectionIntentRule,
} from "../../core/viewport/navigationPreset.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

export interface TopBorderBarProps {
  readonly selectionFilter: SelectionFilterMode;
  readonly onChangeSelectionFilter: (mode: SelectionFilterMode) => void;
  readonly selectionIntent: SelectionIntentRule;
  readonly onChangeSelectionIntent: (intent: SelectionIntentRule) => void;
  readonly cueText: string;
  readonly cueTone?: "idle" | "active" | "sketch" | "warn";
  readonly selectedCount: number;
  readonly selectionSummary?: string | null;
  readonly onClearSelection: () => void;
  readonly onTriggerInlineAi: () => void;
  readonly hasPreview: boolean;
  readonly previewBranch?: string | null;
  readonly onFocusPreview?: () => void;
}

function filterIcon(mode: SelectionFilterMode): CadIconName {
  switch (mode) {
    case "all":
      return "filterAll";
    case "face":
      return "filterFace";
    case "edge":
      return "filterEdge";
    case "body":
      return "filterBody";
  }
}

/**
 * Siemens NX inspired Top Border Bar & Cue Line (30px fixed strip directly above the workspace).
 * Keeps Selection Filter, Selection Intent rules, and real-time step prompts permanently accessible
 * without obstructing the 3D canvas.
 */
export function TopBorderBar({
  selectionFilter,
  onChangeSelectionFilter,
  selectionIntent,
  onChangeSelectionIntent,
  cueText,
  cueTone = "idle",
  selectedCount,
  selectionSummary,
  onClearSelection,
  onTriggerInlineAi,
  hasPreview,
  previewBranch,
  onFocusPreview,
}: TopBorderBarProps) {
  return (
    <div className="studio-top-border-bar" role="region" aria-label="顶部边框选择栏与提示行">
      <div className="tbb-left">
        <div className="tbb-filter-group" role="group" aria-label="选择过滤器">
          {SELECTION_FILTER_OPTIONS.map((opt) => {
            const active = selectionFilter === opt.id;
            return (
              <button
                key={opt.id}
                type="button"
                className={`tbb-filter-btn${active ? " is-active" : ""}`}
                aria-pressed={active}
                title={`${opt.label} (${opt.shortcut}) · ${opt.description}`}
                onClick={() => onChangeSelectionFilter(opt.id)}
              >
                <CadIcon name={filterIcon(opt.id)} size={12} />
                <span>{opt.shortLabel}</span>
              </button>
            );
          })}
        </div>

        <div className="tbb-divider" aria-hidden="true" />

        <label className="tbb-intent-wrap" title="NX 选择意图规则 (Selection Intent)">
          <span className="tbb-intent-label">意图</span>
          <select
            className="tbb-intent-select"
            aria-label="选择意图规则"
            value={selectionIntent}
            onChange={(event) => onChangeSelectionIntent(event.target.value as SelectionIntentRule)}
          >
            {SELECTION_INTENT_OPTIONS.map((opt) => (
              <option key={opt.id} value={opt.id} title={opt.description}>
                {opt.shortLabel}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className={`tbb-cue-line is-${cueTone}`} role="status" aria-live="polite">
        <CadIcon
          name={cueTone === "sketch" ? "sketch" : cueTone === "warn" ? "warning" : cueTone === "active" ? "info" : "mouse"}
          size={12}
        />
        <span className="tbb-cue-text">{cueText}</span>
      </div>

      <div className="tbb-right">
        {selectedCount > 0 && (
          <div className="tbb-selection-pill">
            <span className="tbb-selection-count" title={selectionSummary ?? undefined}>
              已选 {selectedCount} 项{selectionSummary ? ` · ${selectionSummary}` : ""}
            </span>
            <button
              type="button"
              className="tbb-pill-clear"
              title="清除选择 (Esc)"
              aria-label="清除当前选区"
              onClick={onClearSelection}
            >
              <CadIcon name="close" size={11} />
            </button>
          </div>
        )}

        <button
          type="button"
          className={`tbb-ai-pill${selectedCount > 0 ? " is-context" : ""}`}
          title="在视口内联唤起 AI Point-and-Prompt（自动挂载选中拓扑） (Alt+A)"
          onClick={onTriggerInlineAi}
        >
          <CadIcon name="sparkles" size={12} />
          <span>{selectedCount > 0 ? "AI 修改选区" : "AI 快捷指令"}</span>
          <kbd className="shortcut-kbd">Alt+A</kbd>
        </button>

        {hasPreview && (
          <button
            type="button"
            className="tbb-preview-pill"
            title="存在待审阅的候选分支预览"
            onClick={onFocusPreview}
          >
            <CadIcon name="sparkles" size={12} />
            <span>预览分支: {previewBranch ?? "preview"}</span>
          </button>
        )}
      </div>
    </div>
  );
}
