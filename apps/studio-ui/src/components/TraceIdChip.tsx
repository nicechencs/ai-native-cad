import { useEffect, useRef, useState } from "react";
import { copyTraceId, shortTraceId } from "../utils/trace.ts";

export function TraceIdChip({ traceId }: { readonly traceId: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  return (
    <span className="trace-chip">
      <code className="trace-chip-id" title={traceId}>{shortTraceId(traceId)}</code>
      <button
        type="button"
        className="trace-chip-copy"
        onClick={() => {
          void copyTraceId(traceId).then((ok) => {
            if (!ok) return;
            setCopied(true);
            if (timer.current !== null) window.clearTimeout(timer.current);
            timer.current = window.setTimeout(() => setCopied(false), 1600);
          });
        }}
      >
        {copied ? "已复制" : "复制 trace_id"}
      </button>
    </span>
  );
}
