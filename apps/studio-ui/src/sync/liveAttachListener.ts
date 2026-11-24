/**
 * Turns a desktop Live-Attach review event into Change Preview state.
 *
 * The desktop host emits `live-attach://preview-ready` after an external agent
 * calls `cad_branch_control` with `action: "request_review"`. In a browser tab
 * there is no Tauri runtime, so subscribing does nothing.
 */
import {
  emptyVerificationReport,
  type ChangePreviewInput,
  type FeatureDiff,
  type ParameterDiff,
} from "../core/session/engineeringSession.ts";

export const LIVE_ATTACH_PREVIEW_READY_EVENT = "live-attach://preview-ready";

export interface LiveAttachBranchDiff {
  readonly branch: string;
  readonly base: string;
  readonly added_parameters: readonly string[];
  readonly removed_parameters: readonly string[];
  readonly changed_parameters: readonly string[];
  readonly parameter_diffs?: readonly ParameterDiff[];
  readonly parameter_values?: Readonly<Record<string, { readonly before?: unknown; readonly after?: unknown }>>;
  readonly added_nodes: readonly string[];
  readonly removed_nodes: readonly string[];
  readonly changed_nodes: readonly string[];
  readonly volume_mm3: { readonly before: number; readonly after: number };
}

export interface LiveAttachPendingReview {
  readonly branch: string;
  readonly revision: number;
  readonly diff: LiveAttachBranchDiff;
  readonly plan_summary?: string;
}

export interface LiveAttachEvent {
  readonly payload: unknown;
}

export type LiveAttachUnlisten = () => void;

/** Tauri `event.listen`, or a test double with the same shape. */
export type LiveAttachListen = (
  event: string,
  handler: (event: LiveAttachEvent) => void,
) => Promise<LiveAttachUnlisten> | LiveAttachUnlisten;

interface TauriPreviewHost {
  __TAURI__?: {
    event?: {
      listen?: LiveAttachListen;
    };
  };
  __TAURI_INTERNALS__?: {
    invoke?: (cmd: string, args?: object) => Promise<unknown>;
    transformCallback?: (callback: (event: LiveAttachEvent) => void, once?: boolean) => number;
  };
}

/** Parse a `PendingReview` event payload. Returns null when the payload is incomplete. */
export function parseLiveAttachPendingReview(payload: unknown): LiveAttachPendingReview | null {
  if (!isRecord(payload)) return null;
  const branch = readText(payload.branch);
  const revision = readRevision(payload.revision);
  const diff = parseDiff(payload.diff);
  if (!branch || revision === null || !diff) return null;
  const planSummary = readText(payload.plan_summary) ?? readText(payload.explanation);
  return {
    branch,
    revision,
    diff,
    ...(planSummary ? { plan_summary: planSummary } : {}),
  };
}

/** Map the desktop diff onto the Change Preview card model. */
export function changePreviewFromPendingReview(review: LiveAttachPendingReview): ChangePreviewInput {
  const diff = review.diff;
  const base = diff.base || "main";
  return {
    previewBranch: review.branch,
    planSummary: review.plan_summary ?? `相对 ${base} 审阅 ${review.branch}（修订 ${review.revision}）`,
    parameterDiffs: parameterDiffsOf(diff),
    featureDiffs: featureDiffsOf(diff),
    volumeDeltaMm3: diff.volume_mm3.after - diff.volume_mm3.before,
    verificationReport: emptyVerificationReport(),
  };
}

/** Parse an event payload and build the preview input. Malformed events return null. */
export function changePreviewFromLiveAttachPayload(payload: unknown): ChangePreviewInput | null {
  const review = parseLiveAttachPendingReview(payload);
  if (!review) return null;
  const preview = changePreviewFromPendingReview(review);
  if (!Number.isFinite(preview.volumeDeltaMm3 ?? 0)) return null;
  return preview;
}

