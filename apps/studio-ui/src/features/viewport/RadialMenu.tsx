import {
  RADIAL_MENU_SLOTS,
  type RadialDirection,
} from "../../core/viewport/navigationPreset.ts";
import { CadIcon, type CadIconName } from "../../ui/icons/CadIcons.tsx";

export interface RadialMenuProps {
  readonly origin: { readonly x: number; readonly y: number } | null;
  readonly activeDirection: RadialDirection | null;
}

const RING_RADIUS = 78;

/**
 * Siemens NX style 8-way Radial Gesture Menu (Swish Menu).
 * Appears around the cursor when holding Right Mouse Button and dragging outward.
 */
export function RadialMenu({ origin, activeDirection }: RadialMenuProps) {
  if (!origin) return null;

  const activeSlot = RADIAL_MENU_SLOTS.find((slot) => slot.direction === activeDirection) ?? null;

  return (
    <div
      className="nx-radial-menu"
      role="menu"
      aria-label="NX 8 向径向手势菜单"
      style={{ left: origin.x, top: origin.y }}
    >
      <div className="nx-radial-center">
        <span>{activeSlot ? activeSlot.label : "划动选择"}</span>
      </div>
      {RADIAL_MENU_SLOTS.map((slot) => {
        const rad = (slot.angleDeg * Math.PI) / 180;
        const offsetX = Math.cos(rad) * RING_RADIUS;
        const offsetY = Math.sin(rad) * RING_RADIUS;
        const isActive = slot.direction === activeDirection;
        return (
          <div
            key={slot.direction}
            className={`nx-radial-slot${isActive ? " is-active" : ""}${slot.accent === "ai" ? " is-ai" : ""}`}
            style={{ transform: `translate(${offsetX.toFixed(1)}px, ${offsetY.toFixed(1)}px)` }}
          >
            <CadIcon name={slot.icon as CadIconName} size={14} />
            <span>{slot.label}</span>
          </div>
        );
      })}
    </div>
  );
}
