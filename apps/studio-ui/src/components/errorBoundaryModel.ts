import { createTraceId } from "../utils/trace.ts";

export interface RenderErrorEvent {
  readonly event: "ui.error_boundary";
  readonly trace_id: string;
  readonly message: string;
  readonly name: string;
  readonly stack: string | null;
  readonly component_stack: string | null;
}

export interface ErrorBoundaryState {
  readonly error: Error | null;
  readonly traceId: string | null;
  readonly resetKey: number;
}

export function initialErrorBoundaryState(): ErrorBoundaryState {
  return { error: null, traceId: null, resetKey: 0 };
}

export function deriveErrorBoundaryState(error: unknown): { readonly error: Error; readonly traceId: string } {
  const normalized = error instanceof Error ? error : new Error(typeof error === "string" ? error : "工作台界面渲染失败");
  return { error: normalized, traceId: createTraceId() };
}

export function formatRenderErrorEvent(
  traceId: string,
  error: unknown,
  componentStack: string | null | undefined,
): RenderErrorEvent {
  const normalized = error instanceof Error ? error : new Error(String(error));
  return {
    event: "ui.error_boundary",
    trace_id: traceId,
    message: normalized.message,
    name: normalized.name || "Error",
    stack: normalized.stack ?? null,
    component_stack: componentStack ?? null,
  };
}

/** Clear the captured error and render the same tree again. */
export function retryErrorBoundaryState(state: ErrorBoundaryState): ErrorBoundaryState {
  return { ...state, error: null, traceId: null };
}

/** Clear the error and remount the tree so workbench state starts over. */
export function resetErrorBoundaryState(state: ErrorBoundaryState): ErrorBoundaryState {
  return { error: null, traceId: null, resetKey: state.resetKey + 1 };
}