/**
 * Subscribe to `live-attach://preview-ready`.
 * Pass `listen` in tests. Outside Tauri, this returns a no-op unsubscribe.
 */
export function subscribeLiveAttachPreviewReady(
  onPreview: (preview: ChangePreviewInput) => void,
  listen: LiveAttachListen | null = defaultLiveAttachListen(),
): LiveAttachUnlisten {
  if (!listen) return () => {};
  let unlisten: LiveAttachUnlisten | undefined;
  let closed = false;
  const deliver = (event: LiveAttachEvent) => {
    if (closed) return;
    const preview = changePreviewFromLiveAttachPayload(event.payload);
    if (preview) onPreview(preview);
  };
  try {
    const pending = listen(LIVE_ATTACH_PREVIEW_READY_EVENT, deliver);
    if (isPromise(pending)) {
      void pending.then(
        (stop) => {
          if (closed) stop();
          else unlisten = stop;
        },
        () => {},
      );
    } else {
      unlisten = pending;
    }
  } catch {
    return () => {};
  }
  return () => {
    closed = true;
    try {
      unlisten?.();
    } catch {
      // The webview may already have dropped the listener.
    }
  };
}

function defaultLiveAttachListen(): LiveAttachListen | null {
  try {
    const host = tauriHost();
    const eventApi = host?.__TAURI__?.event;
    if (eventApi && typeof eventApi.listen === "function") {
      const listen = eventApi.listen;
      return (event, handler) => listen.call(eventApi, event, handler);
    }
    const internals = host?.__TAURI_INTERNALS__;
    if (typeof internals?.invoke === "function" && typeof internals.transformCallback === "function") {
      return (event, handler) => listenWithInternals(internals, event, handler);
    }
    return null;
  } catch {
    return null;
  }
}

function listenWithInternals(
  internals: NonNullable<TauriPreviewHost["__TAURI_INTERNALS__"]>,
  event: string,
  handler: (event: LiveAttachEvent) => void,
): Promise<LiveAttachUnlisten> {
  const callbackId = internals.transformCallback!(handler);
  return internals.invoke!("plugin:event|listen", {
    event,
    target: { kind: "Any" },
    handler: callbackId,
  }).then((eventId) => () => {
    void Promise.resolve(internals.invoke!("plugin:event|unlisten", { event, eventId })).catch(() => {});
  });
}

function tauriHost(): TauriPreviewHost | null {
  return (globalThis as { window?: TauriPreviewHost }).window ?? null;
}

function formatParamValue(value: unknown): string | null {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed ? trimmed : null;
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return String(value);
  }
  if (typeof value === "boolean") {
    return value ? "true" : "false";
  }
  if (isRecord(value)) {
    const num = readFinite(value.value);
    const unit = readText(value.unit);
    if (num !== null && unit) return `${num}${unit}`;
    if (num !== null) return String(num);
  }
  return null;
}

function parameterDiffsOf(diff: LiveAttachBranchDiff): ParameterDiff[] {
  const byName = new Map<string, ParameterDiff>();
  if (Array.isArray(diff.parameter_diffs)) {
    for (const entry of diff.parameter_diffs) {
      byName.set(entry.name, entry);
    }
  }
  const valueMap = diff.parameter_values;
  const resolveEntry = (name: string, fallbackBefore: string, fallbackAfter: string): ParameterDiff => {
    const explicit = byName.get(name);
    if (explicit) {
      byName.delete(name);
      return explicit;
    }
    const pair = valueMap?.[name];
    const before = formatParamValue(pair?.before) ?? fallbackBefore;
    const after = formatParamValue(pair?.after) ?? fallbackAfter;
    return { name, before, after };
  };

  const ordered: ParameterDiff[] = [
    ...diff.added_parameters.map((name) => resolveEntry(name, "（无）", "新增")),
    ...diff.removed_parameters.map((name) => resolveEntry(name, "存在", "删除")),
    ...diff.changed_parameters.map((name) => resolveEntry(name, "（原值未提供）", "（已变更）")),
  ];
  for (const remaining of byName.values()) {
    ordered.push(remaining);
  }
  return ordered;
}

