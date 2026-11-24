export type NotificationLevel = "info" | "success" | "warning" | "error";

export interface NotificationAction {
  readonly label: string;
  readonly commandId?: string;
  readonly sourceRevision?: number;
  readonly onClick?: () => void;
}

export interface NotificationItem {
  readonly id: string;
  readonly level: NotificationLevel;
  readonly title: string;
  readonly detail?: string;
  readonly source?: string;
  readonly timestamp: number;
  readonly action?: NotificationAction;
  readonly persistent?: boolean;
  readonly read?: boolean;
  readonly traceId?: string;
}

export interface NotificationState {
  readonly toasts: readonly NotificationItem[];
  readonly logs: readonly NotificationItem[];
  readonly drawerOpen: boolean;
}

const MAX_VISIBLE_TOASTS = 3;
const MAX_LOG_ENTRIES = 100;

let nextNotificationSeq = 1;

export function createNotificationId(): string {
  return `ntf_${Date.now()}_${nextNotificationSeq++}`;
}

export function createInitialNotificationState(): NotificationState {
  return {
    toasts: [],
    logs: [],
    drawerOpen: false,
  };
}

export interface PushNotificationInput {
  readonly level: NotificationLevel;
  readonly title: string;
  readonly detail?: string;
  readonly source?: string;
  readonly action?: NotificationAction;
  readonly persistent?: boolean;
  readonly showToast?: boolean;
  readonly traceId?: string;
}

export function pushNotification(
  state: NotificationState,
  input: PushNotificationInput,
  timestamp = Date.now(),
): { readonly state: NotificationState; readonly item: NotificationItem } {
  const item: NotificationItem = {
    id: createNotificationId(),
    level: input.level,
    title: input.title,
    detail: input.detail,
    source: input.source,
    timestamp,
    action: input.action,
    persistent: input.persistent ?? input.level === "error",
    read: state.drawerOpen,
    ...(input.traceId ? { traceId: input.traceId } : {}),
  };

  const nextLogs = [item, ...state.logs].slice(0, MAX_LOG_ENTRIES);
  const shouldToast = input.showToast !== false;
  const nextToasts = shouldToast
    ? [item, ...state.toasts].slice(0, MAX_VISIBLE_TOASTS)
    : state.toasts;

  return {
    item,
    state: {
      ...state,
      toasts: nextToasts,
      logs: nextLogs,
    },
  };
}

export function dismissToast(state: NotificationState, id: string): NotificationState {
  return {
    ...state,
    toasts: state.toasts.filter((item) => item.id !== id),
  };
}

export function markAllLogsRead(state: NotificationState): NotificationState {
  return {
    ...state,
    logs: state.logs.map((item) => (item.read ? item : { ...item, read: true })),
  };
}

export function clearNotificationLogs(state: NotificationState): NotificationState {
  return {
    ...state,
    logs: [],
    toasts: [],
  };
}

export function countUnreadNotifications(state: NotificationState): number {
  return state.logs.reduce((acc, item) => acc + (item.read ? 0 : 1), 0);
}
