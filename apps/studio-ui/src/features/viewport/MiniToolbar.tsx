import type { SelectionTarget } from "../../types/scene.ts";
import { CadIcon, type CadIconName } from "../../ui/icons/CadIcons.tsx";

export interface MiniToolbarProps {
  readonly target: SelectionTarget;
  readonly position: { readonly x: number; readonly y: number } | null;
  readonly selectedCount: number;
  readonly onExecute: (commandId: string) => void;
  readonly onDismiss: () => void;
}

interface MiniAction {
  readonly commandId: string;
  readonly label: string;
  readonly icon: CadIconName;
  readonly accent?: "ai";
}

/**
 * PTC Creo style cursor-adjacent Mini Toolbar.
 * Pops up right next to the clicked face or edge to eliminate mouse travel to the Ribbon.
 */
export function MiniToolbar({
  target,
  position,
  selectedCount,
  onExecute,
  onDismiss,
}: MiniToolbarProps) {
  if (!target || !position) return null;

  const actions: readonly MiniAction[] = target.kind === "face"
    ? [
        { commandId: "model.sketch", label: "草图", icon: "sketch" },
        { commandId: "model.throughHole", label: "打孔", icon: "throughHole" },
        { commandId: "model.directEdit", label: "偏移面", icon: "directEdit" },
        { commandId: "view.normalTo", label: "正视", icon: "normalTo" },
        { commandId: "display.hideSelected", label: "隐藏", icon: "eyeOff" },
        { commandId: "ai.inlinePrompt", label: "AI 修改", icon: "sparkles", accent: "ai" },
      ]
    : [
        { commandId: "model.filletEdges", label: "圆角", icon: "fillet" },
        { commandId: "model.chamferEdges", label: "倒角", icon: "chamfer" },
        { commandId: "inspect.measureCircle", label: "测距", icon: "measureCircle" },
        { commandId: "source.focusSelection", label: "源码", icon: "code" },
        { commandId: "ai.inlinePrompt", label: "AI 修改", icon: "sparkles", accent: "ai" },
      ];

  const clampedX = Math.max(12, position.x);
  const clampedY = Math.max(54, position.y - 44);

  return (
    <div
      className="viewport-mini-toolbar"
      role="toolbar"
      aria-label="光标快捷工具条 (Mini Toolbar)"
      style={{ left: clampedX, top: clampedY }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <span className="mini-tb-badge">
        {target.kind === "face" ? "面" : "边"}
        {selectedCount > 1 ? ` ×${selectedCount}` : ""}
      </span>
      {actions.map((action) => (
        <button
          key={action.commandId}
          type="button"
          className={`mini-tb-btn${action.accent === "ai" ? " is-ai" : ""}`}
          title={action.label}
          onClick={() => {
            onDismiss();
            onExecute(action.commandId);
          }}
        >
          <CadIcon name={action.icon} size={13} />
          <span>{action.label}</span>
        </button>
      ))}
    </div>
  );
}