function featureDiffsOf(diff: LiveAttachBranchDiff): FeatureDiff[] {
  return [
    ...diff.added_nodes.map((nodeId) => ({ nodeId, change: "added" as const, summary: "预览分支新增" })),
    ...diff.removed_nodes.map((nodeId) => ({ nodeId, change: "removed" as const, summary: "预览分支删除" })),
    ...diff.changed_nodes.map((nodeId) => ({ nodeId, change: "modified" as const, summary: "预览分支修改" })),
  ];
}

function parseParameterDiffList(value: unknown): readonly ParameterDiff[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const list: ParameterDiff[] = [];
  for (const item of value) {
    if (!isRecord(item)) continue;
    const name = readText(item.name);
    const before = formatParamValue(item.before);
    const after = formatParamValue(item.after);
    if (name && before !== null && after !== null) {
      list.push({ name, before, after });
    }
  }
  return list.length > 0 ? list : undefined;
}

function parseParameterValuesMap(
  value: unknown,
): Readonly<Record<string, { readonly before?: unknown; readonly after?: unknown }>> | undefined {
  if (!isRecord(value)) return undefined;
  const out: Record<string, { readonly before?: unknown; readonly after?: unknown }> = {};
  for (const [key, entry] of Object.entries(value)) {
    const name = key.trim();
    if (!name || !isRecord(entry)) continue;
    out[name] = {
      ...(entry.before !== undefined ? { before: entry.before } : {}),
      ...(entry.after !== undefined ? { after: entry.after } : {}),
    };
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

function parseDiff(value: unknown): LiveAttachBranchDiff | null {
  if (!isRecord(value)) return null;
  const branch = readText(value.branch) ?? "";
  const base = readText(value.base) ?? "main";
  const addedParameters = readStringArray(value.added_parameters);
  const removedParameters = readStringArray(value.removed_parameters);
  const changedParameters = readStringArray(value.changed_parameters);
  const addedNodes = readStringArray(value.added_nodes);
  const removedNodes = readStringArray(value.removed_nodes);
  const changedNodes = readStringArray(value.changed_nodes);
  const volume = isRecord(value.volume_mm3) ? value.volume_mm3 : null;
  const before = volume ? readFinite(volume.before) : null;
  const after = volume ? readFinite(volume.after) : null;
  if (
    !addedParameters || !removedParameters || !changedParameters
    || !addedNodes || !removedNodes || !changedNodes
    || before === null || after === null
  ) {
    return null;
  }
  const parameterDiffs = parseParameterDiffList(value.parameter_diffs);
  const parameterValues = parseParameterValuesMap(value.parameter_values);
  return {
    branch,
    base,
    added_parameters: addedParameters,
    removed_parameters: removedParameters,
    changed_parameters: changedParameters,
    ...(parameterDiffs ? { parameter_diffs: parameterDiffs } : {}),
    ...(parameterValues ? { parameter_values: parameterValues } : {}),
    added_nodes: addedNodes,
    removed_nodes: removedNodes,
    changed_nodes: changedNodes,
    volume_mm3: { before, after },
  };
}

function readStringArray(value: unknown): readonly string[] | null {
  if (!Array.isArray(value)) return null;
  const names: string[] = [];
  for (const item of value) {
    if (typeof item !== "string") return null;
    const name = item.trim();
    if (name) names.push(name);
  }
  return names;
}

function readText(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const text = value.trim();
  return text ? text : null;
}

function readRevision(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) return null;
  return value;
}

function readFinite(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isPromise<T>(value: Promise<T> | T): value is Promise<T> {
  return typeof (value as Promise<T> | undefined)?.then === "function";
}
