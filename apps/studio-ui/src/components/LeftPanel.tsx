import { useMemo, useRef, useState } from "react";
import { tokenizeKclLine, type KclTokenCategory } from "../language/kclMonarch.ts";
import type {
  CompilerDiagnostic,
  DesignContract,
  DiagnosticsReport,
  LspCompletionItem,
  LspHoverInfo,
  SourceSpan,
} from "../types/scene.ts";
import { analyzeKclLspFallback, applyKclCompletion } from "../utils/studioHelpers.ts";

export interface LeftPanelProps {
  readonly contract: DesignContract;
  readonly diagnosticsReport: DiagnosticsReport;
  readonly kclSource: string;
  readonly highlightedSpan: SourceSpan | null;
  readonly highlightLabel: string | null;
  readonly onChangeSource: (nextSource: string) => void;
  readonly onSelectSpan: (span: SourceSpan, label: string) => void;
  readonly onAiGenerate?: (
    prompt: string,
    material: string,
    manufacturingMethod: string,
  ) => void;
}

const TOKEN_COLOR_CLASS: Readonly<Record<KclTokenCategory, string>> = {
  comment: "kcl-tok-comment",
  annotation: "kcl-tok-annotation",
  keyword: "kcl-tok-keyword",
  type: "kcl-tok-type",
  namespace: "kcl-tok-namespace",
  dimension: "kcl-tok-dimension",
  number: "kcl-tok-number",
  constant: "kcl-tok-constant",
  pipe: "kcl-tok-pipe",
  string: "kcl-tok-string",
  operator: "kcl-tok-operator",
  plain: "kcl-tok-plain",
};

function isLineInSpan(lineNumber: number, span: SourceSpan | null): boolean {
  if (!span) {
    return false;
  }
  return lineNumber >= span.start_line && lineNumber <= span.end_line;
}

function getLineColFromOffset(
  source: string,
  offset: number,
): { readonly line: number; readonly col: number } {
  let line = 1;
  let col = 1;
  const limit = Math.min(offset, source.length);
  for (let i = 0; i < limit; i += 1) {
    if (source[i] === "\n") {
      line += 1;
      col = 1;
    } else {
      col += 1;
    }
  }
  return { line, col };
}

