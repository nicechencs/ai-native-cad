import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { handleConfirmBackdropMouseDown } from "./confirmDialogPolicy.ts";
import { COMMAND_LINE_SUGGESTIONS } from "../../core/commands/commandLineParser.ts";
import type { CommandDefinition } from "../../core/commands/commandRegistry.ts";
import { NAVIGATION_PRESETS, RADIAL_MENU_SLOTS } from "../../core/viewport/navigationPreset.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

export interface ContextMenuItem {
  readonly id: string;
  readonly label: string;
  readonly icon?: CadIconName;
  readonly shortcut?: string;
  readonly disabled?: boolean;
  readonly danger?: boolean;
  readonly separatorBefore?: boolean;
  readonly onSelect: () => void;
}

export function ContextMenu({
  position,
  title,
  items,
  onClose,
}: {
  readonly position: { readonly x: number; readonly y: number } | null;
  readonly title?: string;
  readonly items: readonly ContextMenuItem[];
  readonly onClose: () => void;
}) {
  useEffect(() => {
    if (!position) return;
    const handlePointerDown = () => onClose();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [position, onClose]);

  if (!position) return null;

  const clampedX = typeof window !== "undefined" ? Math.min(position.x, window.innerWidth - 230) : position.x;
  const clampedY = typeof window !== "undefined" ? Math.min(position.y, window.innerHeight - 280) : position.y;

  return (
    <div
      className="studio-context-menu"
      role="menu"
      style={{ left: Math.max(8, clampedX), top: Math.max(8, clampedY) }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      {title && <div className="context-menu-title">{title}</div>}
      {items.map((item) => (
        <div key={item.id}>
          {item.separatorBefore && <div className="dropdown-separator" role="separator" />}
          <button
            type="button"
            role="menuitem"
            disabled={item.disabled}
            className={`dropdown-item${item.danger ? " is-danger" : ""}`}
            onClick={() => {
              onClose();
              item.onSelect();
            }}
          >
            <span className="dropdown-item-check">
              {item.icon && <CadIcon name={item.icon} size={14} />}
            </span>
            <span className="dropdown-item-label">{item.label}</span>
            {item.shortcut && <kbd className="shortcut-kbd">{item.shortcut}</kbd>}
          </button>
        </div>
      ))}
    </div>
  );
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = "确认",
  cancelLabel = "取消",
  onConfirm,
  onCancel,
}: {
  readonly open: boolean;
  readonly title: string;
  readonly description: string;
  readonly confirmLabel?: string;
  readonly cancelLabel?: string;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
}) {
  const openedAtRef = useRef(0);
  useEffect(() => {
    if (!open) return;
    openedAtRef.current = typeof performance !== "undefined" ? performance.now() : Date.now();
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [open, onCancel]);

  if (!open) return null;
  if (typeof document === "undefined") return null;

  return createPortal(
    <div
      className="modal-backdrop confirm-dialog-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onMouseDown={(event) => {
        const now = typeof performance !== "undefined" ? performance.now() : Date.now();
        if (
          handleConfirmBackdropMouseDown({
            openedAtMs: openedAtRef.current,
            eventTimeMs: now,
            target: event.target,
            currentTarget: event.currentTarget,
          }) === "cancel"
        ) {
          onCancel();
        }
      }}
    >
      <div className="confirm-dialog">
        <div className="modal-header">
          <strong>{title}</strong>
          <button type="button" className="icon-ghost-btn" onClick={onCancel} aria-label="关闭">
            <CadIcon name="close" size={14} />
          </button>
        </div>
        <p className="confirm-description">{description}</p>
        <div className="confirm-actions">
          <button type="button" className="studio-btn" onClick={onCancel}>
            {cancelLabel}
          </button>
          <button type="button" className="studio-btn studio-btn-primary" onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

export function ShortcutsModal({
  open,
  commands,
  onClose,
}: {
  readonly open: boolean;
  readonly commands: readonly CommandDefinition[];
  readonly onClose: () => void;
}) {
  useEffect(() => {
    if (!open) return;
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [open, onClose]);

  if (!open) return null;

  const withShortcuts = commands.filter((cmd) => cmd.shortcut);

  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="快捷键与命令行速查"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="shortcuts-modal">
        <div className="modal-header">
          <div className="modal-header-title">
            <CadIcon name="keyboard" size={16} />
            <strong>快捷键与命令行速查</strong>
          </div>
          <button type="button" className="icon-ghost-btn" onClick={onClose} aria-label="关闭">
            <CadIcon name="close" size={15} />
          </button>
        </div>
        <div className="shortcuts-grid">
          <section className="shortcuts-section">
            <h4>鼠标导航方案（默认 NX / Creo，可在状态栏切换）</h4>
            <div className="shortcuts-list">
              {NAVIGATION_PRESETS.map((preset) => (
                <div key={preset.id} className="shortcut-row shortcut-row-stacked">
                  <strong>
                    {preset.label}
                    {preset.id === "nx-creo" ? " · 默认" : ""}
                  </strong>
                  <span className="shortcut-desc">
                    旋转: {preset.orbitHint} · 平移: {preset.panHint} · 缩放: {preset.zoomHint} · 确认: {preset.confirmHint}
                  </span>
                </div>
              ))}
              <div className="shortcut-row shortcut-row-stacked">
                <strong>右键长按 8 向径向手势 (Radial Menu)</strong>
                <span className="shortcut-desc">
                  {RADIAL_MENU_SLOTS.map((slot) => `${slot.direction}:${slot.label}`).join(" · ")}
                </span>
              </div>
            </div>
            <h4 style={{ marginTop: 14 }}>底部命令行语法</h4>
            <div className="shortcuts-list">
              {COMMAND_LINE_SUGGESTIONS.map((item) => (
                <div key={item.syntax} className="shortcut-row">
                  <code>{item.syntax}</code>
                  <span className="shortcut-desc">{item.description}</span>
                </div>
              ))}
            </div>
          </section>
          <section className="shortcuts-section">
            <h4>键盘快捷键</h4>
            <div className="shortcuts-list">
              {withShortcuts.map((cmd) => (
                <div key={cmd.id} className="shortcut-row">
                  <span>{cmd.title}</span>
                  <kbd className="shortcut-kbd">{cmd.shortcut}</kbd>
                </div>
              ))}
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
