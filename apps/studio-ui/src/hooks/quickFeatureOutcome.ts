import type { NotificationLevel } from "../core/notifications/notificationStore.ts";

export interface QuickFeatureNotice {
  readonly level: NotificationLevel;
  readonly title: string;
  readonly source: "快速建模";
}

/**
 * A quick feature is only a success after the kernel accepts the edited
 * source. Submitting the text edit is still pending.
 */
export function quickFeatureNotice(
  phase: "submitted" | "compiled",
  summary: string,
  diagnosticsStatus: "ok" | "error" | undefined,
): QuickFeatureNotice | null {
  if (phase !== "compiled" || diagnosticsStatus !== "ok" || summary.trim() === "") return null;
  return { level: "success", title: summary, source: "快速建模" };
}

export function geometryAvailabilityBadge(input: {
  readonly pending: boolean;
  readonly faceCount: number;
  readonly diagnosticsStatus: "ok" | "error";
  readonly watertight: boolean;
}): string {
  if (input.pending) return "正在编译";
  if (input.faceCount <= 0) return "暂无可用模型";
  if (input.diagnosticsStatus === "error") return "特征未生效";
  return input.watertight ? "L0 水密流形" : "未通过水密检查";
}

export function exportBlockedDetail(input: {
  readonly pending: boolean;
  readonly faceCount: number;
  readonly diagnosticsStatus: "ok" | "error";
  /** True only when the mesh and metrics on screen are the last accepted solid. */
  readonly showingLastAccepted?: boolean;
}): string | null {
  if (input.pending) return "正在编译当前源码。";
  if (input.diagnosticsStatus === "error" && input.showingLastAccepted && input.faceCount > 0) {
    return "当前特征未生效，显示的是上一份有效实体。";
  }
  if (input.faceCount <= 0) return "当前源码没有可导出的网格。";
  if (input.diagnosticsStatus === "error") return "当前特征未生效。";
  return null;
}