export function LeftPanel({
  contract,
  diagnosticsReport,
  kclSource,
  highlightedSpan,
  highlightLabel,
  onChangeSource,
  onSelectSpan,
  onAiGenerate,
}: LeftPanelProps) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const [editMode, setEditMode] = useState<"highlighted" | "raw">("highlighted");
  const [severityFilter, setSeverityFilter] = useState<
    "all" | CompilerDiagnostic["severity"]
  >("all");
  const [aiPrompt, setAiPrompt] = useState<string>(
    "设计一个 96x68x36mm 壁厚 2.5mm 的铝合金抽壳控制盒，带镜像安装耳和 M4x0.7 螺纹孔",
  );
  const [aiMaterial, setAiMaterial] = useState<string>("Al-6061-T6");
  const [aiMethod, setAiMethod] = useState<string>("CNC_3AXIS_MILLING");
  const [cursorPos, setCursorPos] = useState<{ readonly line: number; readonly col: number }>({
    line: 6,
    col: 5,
  });
  const [showLspDrawer, setShowLspDrawer] = useState<boolean>(false);

  const lines = useMemo(() => kclSource.split(/\r?\n/), [kclSource]);

  const lspResult = useMemo(
    () => analyzeKclLspFallback(kclSource, cursorPos.line, cursorPos.col),
    [kclSource, cursorPos],
  );

  const filteredDiagnostics = useMemo(() => {
    if (severityFilter === "all") {
      return diagnosticsReport.diagnostics;
    }
    return diagnosticsReport.diagnostics.filter(
      (diag) => diag.severity === severityFilter,
    );
  }, [diagnosticsReport.diagnostics, severityFilter]);

  const handleInsertCompletion = (item: LspCompletionItem) => {
    const edit = applyKclCompletion(kclSource, cursorPos.line, cursorPos.col, item.insert_text);
    onChangeSource(edit.source);
    setEditMode("raw");
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      const caret = edit.source.slice(0, edit.caret).replace(/\r\n/g, "\n").length;
      el?.focus();
      el?.setSelectionRange(caret, caret);
    });
  };

  const activeHover: LspHoverInfo | null = lspResult.hover;

  return (
    <aside className="panel panel-left" aria-label="AI 生成、设计契约、诊断与代码编辑器">
      {/* 0. AI 自然语言生成模型面板 */}
      {onAiGenerate && (
        <section className="card ai-gen-card">
          <div className="card-header">
            <span className="card-title">自然语言生成（本地模板）</span>
            <span className="badge badge-sync">CAD-DSL v0.2</span>
          </div>
          <textarea
            className="ai-prompt-input"
            rows={2}
            value={aiPrompt}
            onChange={(e) => setAiPrompt(e.target.value)}
            placeholder="输入自然语言机械需求（支持抽壳盒、扫掠管道、支架、装配体）..."
            aria-label="AI 自然语言设计需求"
          />
          <div className="ai-controls-row">
            <select
              className="select-input"
              value={aiMaterial}
              onChange={(e) => setAiMaterial(e.target.value)}
              aria-label="AI 生成材料"
            >
              <option value="Al-6061-T6">Al-6061-T6</option>
              <option value="Ti-6Al-4V">Ti-6Al-4V</option>
              <option value="SUS304">SUS304</option>
            </select>
            <select
              className="select-input"
              value={aiMethod}
              onChange={(e) => setAiMethod(e.target.value)}
              aria-label="AI 生成工艺"
            >
              <option value="CNC_3AXIS_MILLING">CNC 3轴铣削</option>
              <option value="CNC_5AXIS_MILLING">CNC 5轴铣削</option>
              <option value="ADDITIVE_SLM">金属 3D 打印</option>
            </select>
            <button
              type="button"
              className="btn-primary"
              onClick={() => onAiGenerate(aiPrompt, aiMaterial, aiMethod)}
            >
              生成模板源码
            </button>
          </div>
        </section>
      )}

      {/* 1. 自然语言设计契约卡片 */}
      <section className="card contract-card">
        <div className="card-header">
          <span className="card-title">设计契约 (Design Contract)</span>
          <span className="badge badge-accent">{contract.manufacturing_method}</span>
        </div>
        <div className="contract-summary">{contract.summary}</div>
        <dl className="contract-grid">
          <div>
            <dt>零件</dt>
            <dd>{contract.part_name}</dd>
          </div>
          <div>
            <dt>材料</dt>
            <dd>
              {typeof contract.material === "string"
                ? contract.material
                : contract.material.name}
            </dd>
          </div>
          <div>
            <dt>Keep-In 包络</dt>
            <dd>
              [{contract.keep_in_bbox_mm.min.join(", ")}] .. [
              {contract.keep_in_bbox_mm.max.join(", ")}]{" "}
              {typeof contract.units === "string"
                ? contract.units
                : contract.units.length}
            </dd>
          </div>
          <div>
            <dt>配合接口</dt>
            <dd>
              {contract.interfaces
                .map((i) => i.tolerance ?? i.interface_id)
                .join(" · ")}
            </dd>
          </div>
        </dl>
      </section>

      {/* 2. 约束与门禁诊断列表 */}
      <section className="card diagnostics-card">
        <div className="card-header">
          <span className="card-title">
            约束与门禁诊断 ({filteredDiagnostics.length})
          </span>
          <div className="pill-group" role="group" aria-label="诊断级别筛选">
            {(["all", "error", "warning", "info"] as const).map((level) => (
              <button
                key={level}
                type="button"
                className={`pill-btn ${severityFilter === level ? "active" : ""}`}
                onClick={() => setSeverityFilter(level)}
              >
                {level.toUpperCase()}
              </button>
            ))}
          </div>
        </div>
        <ul className="diag-list">
          {filteredDiagnostics.map((diag, idx) => (
            <li key={`${diag.code}-${idx}`}>
              <button
                type="button"
                className={`diag-item diag-${diag.severity}`}
                onClick={() => onSelectSpan(diag.span, `${diag.code} (L${diag.span.start_line})`)}
              >
                <div className="diag-item-top">
                  <span className={`severity-dot severity-${diag.severity}`} />
                  <span className="diag-code">{diag.code}</span>
                  <span className="diag-loc">
                    L{diag.span.start_line}:{diag.span.start_col}
                  </span>
                </div>
                <div className="diag-msg">{diag.message}</div>
              </button>
            </li>
          ))}
        </ul>
      </section>

      {/* 3. Monaco / CAD-DSL 代码编辑器壳 + LSP 悬停/补全 */}
      <section className="card editor-card">
        <div className="card-header">
          <div className="editor-title-group">
            <span className="card-title">CAD-DSL (.kcl)</span>
            {highlightedSpan && (
              <span className="badge badge-sync">
                {highlightLabel ?? "联动定位"} · L{highlightedSpan.start_line}:
                {highlightedSpan.start_col}
              </span>
            )}
          </div>
          <div className="pill-group">
            <button
              type="button"
              className={`pill-btn ${showLspDrawer ? "active" : ""}`}
              onClick={() => setShowLspDrawer((prev) => !prev)}
            >
              LSP 补全
            </button>
            <button
              type="button"
              className={`pill-btn ${editMode === "highlighted" ? "active" : ""}`}
              onClick={() => setEditMode("highlighted")}
            >
              Monarch 视图
            </button>
            <button
              type="button"
              className={`pill-btn ${editMode === "raw" ? "active" : ""}`}
              onClick={() => setEditMode("raw")}
            >
              直接编辑
            </button>
          </div>
        </div>

        {(showLspDrawer || activeHover) && (
          <div className="lsp-card">
            {activeHover && (
              <div className="lsp-hover-box">
                <div className="lsp-hover-title">
                  <strong>{activeHover.symbol}</strong>
                  <span className="badge badge-accent">{activeHover.dimension_type}</span>
                </div>
                <div className="lsp-hover-summary">{activeHover.summary}</div>
              </div>
            )}
            {showLspDrawer && (
              <div className="lsp-completion-row">
                {lspResult.completions.slice(0, 8).map((item) => (
                  <button
                    key={`${item.kind}-${item.label}`}
                    type="button"
                    className="topo-chip"
                    title={item.detail}
                    onClick={() => handleInsertCompletion(item)}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        {editMode === "highlighted" ? (
          <div className="kcl-code-view" role="region" aria-label="CAD-DSL 语法高亮视图">
            {lines.map((line, index) => {
              const lineNum = index + 1;
              const isHighlighted = isLineInSpan(lineNum, highlightedSpan);
              const tokens = tokenizeKclLine(line);
              return (
                <div
                  key={lineNum}
                  className={`kcl-line ${isHighlighted ? "kcl-line-active" : ""}`}
                  onClick={() => {
                    const firstIdent = /[a-zA-Z_][\w:]*/.exec(line);
                    setCursorPos({
                      line: lineNum,
                      col: firstIdent ? (firstIdent.index ?? 0) + 1 : 1,
                    });
                  }}
                >
                  <span className="kcl-gutter">{lineNum}</span>
                  <code className="kcl-line-code">
                    {tokens.length === 0 ? "\u00A0" : null}
                    {tokens.map((tok, tokIdx) => (
                      <span key={tokIdx} className={TOKEN_COLOR_CLASS[tok.category]}>
                        {tok.text}
                      </span>
                    ))}
                  </code>
                </div>
              );
            })}
          </div>
        ) : (
          <textarea
            ref={textareaRef}
            className="kcl-raw-textarea"
            value={kclSource}
            onChange={(e) => {
              const nextVal = e.target.value;
              onChangeSource(nextVal);
              setCursorPos(
                getLineColFromOffset(nextVal, e.target.selectionStart ?? 0),
              );
            }}
            onClick={(e) => {
              const target = e.currentTarget;
              setCursorPos(
                getLineColFromOffset(target.value, target.selectionStart ?? 0),
              );
            }}
            spellCheck={false}
            aria-label="CAD-DSL 源码编辑器"
          />
        )}
      </section>
    </aside>
  );
}
