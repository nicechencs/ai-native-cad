import { useMemo, useState } from "react";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";
import {
  computeKclLineDiff,
  computeMetricsDelta,
  formatDurationMs,
  type AiThreadItem,
} from "./aiDiff.ts";

export interface AiPipelineCardProps {
  readonly item: AiThreadItem;
  readonly canUndo?: boolean;
  readonly onUndo?: () => void;
  readonly onRetry?: () => void;
  readonly onConfigure?: () => void;
}

export function AiPipelineCard({ item, canUndo, onUndo, onRetry, onConfigure }: AiPipelineCardProps) {
  const [timelineExpanded, setTimelineExpanded] = useState(item.status === "running");
  const [diffExpanded, setDiffExpanded] = useState(false);

  const isRunning = item.status === "running";
  const isSuccess = item.status === "success";
  const isError = item.status === "error";
  const isCancelled = item.status === "cancelled";

  const showTimeline = isRunning || timelineExpanded;

  const diff = useMemo(() => {
    const before = item.feedback?.beforeSource;
    const after = item.feedback?.afterSource;
    if (typeof before !== "string" || typeof after !== "string" || before === after) return null;
    return computeKclLineDiff(before, after);
  }, [item.feedback?.beforeSource, item.feedback?.afterSource]);

  const metricsDelta = useMemo(
    () => computeMetricsDelta(item.feedback?.beforeMetrics, item.feedback?.afterMetrics),
    [item.feedback?.beforeMetrics, item.feedback?.afterMetrics],
  );

  const durationText = formatDurationMs(item.feedback?.durationMs);
  const latestStep = item.steps.length > 0 ? item.steps[item.steps.length - 1] : null;

  const summaryTitle = isRunning
    ? latestStep?.message || "正在调用 AI 工程代理并执行几何门禁校验…"
    : item.feedback?.message || (isCancelled ? "已取消本次生成任务" : "执行完成");

  return (
    <div
      className={`ai-run-card ${
        isRunning ? "is-running" : isError ? "is-error" : isSuccess ? "is-ok" : "is-cancelled"
      }`}
    >
      <button
        type="button"
        className="ai-run-card-head"
        onClick={() => setTimelineExpanded((v) => !v)}
        aria-expanded={showTimeline}
      >
        <span className="ai-run-status-icon">
          {isRunning ? (
            <CadIcon name="spinner" size={14} />
          ) : isSuccess ? (
            <CadIcon name="check" size={14} />
          ) : isError ? (
            <CadIcon name="error" size={14} />
          ) : (
            <CadIcon name="info" size={14} />
          )}
        </span>
        <span className="ai-run-summary-text">{summaryTitle}</span>
        <span className="ai-run-meta">
          {durationText && <span className="ai-run-duration">{durationText}</span>}
          {item.steps.length > 0 && <span className="mini-badge">{item.steps.length} 步</span>}
          <CadIcon name={showTimeline ? "chevronDown" : "chevronRight"} size={13} />
        </span>
      </button>

      {item.feedback?.detail && !isRunning && (
        <div className="ai-run-detail">{item.feedback.detail}</div>
      )}

      {showTimeline && item.steps.length > 0 && (
        <ol className="ai-timeline ai-timeline-connected" aria-label="AI 推理与门禁进度">
          {item.steps.map((step, idx) => {
            const isLatest = idx === item.steps.length - 1;
            const statusClass =
              step.passed === false
                ? "is-error"
                : step.passed === true || step.stage === "completed"
                  ? "is-ok"
                  : isRunning && isLatest
                    ? "is-active"
                    : "is-done";
            const prevTime = idx > 0 ? item.steps[idx - 1].timestamp : item.createdAt;
            const stepDelta =
              typeof step.timestamp === "number" && typeof prevTime === "number"
                ? formatDurationMs(step.timestamp - prevTime)
                : "";
            return (
              <li key={`${step.stage}-${idx}`} className={`ai-timeline-step ${statusClass}`}>
                <span className="timeline-dot" />
                <div className="timeline-body">
                  {typeof step.iteration === "number" && (
                    <span className="timeline-iter">
                      R{step.iteration}
                      {typeof step.max_iters === "number" ? `/${step.max_iters}` : ""}
                    </span>
                  )}
                  <span className="timeline-msg">{step.message}</span>
                </div>
                {stepDelta && <span className="timeline-time">{stepDelta}</span>}
              </li>
            );
          })}
        </ol>
      )}

      {!isRunning && (metricsDelta || item.feedback?.vlmEnabled !== undefined || diff) && (
        <div className="ai-run-gates">
          {metricsDelta && (
            <>
              <span className={`mini-badge${metricsDelta.watertightAfter ? " is-ok" : ""}`}>
                {metricsDelta.watertightAfter ? "L0 水密 ✓" : "L0 未水密"}
              </span>
              <span className="mini-badge">
                体积 {metricsDelta.volumeAfterCm3.toFixed(1)} cm³
                {metricsDelta.volumeDeltaPct !== null
                  ? ` (${metricsDelta.volumeDeltaPct >= 0 ? "+" : ""}${metricsDelta.volumeDeltaPct.toFixed(1)}%)`
                  : ""}
              </span>
              <span className="mini-badge">
                拓扑 {metricsDelta.facesBefore > 0 ? `${metricsDelta.facesBefore}→` : ""}
                {metricsDelta.facesAfter}F
              </span>
            </>
          )}
          {item.feedback?.vlmEnabled !== undefined && (
            <span className="mini-badge">VLM: {item.feedback.vlmEnabled ? "已启用" : "未启用"}</span>
          )}
          {item.feedback?.contractRepairAttempts !== undefined &&
            item.feedback.contractRepairAttempts > 0 && (
              <span className="mini-badge">自修复 {item.feedback.contractRepairAttempts} 次</span>
            )}
          {item.feedback?.retrievedExamples && item.feedback.retrievedExamples.length > 0 && (
            <span className="mini-badge">参考 {item.feedback.retrievedExamples.length} 样本</span>
          )}
        </div>
      )}

      {isError && (onRetry || onConfigure) && (
        <div className="ai-run-actions">
          {onRetry && (
            <button type="button" className="studio-btn studio-btn-sm" onClick={onRetry}>
              <CadIcon name="refresh" size={12} />
              <span>重试</span>
            </button>
          )}
          {onConfigure && (
            <button type="button" className="studio-btn studio-btn-sm" onClick={onConfigure}>
              <CadIcon name="settings" size={12} />
              <span>配置密钥</span>
            </button>
          )}
        </div>
      )}

      {!isRunning && (diff || (isSuccess && canUndo && onUndo)) && (
        <div className="ai-run-actions">
          {diff && (
            <button
              type="button"
              className="studio-btn studio-btn-sm"
              onClick={() => setDiffExpanded((v) => !v)}
            >
              <CadIcon name="code" size={12} />
              <span>
                {diffExpanded ? "收起 KCL 变更" : `查看 KCL 变更 (+${diff.added} / -${diff.removed})`}
              </span>
            </button>
          )}
          {isSuccess && canUndo && onUndo && (
            <button type="button" className="studio-btn studio-btn-sm" onClick={onUndo}>
              <CadIcon name="undo" size={12} />
              <span>撤销本次修改</span>
            </button>
          )}
        </div>
      )}

      {diffExpanded && diff && (
        <div className="ai-diff-viewer" aria-label="KCL 源码变更对比">
          <pre className="ai-diff-pre">
            {diff.lines.map((line, idx) => (
              <div key={idx} className={`ai-diff-line is-${line.kind}`}>
                <span className="ai-diff-gutter">
                  {line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "}
                </span>
                <span className="ai-diff-code">{line.text}</span>
              </div>
            ))}
          </pre>
          {diff.truncated && <div className="ai-diff-truncated">变更较长，已截断展示前 140 行。</div>}
        </div>
      )}
    </div>
  );
}
