import { useEffect, useState } from "react";
import { loadWorkbenchUiPrefs, updateWorkbenchUiPrefs } from "../../core/layout/workbenchPrefsStore.ts";
import type { NotificationItem, NotificationLevel } from "../../core/notifications/notificationStore.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";
import { TraceIdChip } from "../../components/TraceIdChip.tsx";

function levelIcon(level: NotificationLevel): CadIconName {
  switch (level) {
    case "success":
      return "check";
    case "warning":
      return "warning";
    case "error":
      return "error";
    default:
      return "info";
  }
}

function formatTime(timestamp: number): string {
  const date = new Date(timestamp);
  return date.toLocaleTimeString("zh-CN", { hour12: false });
}

export function ToastStack({
  toasts,
  sourceRevision,
  onDismiss,
  onExecuteCommand,
}: {
  readonly toasts: readonly NotificationItem[];
  readonly sourceRevision: number;
  readonly onDismiss: (id: string) => void;
  readonly onExecuteCommand: (commandId: string, sourceRevision?: number) => void;
}) {
  const [pausedId, setPausedId] = useState<string | null>(null);

  useEffect(() => {
    const timers: number[] = [];
    for (const item of toasts) {
      if (item.persistent || item.id === pausedId) continue;
      const timer = window.setTimeout(() => onDismiss(item.id), 3600);
      timers.push(timer);
    }
    return () => {
      for (const timer of timers) window.clearTimeout(timer);
    };
  }, [toasts, pausedId, onDismiss]);

  if (toasts.length === 0) return null;

  return (
    <div className="toast-stack" role="region" aria-label="系统提示">
      {toasts.map((item) => (
        <div
          key={item.id}
          className={`toast-card toast-${item.level}`}
          role={item.level === "error" ? "alert" : "status"}
          onMouseEnter={() => setPausedId(item.id)}
          onMouseLeave={() => setPausedId((cur) => (cur === item.id ? null : cur))}
        >
          <span className="toast-icon">
            <CadIcon name={levelIcon(item.level)} size={15} />
          </span>
          <div className="toast-body">
            <div className="toast-title">{item.title}</div>
            {item.detail && <div className="toast-detail">{item.detail}</div>}
            {item.traceId && <TraceIdChip traceId={item.traceId} />}
            {item.action && (item.action.sourceRevision === undefined || item.action.sourceRevision === sourceRevision) && (
              <button
                type="button"
                className="toast-action-btn"
                onClick={() => {
                  if (item.action?.commandId) onExecuteCommand(item.action.commandId, item.action.sourceRevision);
                  item.action?.onClick?.();
                  onDismiss(item.id);
                }}
              >
                {item.action.label}
              </button>
            )}
          </div>
          <button
            type="button"
            className="icon-ghost-btn"
            aria-label="关闭提示"
            onClick={() => onDismiss(item.id)}
          >
            <CadIcon name="close" size={13} />
          </button>
        </div>
      ))}
    </div>
  );
}

export function NotificationDrawer({
  open,
  logs,
  onClose,
  onClear,
}: {
  readonly open: boolean;
  readonly logs: readonly NotificationItem[];
  readonly onClose: () => void;
  readonly onClear: () => void;
}) {
  const [filter, setFilterState] = useState<"all" | NotificationLevel>(() => loadWorkbenchUiPrefs().notificationFilter);
  const setFilter = (next: "all" | NotificationLevel) => {
    setFilterState(next);
    updateWorkbenchUiPrefs({ notificationFilter: next });
  };

  if (!open) return null;

  const visible = filter === "all" ? logs : logs.filter((item) => item.level === filter);

  return (
    <aside className="notification-drawer" role="dialog" aria-label="通知中心与事件日志">
      <div className="drawer-header">
        <div className="drawer-title">
          <CadIcon name="bell" size={15} />
          <span>通知与事件日志</span>
          <span className="count-pill">{logs.length}</span>
        </div>
        <div className="drawer-actions">
          <button type="button" className="icon-ghost-btn" onClick={onClear} title="清空日志">
            <CadIcon name="trash" size={14} />
          </button>
          <button type="button" className="icon-ghost-btn" onClick={onClose} title="关闭">
            <CadIcon name="close" size={14} />
          </button>
        </div>
      </div>

      <div className="drawer-filters">
        {(
          [
            ["all", "全部"],
            ["error", "错误"],
            ["warning", "警告"],
            ["success", "成功"],
            ["info", "信息"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            className={`filter-chip${filter === id ? " is-active" : ""}`}
            onClick={() => setFilter(id)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="drawer-list">
        {visible.length === 0 ? (
          <div className="drawer-empty">暂无事件记录</div>
        ) : (
          visible.map((item) => (
            <div key={item.id} className={`log-entry log-${item.level}`}>
              <span className="log-icon">
                <CadIcon name={levelIcon(item.level)} size={14} />
              </span>
              <div className="log-content">
                <div className="log-header">
                  <strong>{item.title}</strong>
                  <span className="log-time tabular-nums">{formatTime(item.timestamp)}</span>
                </div>
                {item.detail && <div className="log-detail">{item.detail}</div>}
                {item.traceId && <TraceIdChip traceId={item.traceId} />}
                {item.source && <span className="log-source">{item.source}</span>}
              </div>
            </div>
          ))
        )}
      </div>
    </aside>
  );
}
