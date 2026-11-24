/** 16-byte trace id, written as 32 lowercase hex characters. */
const TRACE_BYTES = 16;
const HEX = "0123456789abcdef";

export const TRACE_HEADER = "x-trace-id";
export const TRANSPORT_IPC_FAILED = "R011_IPC_FAILED";

export interface TransportError {
  readonly code: string;
  readonly message: string;
  readonly traceId: string;
  readonly operation?: string;
}

/** A 32-character hex id. Callers also accept a 16-character hex id from the host. */
export function createTraceId(): string {
  const bytes = new Uint8Array(TRACE_BYTES);
  const cryptoApi = globalThis.crypto;
  if (cryptoApi?.getRandomValues) cryptoApi.getRandomValues(bytes);
  else {
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = Math.floor(Math.random() * 256);
  }
  let out = "";
  for (const byte of bytes) out += HEX[byte >> 4] + HEX[byte & 0x0f];
  return out;
}

export function isTraceId(value: string): boolean {
  return /^[0-9a-f]{16}$|^[0-9a-f]{32}$/.test(value);
}

export function normalizeTraceId(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim().toLowerCase();
  return isTraceId(trimmed) ? trimmed : undefined;
}

function traceField(record: Record<string, unknown>): string | undefined {
  return normalizeTraceId(record.trace_id) ?? normalizeTraceId(record.traceId);
}

/** Header `x-trace-id` wins, then `trace_id` / `traceId` on the body, then the id we sent. */
export function readTraceId(headerValue: string | null | undefined, body: unknown, fallback: string): string {
  const header = normalizeTraceId(headerValue);
  if (header) return header;
  if (body && typeof body === "object") {
    const record = body as Record<string, unknown>;
    const direct = traceField(record);
    if (direct) return direct;
    const nested = record.error;
    if (nested && typeof nested === "object") {
      const fromError = traceField(nested as Record<string, unknown>);
      if (fromError) return fromError;
    }
  }
  return normalizeTraceId(fallback) ?? fallback;
}

export function traceIdFrom(value: unknown): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  return traceField(record) ?? (record.error && typeof record.error === "object"
    ? traceField(record.error as Record<string, unknown>)
    : undefined);
}

export function shortTraceId(traceId: string): string {
  const trimmed = traceId.trim();
  if (trimmed.length <= 8) return trimmed;
  return `${trimmed.slice(0, 8)}…`;
}

export function withTraceHeader(init: RequestInit | undefined, traceId: string): RequestInit {
  const headers = new Headers(init?.headers);
  headers.set(TRACE_HEADER, traceId);
  return { ...init, headers };
}

/** Copy a trace id. Uses the async clipboard API, then a hidden textarea. */
export async function copyTraceId(traceId: string): Promise<boolean> {
  const text = traceId.trim();
  if (!text) return false;
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through when the page blocks the async clipboard API.
  }
  try {
    if (typeof document === "undefined") return false;
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "true");
    area.style.position = "fixed";
    area.style.left = "-9999px";
    document.body.appendChild(area);
    area.select();
    const copied = document.execCommand("copy");
    area.remove();
    return copied;
  } catch {
    return false;
  }
}

export function transportCodeFrom(body: unknown): string | undefined {
  if (!body || typeof body !== "object") return undefined;
  const record = body as Record<string, unknown>;
  if (typeof record.code === "string" && record.code.trim()) return record.code.trim();
  const error = record.error;
  if (error && typeof error === "object") {
    const code = (error as { code?: unknown }).code;
    if (typeof code === "string" && code.trim()) return code.trim();
  }
  return undefined;
}

export function transportMessageFrom(body: unknown, fallback: string): string {
  if (body && typeof body === "object") {
    const record = body as Record<string, unknown>;
    const error = record.error;
    if (error && typeof error === "object" && typeof (error as { message?: unknown }).message === "string") {
      const message = (error as { message: string }).message.trim();
      if (message) return message;
    }
    if (typeof error === "string" && error.trim()) return error.trim();
    if (typeof record.message === "string" && record.message.trim()) return record.message.trim();
  }
  if (typeof body === "string" && body.trim()) return body.trim().slice(0, 500);
  return fallback;
}
