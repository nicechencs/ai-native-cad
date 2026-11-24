/**
 * Five-layer tri-state validation panel.
 * Each layer is PASS, FAIL, or UNKNOWN. A failed check can jump to its node or TopoRef.
 */
import type { VerificationReport } from "../../types/verification.ts";
import {
  buildValidationPanelModel,
  verdictClassName,
  type ValidationJumpTarget,
} from "./validationPanelModel.ts";

export interface ValidationPanelProps {
  readonly report: VerificationReport;
  readonly onJump?: (target: ValidationJumpTarget) => void;
}

export function ValidationPanel({ report, onJump }: ValidationPanelProps) {
  const model = buildValidationPanelModel(report);
  return (
    <section className="validation-panel" aria-label="五层验证">
      <div className="card-header">
        <span className="card-title">验证门禁</span>
        <span className={verdictClassName(model.overall)} data-verdict={model.overall}>
          {model.overall}
        </span>
      </div>
      <p className="validation-kernel">内核 {model.kernelLabel}</p>
      <ul className="validation-layers" aria-label="验证分层">
        {model.badges.map((badge) => (
          <li key={badge.layer}>
            <span className="diag-code">{badge.shortLabel}</span>
            <span
              className={verdictClassName(badge.verdict)}
              data-layer={badge.layer}
              data-verdict={badge.verdict}
              data-tone={badge.tone}
            >
              {badge.verdict}
            </span>
          </li>
        ))}
      </ul>
      {model.jumps.length === 0 ? (
        <p className="workbench-panel-empty">没有失败项。</p>
      ) : (
        <ul className="diag-list" aria-label="违例定位">
          {model.jumps.map((jump) => {
            const target = jump.topoRef ?? jump.nodeId;
            const canJump = Boolean(onJump) && target !== null;
            return (
              <li key={`${jump.layer}-${jump.code}-${target ?? "none"}`}>
                <button
                  type="button"
                  className="diag-item diag-error"
                  disabled={!canJump}
                  onClick={() => {
                    if (canJump) onJump?.(jump);
                  }}
                >
                  <div className="diag-item-top">
                    <span className="diag-code">{jump.code}</span>
                    <span className="diag-loc">{target ?? jump.layer}</span>
                  </div>
                  <div className="diag-msg">{jump.message}</div>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
