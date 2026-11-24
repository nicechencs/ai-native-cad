import occtimportjs from "occt-import-js";
import type { OcctPreviewMesh } from "./stepPreview.ts";

let loading: Promise<Awaited<ReturnType<typeof occtimportjs>>> | null = null;

function loadOcct(wasmUrl?: string) {
  loading ??= occtimportjs(wasmUrl
    ? { locateFile: (file) => file.endsWith(".wasm") ? wasmUrl : file }
    : undefined).catch((err: unknown) => {
      loading = null;
      throw err;
    });
  return loading;
}

/** Reads one STEP buffer. The returned arrays are copies, so a later read cannot overwrite them. */
export async function meshesFromStepBytes(bytes: ArrayBuffer, wasmUrl?: string): Promise<OcctPreviewMesh[]> {
  const occt = await loadOcct(wasmUrl);
  const result = occt.ReadStepFile(new Uint8Array(bytes), null);
  if (!result.success || !result.meshes?.length) throw new Error("浏览器无法读取这个 STEP 文件");
  return result.meshes.map((mesh) => {
    const positions = mesh.attributes?.position?.array;
    const normals = mesh.attributes?.normal?.array;
    const indices = mesh.index?.array;
    if (!positions || !normals || !indices) throw new Error("STEP 网格缺少顶点、法线或三角面");
    return {
      name: mesh.name,
      positions: Float64Array.from(positions),
      normals: Float64Array.from(normals),
      indices: Uint32Array.from(indices),
      faceTriangles: mesh.brep_faces?.map((face) => ({ first: face.first, last: face.last })),
    };
  });
}
