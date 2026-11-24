export function occtBridgeFileName(platform?: NodeJS.Platform | string): string;
export function libraryPathKey(platform?: NodeJS.Platform | string): "PATH" | "DYLD_LIBRARY_PATH" | "LD_LIBRARY_PATH";
export function resolveOcctBridgeLibDir(
  cliBin: string | null | undefined,
  env?: Record<string, string | undefined>,
  platform?: NodeJS.Platform | string,
): string | null;
export function envForCadCli(
  baseEnv: Record<string, string | undefined> | null | undefined,
  cliBin: string | null | undefined,
  platform?: NodeJS.Platform | string,
): Record<string, string | undefined>;
export function isCadCliBinary(file: unknown): boolean;
