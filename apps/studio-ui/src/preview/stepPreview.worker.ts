import wasmUrl from "occt-import-js/dist/occt-import-js.wasm?url";
import { meshesFromStepBytes } from "./readOcctStep.ts";
import type { OcctPreviewMesh } from "./stepPreview.ts";

interface StepPreviewRequest {
  readonly id: number;
  readonly bytes: ArrayBuffer;
}

type StepPreviewResponse =
  | { readonly id: number; readonly ok: true; readonly meshes: readonly OcctPreviewMesh[] }
  | { readonly id: number; readonly ok: false; readonly error: string };

self.onmessage = (event: MessageEvent<StepPreviewRequest>) => {
  const { id, bytes } = event.data;
  void meshesFromStepBytes(bytes, wasmUrl).then(
    (meshes) => {
      const response: StepPreviewResponse = { id, ok: true, meshes };
      self.postMessage(response);
    },
    (err: unknown) => {
      const response: StepPreviewResponse = {
        id,
        ok: false,
        error: err instanceof Error ? err.message : "浏览器读取 STEP 失败",
      };
      self.postMessage(response);
    },
  );
};
