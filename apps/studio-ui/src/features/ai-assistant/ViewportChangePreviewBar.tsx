import { useState } from "react";
import { useEngineeringSession } from "../../hooks/useEngineeringSession.ts";
import { useLiveAttachPreviewReady } from "../../hooks/useLiveAttachPreview.ts";
import { verdictClassName } from "../validation/validationPanelModel.ts";
import { buildChangePreviewView } from "./changePreviewModel.ts";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";

export interface ViewportChangePreviewBarProps {
  readonly busy?: boolean;
  readonly onCommit: () => void | Promise<void>;
  readonly onDiscard: () => void | Promise<void>;
}

/**
 * Viewport-native AI Candidate Branch Review Banner.
 * Surfaces candidate branch diffs and L0–L4 PASS/FAIL/UNKNOWN gates directly above the 3D canvas
 * so engineers can inspect WBOIT visual diffs and confirm/discard even when the right drawer is closed.
 */
export function ViewportChangePreviewBar({
  busy = false,
  onCommit,
  onDiscard,
}: ViewportChangePreviewBarProps) {
  useLiveAttachPreviewReady();
  const session = useEngineeringSession();
  const [expanded, setExpanded] = useState(false);

  if (!session.preview) return null;
  const view = buildChangePreviewView(session.preview);

  return (
    <section className="viewport-preview-banner" role="region" aria-label="视口 AI 候选分支审阅栏">
      <div className="vpb-main-row">
        <span className="vpb-title">
          <CadIcon name="sparkles" size={14} />
          <strong>AI 候选分支预览</strong>
          <span className="mini-badge">{view.previewBranch}</span>
        </span>
        <span className="vpb-summary" title={view.planSummary}>{view.planSummary}</span>
        <div className="vpb-gates" aria-label="L0-L4 预览门禁">
          {view.badges.map((badge) => (
            <span
              key={badge.layer}
              className={`vpb-gate-chip ${verdictClassName(badge.verdict)}`}
              title={`${badge.layer}: ${badge.verdict}`}
            >
              {badge.shortLabel}:{badge.verdict}
            </span>
          ))}
        </div>
        <span className="vpb-volume tabular-nums">ΔV {view.volumeLabel}</span>

        <div className="vpb-actions">
          <button
            type="button"
            className="fd-btn"
            onClick={() => setExpanded((cur) => !cur)}
          >
            <span>{expanded ? "收起差异" : "查看差异"}</span>
            <CadIcon name="chevronDown" size={11} />
          </button>
          <button
            type="button"
            className="fd-btn is-confirm"
            disabled={busy}
            onClick={() => void onCommit()}
          >
            <CadIcon name="check" size={13} />
            <span>{busy ? "合入中…" : "确认合入"}</span>
          </button>
          <button
            type="button"
            className="fd-btn is-cancel"
            disabled={busy}
            onClick={() => void onDiscard()}
          >
            <CadIcon name="close" size={13} />
            <span>放弃回滚</span>
          </button>
        </div>
      </div>

      {expanded && (
        <div className="vpb-diff-drawer">
          <div className="vpb-diff-col">
            <strong>参数差异 ({view.parameterLines.length})</strong>
            {view.parameterLines.length === 0 ? (
              <span className="muted-text">无参数变更</span>
            ) : (
              <ul>
                {view.parameterLines.map((line) => <li key={line}><code>{line}</code></li>)}
              </ul>
            )}
          </div>
          <div className="vpb-diff-col">
            <strong>特征差异 ({view.featureLines.length})</strong>
            {view.featureLines.length === 0 ? (
              <span className="muted-text">无特征变更</span>
            ) : (
              <ul>
                {view.featureLines.map((line) => <li key={line}>{line}</li>)}
              </ul>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
