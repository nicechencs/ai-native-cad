import type { OcctPreviewMesh } from "./stepPreview.ts";

interface StepPreviewResponse {
  readonly id: number;
  readonly ok: boolean;
  readonly meshes?: readonly OcctPreviewMesh[];
  readonly error?: string;
}

/** Sends the STEP bytes to a worker and drops that worker when the read finishes or is cancelled. */
export function readStepOffThread(bytes: ArrayBuffer, signal: AbortSignal): Promise<readonly OcctPreviewMesh[]> {
  const worker = new Worker(new URL("./stepPreview.worker.ts", import.meta.url), { type: "module" });
  const id = 1;
  return new Promise((resolve, reject) => {
    const finish = (error?: Error, meshes?: readonly OcctPreviewMesh[]) => {
      signal.removeEventListener("abort", onAbort);
      worker.terminate();
      if (error) reject(error);
      else resolve(meshes ?? []);
    };
    const onAbort = () => finish(new DOMException("浏览器读取 STEP 已取消", "AbortError"));
    if (signal.aborted) {
      onAbort();
      return;
    }
    signal.addEventListener("abort", onAbort, { once: true });
    worker.onmessage = (event: MessageEvent<StepPreviewResponse>) => {
      if (event.data.id !== id) return;
      if (!event.data.ok || !event.data.meshes) finish(new Error(event.data.error || "浏览器读取 STEP 失败"));
      else finish(undefined, event.data.meshes);
    };
    worker.onerror = () => finish(new Error("浏览器读取 STEP 的后台线程失败"));
    worker.postMessage({ id, bytes }, [bytes]);
  });
}
