/**
 * Confirm-dialog backdrop policy.
 *
 * Native GUI acceptance on 2026-11-24 (frozen pack, Asia/Taipei): after opening
 * a real .aicad, two clicks on the ribbon control 重置演示 produced no visible
 * change, left the title on part.aicad, and wrote no desktop.jsonl reset event.
 * Root cause: file.resetDemo only opens a ConfirmDialog; the actual reset runs
 * only after confirming 恢复演示模型. The dialog uses a full-viewport
 * modal-backdrop whose mousedown handler cancels immediately, so a second click
 * on the same toolbar position (now covered by the backdrop) dismisses the
 * dialog without applying the reset.
 */
export const CONFIRM_BACKDROP_DISMISS_GUARD_MS = 500;

export function shouldDismissConfirmOnBackdrop(openedAtMs: number, eventTimeMs: number): boolean {
  return Number.isFinite(openedAtMs)
    && Number.isFinite(eventTimeMs)
    && eventTimeMs - openedAtMs >= CONFIRM_BACKDROP_DISMISS_GUARD_MS;
}

export type ConfirmBackdropAction = "cancel" | "ignore";

/** Event-level contract used by ConfirmDialog backdrop onMouseDown. */
export function handleConfirmBackdropMouseDown(event: {
  readonly openedAtMs: number;
  readonly eventTimeMs: number;
  readonly target: unknown;
  readonly currentTarget: unknown;
}): ConfirmBackdropAction {
  if (event.target !== event.currentTarget) return "ignore";
  return shouldDismissConfirmOnBackdrop(event.openedAtMs, event.eventTimeMs) ? "cancel" : "ignore";
}
