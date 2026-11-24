import type { PushNotificationInput, NotificationAction } from "../core/notifications/notificationStore.ts";
import type { WorkbenchModel } from "../sync/workbenchBackend.ts";

export const WORKBENCH_FILE = "model.kcl";

export type WorkbenchNotify = (input: PushNotificationInput) => void;

export type ApplySourceUpdate = (
  nextSource: string,
  options?: {
    readonly summary?: string;
    readonly recordHistory?: boolean;
    /** File open replaces the document. An in-editor edit does not. */
    readonly replaceDocument?: boolean;
    readonly previewModel?: WorkbenchModel;
  },
) => number | undefined;

export function sourceUndoAction(sourceRevision: number | undefined): NotificationAction | undefined {
  return sourceRevision === undefined ? undefined : { label: "撤销", commandId: "edit.undo", sourceRevision };
}
