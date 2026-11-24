declare module "occt-import-js" {
  interface OcctImportModule {
    ReadStepFile(content: Uint8Array, params: null): {
      success?: boolean;
      meshes?: Array<{
        name?: string;
        brep_faces?: Array<{ first: number; last: number }>;
        attributes?: {
          position?: { array?: ArrayLike<number> };
          normal?: { array?: ArrayLike<number> };
        };
        index?: { array?: ArrayLike<number> };
      }>;
    };
  }

  export default function occtimportjs(options?: {
    locateFile?: (file: string) => string;
  }): Promise<OcctImportModule>;
}

declare module "occt-import-js/dist/occt-import-js.wasm?url" {
  const url: string;
  export default url;
}
