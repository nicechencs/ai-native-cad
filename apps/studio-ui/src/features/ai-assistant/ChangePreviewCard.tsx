/**
 * Change Preview for a candidate branch.
 * Confirm merges the preview. Discard drops it. Both actions are semantic session commands.
 */
import { useEngineeringSession } from "../../hooks/useEngineeringSession.ts";
import { useLiveAttachPreviewReady } from "../../hooks/useLiveAttachPreview.ts";
import type { ChangePreview } from "../../core/session/engineeringSession.ts";
import { verdictClassName } from "../validation/validationPanelModel.ts";
import { buildChangePreviewView } from "./changePreviewModel.ts";

export interface ChangePreviewCardProps {
  readonly preview: ChangePreview;
  readonly busy?: boolean;
  readonly onCommit: () => void | Promise<void>;
  readonly onDiscard: () => void | Promise<void>;
}

export function ChangePreviewCard({ preview, busy = false, onCommit, onDiscard }: ChangePreviewCardProps) {
  const view = buildChangePreviewView(preview);
  return (
    <section className="card change-preview-card" aria-label="变更预览">
      <div className="card-header">
        <span className="card-title">变更预览</span>
        <span className="mini-badge">{view.previewBranch}</span>
        <span className={verdictClassName(view.overall)}>{view.overall}</span>
      </div>
      <p className="contract-summary">{view.planSummary}</p>
      <p className="validation-kernel">内核 {view.kernelLabel} · 体积 {view.volumeLabel}</p>
      <ul className="validation-layers" aria-label="预览门禁">
        {view.badges.map((badge) => (
          <li key={badge.layer}>
            <span className="diag-code">{badge.shortLabel}</span>
            <span className={verdictClassName(badge.verdict)} data-verdict={badge.verdict}>
              {badge.verdict}
            </span>
          </li>
        ))}
      </ul>
      <h4 className="explain-heading">参数差异</h4>
      {view.parameterLines.length === 0 ? (
        <p className="workbench-panel-empty">没有参数差异。</p>
      ) : (
        <ul className="explain-list" aria-label="参数差异">
          {view.parameterLines.map((line) => <li key={line}><code>{line}</code></li>)}
        </ul>
      )}
      <h4 className="explain-heading">特征差异</h4>
      {view.featureLines.length === 0 ? (
        <p className="workbench-panel-empty">没有特征差异。</p>
      ) : (
        <ul className="explain-list" aria-label="特征差异">
          {view.featureLines.map((line) => <li key={line}>{line}</li>)}
        </ul>
      )}
      <div className="pill-group">
        <button type="button" className="studio-btn studio-btn-sm" disabled={busy} onClick={() => void onCommit()}>
          {busy ? "处理中…" : "确认合入"}
        </button>
        <button type="button" className="studio-btn studio-btn-sm" disabled={busy} onClick={() => void onDiscard()}>
          放弃回滚
        </button>
      </div>
    </section>
  );
}

export interface SessionChangePreviewCardProps {
  readonly busy?: boolean;
  readonly onCommit?: () => void | Promise<void>;
  readonly onDiscard?: () => void | Promise<void>;
}

/** Reads the open preview from the engineering session and dispatches confirm or discard. */
export function SessionChangePreviewCard({ busy = false, onCommit, onDiscard }: SessionChangePreviewCardProps = {}) {
  useLiveAttachPreviewReady();
  const session = useEngineeringSession();
  if (!session.preview) return null;
  return (
    <ChangePreviewCard
      preview={session.preview}
      busy={busy}
      onCommit={
        onCommit ??
        (() => {
          session.dispatchSemanticCommand({ op: "commit_preview" });
        })
      }
      onDiscard={
        onDiscard ??
        (() => {
          session.dispatchSemanticCommand({ op: "discard_preview" });
        })
      }
    />
  );
}
