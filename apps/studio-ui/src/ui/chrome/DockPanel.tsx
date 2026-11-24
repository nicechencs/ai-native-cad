import { useRef, type ReactNode } from "react";
import { WorkbenchTabs } from "../../components/WorkbenchTabs.tsx";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

export interface DockTabItem<T extends string> {
  readonly id: T;
  readonly label: string;
  readonly icon: CadIconName;
  readonly badge?: string | number;
  readonly accent?: "ai" | "error";
}

export function ActivityBar<T extends string>({
  items,
  activeId,
  panelOpen,
  onSelect,
  side = "left",
  ariaLabel = "工作区模块导航",
}: {
  readonly items: readonly DockTabItem<T>[];
  readonly activeId: T;
  readonly panelOpen: boolean;
  readonly onSelect: (id: T) => void;
  readonly side?: "left" | "right";
  readonly ariaLabel?: string;
}) {
  return (
    <aside
      className={`studio-activitybar${side === "right" ? " studio-right-rail" : ""}`}
      aria-label={ariaLabel}
    >
      {items.map((item) => {
        const active = panelOpen && activeId === item.id;
        return (
          <button
            key={item.id}
            type="button"
            className={`activity-btn${active ? " is-active" : ""}${item.accent === "ai" ? " is-ai" : ""}`}
            aria-pressed={active}
            title={item.label}
            onClick={() => onSelect(item.id)}
          >
            <CadIcon name={item.icon} size={17} />
            <span className="activity-label">{item.label}</span>
            {item.badge !== undefined && item.badge !== 0 && (
              <span className={`activity-badge${item.accent === "error" ? " is-error" : ""}`}>
                {item.badge}
              </span>
            )}
          </button>
        );
      })}
    </aside>
  );
}

export function DockPanel<T extends string>({
  side,
  open,
  pinned = true,
  onTogglePin,
  keepMounted = false,
  width,
  tabs,
  activeTab,
  onSelectTab,
  onResize,
  onClose,
  children,
}: {
  readonly side: "left" | "right";
  readonly open: boolean;
  readonly pinned?: boolean;
  readonly onTogglePin?: () => void;
  readonly keepMounted?: boolean;
  readonly width: number;
  readonly tabs: readonly DockTabItem<T>[];
  readonly activeTab: T;
  readonly onSelectTab: (tab: T) => void;
  readonly onResize: (nextWidth: number) => void;
  readonly onClose: () => void;
  readonly children: ReactNode;
}) {
  const dragStartRef = useRef<{ readonly x: number; readonly width: number } | null>(null);

  if (!open && !keepMounted) return null;

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    dragStartRef.current = { x: event.clientX, width };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = dragStartRef.current;
    if (!start) return;
    const delta = side === "left" ? event.clientX - start.x : start.x - event.clientX;
    onResize(start.width + delta);
  };

  const handlePointerUp = () => {
    dragStartRef.current = null;
  };

  return (
    <aside
      className={`studio-dock studio-dock-${side}${!pinned ? " is-floating" : ""}`}
      hidden={!open}
      style={{ width, display: open ? undefined : "none" }}
      aria-label={side === "left" ? "左侧停靠面板" : "右侧停靠面板"}
    >
      <div className="dock-header">
        {side === "left" ? (
          (() => {
            const activeItem = tabs.find((t) => t.id === activeTab) ?? tabs[0];
            return (
              <div className="dock-title-bar">
                {activeItem && <CadIcon name={activeItem.icon} size={14} />}
                <span className="dock-title-text">{activeItem?.label ?? "项目导航"}</span>
                {activeItem?.badge !== undefined && activeItem.badge !== 0 && (
                  <span className={`dock-tab-badge${activeItem.accent === "error" ? " is-error" : ""}`}>
                    {activeItem.badge}
                  </span>
                )}
              </div>
            );
          })()
        ) : (
          <WorkbenchTabs
            prefix={side}
            label="AI 副驾、属性检查器与导出"
            tabs={tabs}
            active={activeTab}
            onSelect={onSelectTab}
            className="dock-tabs"
            tabClassName={(tab, active) => `dock-tab${active ? " is-active" : ""}${tab.accent === "ai" ? " is-ai" : ""}`}
            renderTab={(tab) => (
              <>
                <CadIcon name={tab.icon} size={14} />
                <span>{tab.label}</span>
                {tab.badge !== undefined && tab.badge !== 0 && (
                  <span className={`dock-tab-badge${tab.accent === "error" ? " is-error" : ""}`}>
                    {tab.badge}
                  </span>
                )}
              </>
            )}
          />
        )}
        <div className="dock-header-actions">
          {onTogglePin && (
            <button
              type="button"
              className={`icon-ghost-btn${pinned ? " is-active" : ""}`}
              onClick={onTogglePin}
              title={pinned ? "取消钉选（切换为悬浮自动隐藏模式）" : "钉选到主工作区"}
              aria-label={pinned ? "取消钉选" : "钉选面板"}
              aria-pressed={pinned}
            >
              <CadIcon name={pinned ? "pin" : "pinOff"} size={13} />
            </button>
          )}
          <button
            type="button"
            className="icon-ghost-btn"
            onClick={onClose}
            title="收起面板"
            aria-label="收起面板"
          >
            <CadIcon name="close" size={14} />
          </button>
        </div>
      </div>

      <div className="dock-body">{children}</div>

      <div
        className={`dock-resizer dock-resizer-${side}`}
        role="separator"
        aria-orientation="vertical"
        aria-label="调整面板宽度"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
      />
    </aside>
  );
}
