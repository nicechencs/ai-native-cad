import type { SelectionTarget } from "../../types/scene.ts";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";

export interface QuickPickCandidate {
  readonly id: string;
  readonly target: NonNullable<SelectionTarget>;
  readonly label: string;
  readonly detail: string;
}

export interface QuickPickPopoverProps {
  readonly position: { readonly x: number; readonly y: number } | null;
  readonly candidates: readonly QuickPickCandidate[];
  readonly activeIndex: number;
  readonly onHoverCandidate: (candidate: QuickPickCandidate, index: number) => void;
  readonly onPickCandidate: (candidate: QuickPickCandidate) => void;
  readonly onClose: () => void;
}

/**
 * Siemens NX QuickPick / PTC Creo Query Select (Pick From List) popover.
 * Lets engineers cleanly disambiguate overlapping faces and edges under the cursor via Tab or click.
 */
export function QuickPickPopover({
  position,
  candidates,
  activeIndex,
  onHoverCandidate,
  onPickCandidate,
  onClose,
}: QuickPickPopoverProps) {
  if (!position || candidates.length < 2) return null;

  return (
    <div
      className="quickpick-popover"
      role="listbox"
      aria-label="QuickPick 重叠几何候选列表"
      style={{ left: position.x + 14, top: position.y + 14 }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <div className="quickpick-header">
        <span>QuickPick ({candidates.length}) · Tab 切换</span>
        <button type="button" className="icon-ghost-btn" aria-label="关闭 QuickPick" onClick={onClose}>
          <CadIcon name="close" size={11} />
        </button>
      </div>
      <ul className="quickpick-list">
        {candidates.map((cand, index) => {
          const active = index === activeIndex;
          return (
            <li key={cand.id}>
              <button
                type="button"
                role="option"
                aria-selected={active}
                className={`quickpick-item${active ? " is-active" : ""}`}
                onMouseEnter={() => onHoverCandidate(cand, index)}
                onClick={() => onPickCandidate(cand)}
              >
                <CadIcon name={cand.target.kind === "face" ? "filterFace" : "filterEdge"} size={12} />
                <span className="quickpick-item-label">{cand.label}</span>
                <span className="quickpick-item-detail">{cand.detail}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
