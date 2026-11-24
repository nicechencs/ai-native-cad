import { existsSync } from "node:fs";
import path from "node:path";

export function occtBridgeFileName(platform = process.platform) {
  if (platform === "win32") return "occt_bridge.dll";
  if (platform === "darwin") return "libocct_bridge.dylib";
  return "libocct_bridge.so";
}

export function libraryPathKey(platform = process.platform) {
  if (platform === "win32") return "PATH";
  if (platform === "darwin") return "DYLD_LIBRARY_PATH";
  return "LD_LIBRARY_PATH";
}

/**
 * Locate a loadable OCCT bridge directory for a cad-cli binary.
 * Prefers OCCT_BRIDGE_LIB_DIR when it actually contains the bridge file;
 * otherwise uses the directory next to cad-cli (portable pack layout).
 * Does not invent a path when the file is missing.
 */
export function resolveOcctBridgeLibDir(cliBin, env = process.env, platform = process.platform) {
  const name = occtBridgeFileName(platform);
  const fromEnv = env?.OCCT_BRIDGE_LIB_DIR;
  if (typeof fromEnv === "string" && fromEnv.trim()) {
    const dir = path.resolve(fromEnv.trim());
    if (existsSync(path.join(dir, name))) return dir;
  }
  if (typeof cliBin === "string" && cliBin) {
    const dir = path.dirname(path.resolve(cliBin));
    if (existsSync(path.join(dir, name))) return dir;
  }
  return null;
}

export function envForCadCli(baseEnv, cliBin, platform = process.platform) {
  const env = { ...(baseEnv ?? {}) };
  const dir = resolveOcctBridgeLibDir(cliBin, env, platform);
  if (!dir) return env;
  const defaultKey = libraryPathKey(platform);
  const key =
    platform === "win32"
      ? (Object.keys(env).find((entry) => entry.toLowerCase() === "path") ?? defaultKey)
      : defaultKey;
  const delimiter = platform === "win32" ? ";" : ":";
  const current = env[key];
  env[key] = current ? `${dir}${delimiter}${current}` : dir;
  return env;
}

export function isCadCliBinary(file) {
  if (typeof file !== "string" || !file) return false;
  const base = path.basename(file).toLowerCase();
  return base === "cad-cli" || base === "cad-cli.exe";
}
