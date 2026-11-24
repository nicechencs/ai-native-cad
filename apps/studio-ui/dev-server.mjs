import { execFile, spawn } from "node:child_process";
import { AsyncLocalStorage } from "node:async_hooks";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { existsSync, statSync } from "node:fs";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { StringDecoder } from "node:string_decoder";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  parameterFromDimension,
  projectParameterIntoKcl,
  SessionCommandError,
} from "./src/core/session/engineeringSession.ts";
import {
  analyzeKclLspFallback,
  applyKclCompletion,
  clipToSection,
  generateFallbackDrawingSvg,
  generateFallbackDxfFromMesh,
  handleRange,
} from "./src/utils/studioHelpers.ts";

import { checkTemplatePrompt } from "./src/utils/promptSupport.ts";
import {
  loadClientAiProfileStore,
  resolveLlmWithSavedProfile,
  saveAiProfileStore,
  testAiProfileConnection,
} from "./ai-profile-store.mjs";

const execFileAsyncBase = promisify(execFile);

// Per-request trace context (x-trace-id) propagated to child processes as AICAD_TRACE_ID.
const traceStore = new AsyncLocalStorage();
const TRACE_ID_PATTERN = /^[A-Za-z0-9_.-]{8,64}$/;
function newTraceId() {
  return randomBytes(16).toString("hex");
}
function currentTraceId() {
  return traceStore.getStore()?.traceId ?? process.env.AICAD_TRACE_ID ?? newTraceId();
}
function withTraceEnv(env) {
  return { ...(env ?? process.env), AICAD_TRACE_ID: currentTraceId() };
}
function execFileAsync(file, args, options = {}) {
  return execFileAsyncBase(file, args, { ...options, env: withTraceEnv(options.env) });
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const REPO_ROOT = path.resolve(__dirname, "..", "..");

const HOST = process.env.HOST || "127.0.0.1";
const PORT = Number.parseInt(process.env.PORT || "5173", 10);

const cliCapabilityCache = new Map();
const agentCliProgressCache = new Map();
const MAX_ASSET_BYTES = 32 * 1024 * 1024;
const MAX_JSON_BYTES = 1024 * 1024;
const MAX_LISTED_ASSETS = 256;
const MAX_HLOD_CHUNK_BYTES = 16 * 1024 * 1024;
const MAX_TREE_CHUNK_BYTES = 32 * 1024 * 1024;
const MAX_CADPACK_BYTES = 1024 * 1024 * 1024;
const MAX_CADPACK_CHUNKS = 8192;
const MAX_DIGEST_JSON_BYTES = 12 * 1024;
const MAX_DIGEST_COMPONENTS = 10;
const MAX_DIGEST_LABEL_BYTES = 32;
const DEFAULT_TREE_SEARCH_LIMIT = 100;
const CADPACK_MAGIC = Buffer.from("CPK1");
const CADPACK_ALIGNMENT = 64;
const CADPACK_HEADER_SIZE = 64;
const CADPACK_ENTRY_SIZE = 64;
const CHUNK_KIND_VIRTUAL_TREE = 4;
const SESSION_ROUTE = /^\/api\/v1\/sessions\/([^/]+)\/(tree\/search|tree|interface-digest|hlod-chunks\/([^/]+))$/;
const ASSET_NAME = /^[a-z0-9][a-z0-9_-]{0,95}\.(?:step|stp|brep|iges|igs|stl|3mf|obj|gltf|glb)(?![\s\S])/;
const MESH_UNITS = new Set(["mm", "cm", "m", "in", "ft", "um"]);

function httpError(statusCode, message) {
  return Object.assign(new Error(message), { statusCode });
}

function isAssetName(name) {
  return ASSET_NAME.test(name) && !/^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])\./.test(name);
}

async function openWorkspace(configuredRoot) {
  await fs.mkdir(configuredRoot, { recursive: true, mode: 0o700 });
  return fs.realpath(configuredRoot);
}

async function openAssetDirectory(root) {
  const dir = path.join(root, "assets");
  await fs.mkdir(dir, { mode: 0o700 }).catch((err) => {
    if (err.code !== "EEXIST") throw err;
  });
  const info = await fs.lstat(dir);
  if (!info.isDirectory() || info.isSymbolicLink() || await fs.realpath(dir) !== dir) {
    throw httpError(403, "The workspace assets directory must be a real directory inside the workspace");
  }
  return dir;
}

function describeAsset(name, sizeBytes) {
  const ext = path.extname(name).slice(1);
  const format = { stp: "step", igs: "iges", glb: "gltf" }[ext] ?? ext;
  const assetPath = `assets/${name}`;
  const family = ["step", "brep", "iges"].includes(format) ? "step" : "mesh";
  const expression = `import::${family}(path = "${assetPath}")`;
  const previewExpression = family === "mesh"
    ? `import::mesh(path = "${assetPath}", allow_open_shell = true)`
    : undefined;
  const previewSource = previewExpression
    ? `part ImportedAsset() -> Solid {\n    return ${previewExpression}\n}\n`
    : undefined;
  return {
    name,
    path: assetPath,
    sizeBytes,
    format,
    expression,
    source: `part ImportedAsset() -> Solid {\n    return ${expression}\n}\n`,
    ...(previewExpression ? { previewExpression, previewSource } : {}),
  };
}

async function listAssets(dir) {
  const assets = [];
  const entries = await fs.opendir(dir);
  for await (const entry of entries) {
    if (!entry.isFile() || !isAssetName(entry.name)) continue;
    const info = await fs.lstat(path.join(dir, entry.name)).catch((err) => {
      if (err.code !== "ENOENT") throw err;
    });
    if (info?.isFile() && info.nlink === 1 && info.size > 0 && info.size <= MAX_ASSET_BYTES) {
      assets.push(describeAsset(entry.name, info.size));
    }
    if (assets.length === MAX_LISTED_ASSETS) break;
  }
  return assets.sort((a, b) => a.name.localeCompare(b.name));
}

async function readAssetPath(dir, assetPath) {
  const name = typeof assetPath === "string" && assetPath.startsWith("assets/") ? assetPath.slice(7) : "";
  if (!isAssetName(name)) throw httpError(400, "Expected a normalized assets/<name> path");
  const file = path.join(dir, name);
  try {
    const info = await fs.lstat(file);
    if (!info.isFile() || info.nlink !== 1 || await fs.realpath(file) !== file) {
      throw httpError(403, "The asset must be a regular file inside the workspace");
    }
    if (info.size === 0 || info.size > MAX_ASSET_BYTES) throw httpError(413, "Asset size is outside the allowed range");
    return file;
  } catch (err) {
    if (err.code === "ENOENT") throw httpError(404, "Asset not found");
    throw err;
  }
}

function normalizeAssetName(inputName) {
  // Validate ASCII first, before normalization can turn non-ASCII into ASCII.
  if (typeof inputName !== "string" || /[^A-Za-z0-9_.-]/.test(inputName) || !isAssetName(inputName.toLowerCase())) {
    throw httpError(400, "Use letters, numbers, underscores or hyphens and a STEP, BREP, IGES, STL, 3MF, OBJ, GLB or embedded glTF extension");
  }
  return inputName.toLowerCase();
}

async function storeAsset(dir, name, bytes) {
  if (!bytes.length) throw httpError(400, "Asset file is empty");
  const file = path.join(dir, name);
  const staging = await fs.mkdtemp(path.join(dir, ".upload-"));
  try {
    const staged = path.join(staging, "payload");
    await fs.writeFile(staged, bytes, { flag: "wx", mode: 0o600 });
    // Publish complete data atomically without replacing files or following a destination link.
    await fs.link(staged, file);
    return describeAsset(name, bytes.length);
  } catch (err) {
    if (err.code === "EEXIST") throw httpError(409, "An asset with this name already exists");
    throw err;
  } finally {
    await fs.rm(staging, { recursive: true, force: true });
  }
}

async function inspectAsset(root, dir, body) {
  await readAssetPath(dir, body.path);
  if (body.sourceUnit !== undefined && !MESH_UNITS.has(body.sourceUnit)) {
    throw httpError(400, "Unsupported source unit");
  }
  const cliBin = resolveCadCliBinary();
  if (!cliBin) throw httpError(503, "Build cad-cli to inspect assets");
  const args = ["inspect", body.path, "--workspace-root", root, "--json"];
  if (body.sourceUnit !== undefined) args.push("--source-unit", body.sourceUnit);
  try {
    const { stdout } = await execFileAsync(cliBin, args, { timeout: 30_000, maxBuffer: 2 * 1024 * 1024 });
    return JSON.parse(stdout);
  } catch (err) {
    if (err.killed || err.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
      throw httpError(422, "Asset inspection exceeded time or output limits");
    }
    let diagnostic;
    try { diagnostic = JSON.parse(err.stdout); } catch { /* CLI emitted no structured diagnostic */ }
    throw Object.assign(httpError(422, "Asset could not be inspected. Check its format and the cad-cli build."),
      diagnostic && typeof diagnostic === "object" ? { payload: diagnostic } : {});
  }
}

function resolveCadCliBinary() {
  const candidates = [
    process.env.CAD_CLI_BIN,
    path.join(REPO_ROOT, "target", "release", "cad-cli.exe"),
    path.join(REPO_ROOT, "target", "release", "cad-cli"),
    path.join(REPO_ROOT, "target", "debug", "cad-cli.exe"),
    path.join(REPO_ROOT, "target", "debug", "cad-cli"),
  ].filter(Boolean);

  let best = null;
  let bestMtime = -1;
  for (const candidate of candidates) {
    if (!existsSync(candidate)) {
      continue;
    }
    if (candidate === process.env.CAD_CLI_BIN) {
      return path.resolve(candidate);
    }
    const mtime = statSync(candidate).mtimeMs;
    if (mtime > bestMtime) {
      bestMtime = mtime;
      best = candidate;
    }
  }
  return best;
}

async function detectCliCapabilities(cliBin, runner = null) {
  const useCache = typeof runner !== "function";
  const cacheKey = useCache ? `${cliBin}:${statSync(cliBin).mtimeMs}` : null;
  if (cacheKey) {
    const cached = cliCapabilityCache.get(cacheKey);
    if (cached) {
      return cached;
    }
  }
  const exec = typeof runner === "function" ? runner : execFileAsync;

  let rootHelp = "";
  let buildHelp = "";
  try {
    const r1 = await exec(cliBin, ["--help"]);
    rootHelp = `${r1.stdout ?? ""}\n${r1.stderr ?? ""}`;
  } catch {
    // Ignore help probe failure
  }
  try {
    const r2 = await exec(cliBin, ["build", "--help"]);
    buildHelp = `${r2.stdout ?? ""}\n${r2.stderr ?? ""}`;
  } catch {
    // Ignore build help probe failure
  }

  const caps = {
    hasOutDxf: buildHelp.includes("--out-dxf"),
    hasOutDrawing: buildHelp.includes("--out-drawing"),
    hasOut3mf: buildHelp.includes("--out-3mf"),
    hasComponent: buildHelp.includes("--component"),
    hasLsp: /\blsp\b/.test(rootHelp),
  };
  if (cacheKey) {
    cliCapabilityCache.set(cacheKey, caps);
  }
  return caps;
}

async function collectKclExamples() {
  const scanDirs = [
    { dir: path.join(REPO_ROOT, "dsl-spec"), category: "dsl-spec" },
    {
      dir: path.join(REPO_ROOT, "examples", "mvp-tier1-plates"),
      category: "mvp-tier1-plates",
    },
    {
      dir: path.join(REPO_ROOT, "examples", "mvp-tier2-brackets"),
      category: "mvp-tier2-brackets",
    },
    {
      dir: path.join(REPO_ROOT, "examples", "mvp-tier3-advanced"),
      category: "mvp-tier3-advanced",
    },
    {
      dir: path.join(REPO_ROOT, "examples", "phase2-hybrid"),
      category: "phase2-hybrid",
    },
  ];

  const items = [];
  for (const entry of scanDirs) {
    if (!existsSync(entry.dir)) {
      continue;
    }
    const files = (await fs.readdir(entry.dir))
      .filter((name) => name.endsWith(".kcl"))
      .sort();
    for (const fileName of files) {
      const fullPath = path.join(entry.dir, fileName);
      const source = await fs.readFile(fullPath, "utf8");
      const relPath = path.relative(REPO_ROOT, fullPath).replace(/\\/g, "/");
      items.push({
        id: relPath,
        name: fileName.replace(/\.kcl$/, ""),
        category: entry.category,
        relativePath: relPath,
        source,
      });
    }
  }
  return items;
}

function sliceLinesSpan(source, span) {
  if (!span || typeof span.start_line !== "number" || typeof span.start_col !== "number") {
    return null;
  }
  const lines = source.split(/\r?\n/);
  const startLineIdx = Math.max(0, span.start_line - 1);
  const endLineIdx = Math.min(lines.length - 1, (span.end_line ?? span.start_line) - 1);
  if (startLineIdx > endLineIdx || startLineIdx >= lines.length) {
    return null;
  }

  if (startLineIdx === endLineIdx) {
    const line = lines[startLineIdx];
    const startColIdx = Math.max(0, span.start_col - 1);
    const endColIdx = Math.max(startColIdx, (span.end_col ?? line.length + 1) - 1);
    return {
      text: line.slice(startColIdx, endColIdx),
      baseLine: span.start_line,
      baseCol: startColIdx + 1,
    };
  }

  const parts = [];
  for (let i = startLineIdx; i <= endLineIdx; i += 1) {
    const line = lines[i];
    if (i === startLineIdx) {
      parts.push(line.slice(Math.max(0, span.start_col - 1)));
    } else if (i === endLineIdx) {
      parts.push(line.slice(0, Math.max(0, (span.end_col ?? line.length + 1) - 1)));
    } else {
      parts.push(line);
    }
  }
  return {
    text: parts.join("\n"),
    baseLine: span.start_line,
    baseCol: span.start_col,
  };
}

function advanceSpanFromIndex(baseLine, baseCol, sliceText, matchIndex, matchLength) {
  let line = baseLine;
  let col = baseCol;
  for (let i = 0; i < matchIndex; i += 1) {
    if (sliceText[i] === "\n") {
      line += 1;
      col = 1;
    } else {
      col += 1;
    }
  }
  let endLine = line;
  let endCol = col;
  for (let i = 0; i < matchLength; i += 1) {
    if (sliceText[matchIndex + i] === "\n") {
      endLine += 1;
      endCol = 1;
    } else {
      endCol += 1;
    }
  }
  return {
    start_line: line,
    start_col: col,
    end_line: endLine,
    end_col: endCol,
  };
}

export function enrichEditHandles(source, handles) {
  if (!Array.isArray(handles)) {
    return [];
  }
  const tier1ByParam = new Map();
  for (const h of handles) {
    if (h && h.tier === 1 && typeof h.param_name === "string") {
      tier1ByParam.set(h.param_name, h);
    }
  }

  return handles.map((h) => {
    if (!h || h.tier !== 2 || h.tier2_binding) {
      return h;
    }

    const lines = source.split(/\r?\n/);
    const spanSlice =
      sliceLinesSpan(source, h.ast_span) ??
      (() => {
        const lineNum = h.ast_span?.start_line ?? 1;
        const text = lines[lineNum - 1] ?? "";
        return { text, baseLine: lineNum, baseCol: 1 };
      })();

    const affineMatch =
      /([a-zA-Z_][\w]*)\s*([+-])\s*(\d[\d_]*(?:\.[\d_]+)?(?:[eE][+-]?\d[\d_]*)?)([a-zA-Z_]+)?/.exec(
        spanSlice.text,
      );
    if (!affineMatch || affineMatch.index === undefined) {
      return h;
    }

    const upstreamParam = affineMatch[1];
    const operator = affineMatch[2];
    const rawNumText = affineMatch[3];
    const offsetVal = Number.parseFloat(rawNumText.replace(/_/g, ""));
    const offsetUnit = affineMatch[4] ?? (h.unit === "unitless" ? "" : h.unit);
    const upstreamHandle = tier1ByParam.get(upstreamParam);
    const offsetToken = `${rawNumText}${affineMatch[4] ?? ""}`;

    const operatorPrefix = affineMatch[0].slice(
      0,
      affineMatch[0].length - offsetToken.length,
    );
    const offsetIndexInSlice = affineMatch.index + operatorPrefix.length;
    const offsetSpan = advanceSpanFromIndex(
      spanSlice.baseLine,
      spanSlice.baseCol,
      spanSlice.text,
      offsetIndexInSlice,
      offsetToken.length,
    );

    if (!upstreamHandle || !Number.isFinite(offsetVal)) {
      return h;
    }

    return {
      ...h,
      tier2_binding: {
        operator,
        upstream_param: upstreamParam,
        upstream_value: upstreamHandle.current_value,
        upstream_unit:
          upstreamHandle.unit === "unitless" ? "" : upstreamHandle.unit,
        upstream_span: upstreamHandle.ast_span,
        offset_value: offsetVal,
        offset_unit: offsetUnit,
        offset_span: offsetSpan,
      },
    };
  });
}

function isClientAbort(signal, err) {
  if (signal?.aborted) return true;
  if (!err || typeof err !== "object") return false;
  return err.name === "AbortError" || err.code === "ABORT_ERR" || err.code === "CLIENT_ABORTED";
}

function clientAbortError() {
  const err = new Error("The export was cancelled before it finished.");
  err.name = "AbortError";
  err.code = "CLIENT_ABORTED";
  return err;
}

async function runCadCliBuild(fileName, source, workspaceRoot, componentId, previewOnly = false, cadCliRunner = null, signal = null) {
  const hasCustomRunner = typeof cadCliRunner === "function";
  const cliBin = hasCustomRunner ? (resolveCadCliBinary() ?? "cad-cli") : resolveCadCliBinary();
  if (!cliBin) {
    const err = new Error(
      "Compiled cad-cli binary not found in target/debug or target/release.",
    );
    err.statusCode = 503;
    throw err;
  }
  if (signal?.aborted) throw clientAbortError();

  const caps = await detectCliCapabilities(cliBin, cadCliRunner);
  if (componentId && !caps.hasComponent) {
    const err = new Error("This cad-cli build does not support component export (--component).");
    err.statusCode = 501;
    throw err;
  }
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-studio-"));
  // The display name never controls a filesystem path (including Windows device names).
  const kclName = "model.kcl";
  const kclPath = path.join(tmpDir, kclName);
  const stepPath = path.join(tmpDir, "output.step");
  const meshPath = path.join(tmpDir, "output.mesh.json");
  const viewsDir = path.join(tmpDir, "views");
  const dxfPath = path.join(tmpDir, "output.dxf");
  const drawingPath = path.join(tmpDir, "drawing.svg");
  const threemfPath = path.join(tmpDir, "output.3mf");

  try {
    await fs.writeFile(kclPath, source, "utf8");
    await fs.mkdir(viewsDir, { recursive: true });

    const args = ["build", kclPath, "--workspace-root", workspaceRoot, "--out-mesh", meshPath];
    if (!previewOnly) {
      args.push("--out-step", stepPath, "--out-views", viewsDir);
      if (caps.hasOutDxf) args.push("--out-dxf", dxfPath);
      if (caps.hasOutDrawing) args.push("--out-drawing", drawingPath);
      if (caps.hasOut3mf) args.push("--out-3mf", threemfPath);
    }
    if (caps.hasComponent && componentId) {
      args.push("--component", componentId);
    }
    args.push("--json");

    let stdout = "";
    let stderr = "";
    let exitCode = 0;

    try {
      if (signal?.aborted) throw clientAbortError();
      const exec = hasCustomRunner ? cadCliRunner : execFileAsync;
      const execOptions = {
        maxBuffer: 25 * 1024 * 1024,
        timeout: 60_000,
      };
      // Custom runners and execFile both receive this. Aborting it kills the
      // production child; a runner that keeps going is discarded by the caller.
      if (signal) execOptions.signal = signal;
      const res = await exec(cliBin, args, execOptions);
      if (signal?.aborted) throw clientAbortError();
      stdout = res.stdout ?? "";
      stderr = res.stderr ?? "";
    } catch (execErr) {
      if (isClientAbort(signal, execErr)) throw clientAbortError();
      exitCode = typeof execErr.code === "number" ? execErr.code : 1;
      stdout = execErr.stdout ?? "";
      stderr = execErr.stderr ?? execErr.message ?? "";
    }

    let diagnosticsReport;
    let transportError = null;
    try {
      diagnosticsReport = JSON.parse(stdout);
    } catch {
      // Transport/process failure: report it as such instead of a fake source diagnostic.
      transportError = {
        message: String(stderr || stdout || "cad-cli failed without JSON output").slice(0, 2000),
        exit_code: exitCode,
      };
      diagnosticsReport = {
        status: "error",
        file: fileName || kclName,
        diagnostics: [
          {
            code: "R011_IPC_FAILED",
            severity: "error",
            message: transportError.message,
            span: null,
          },
        ],
        transport_error: transportError.message,
        trace_id: currentTraceId(),
        code: "R011_IPC_FAILED",
        metrics: {
          watertight: false,
          volume_mm3: 0,
          bbox_mm: { min: [0, 0, 0], max: [0, 0, 0] },
          face_count: 0,
          edge_count: 0,
        },
      };
    }

    let sceneMesh = null;
    if (existsSync(meshPath)) {
      const rawMesh = JSON.parse(await fs.readFile(meshPath, "utf8"));
      sceneMesh = {
        ...rawMesh,
        edit_handles: enrichEditHandles(source, rawMesh.edit_handles),
      };
    }

    let stepContent = "";
    if (existsSync(stepPath)) {
      stepContent = await fs.readFile(stepPath, "utf8");
    }

    const views = {};
    for (const viewName of ["front", "top", "right", "iso"]) {
      const svgPath = path.join(viewsDir, `${viewName}.svg`);
      if (existsSync(svgPath)) {
        views[viewName] = await fs.readFile(svgPath, "utf8");
      }
    }

    const buildOk = diagnosticsReport.status === "ok" && exitCode === 0;

    let dxfContent = "";
    if (existsSync(dxfPath)) {
      dxfContent = await fs.readFile(dxfPath, "utf8");
    } else if (buildOk && !previewOnly && sceneMesh && !caps.hasOutDxf) {
      dxfContent = generateFallbackDxfFromMesh(sceneMesh);
    }

    let drawingSvg = "";
    if (existsSync(drawingPath)) {
      drawingSvg = await fs.readFile(drawingPath, "utf8");
    } else if (buildOk && !previewOnly && sceneMesh && diagnosticsReport?.metrics) {
      drawingSvg = generateFallbackDrawingSvg(
        sceneMesh,
        diagnosticsReport.metrics,
        views,
      );
    }

    let threemfBase64 = "";
    if (existsSync(threemfPath)) {
      threemfBase64 = (await fs.readFile(threemfPath)).toString("base64");
    }

    if (signal?.aborted) throw clientAbortError();

    return {
      ok: buildOk,
      status: buildOk ? "ok" : "error",
      exitCode,
      cliBinary: path.relative(REPO_ROOT, cliBin).replace(/\\/g, "/"),
      file: fileName || kclName,
      componentId: componentId || null,
      diagnostics: diagnosticsReport,
      ...(transportError
        ? { transport_error: transportError.message, trace_id: currentTraceId(), code: "R011_IPC_FAILED" }
        : {}),
      metrics: diagnosticsReport.metrics,
      sceneMesh,
      views,
      stepContent,
      dxfContent,
      dxfIsCutProfile: existsSync(dxfPath),
      drawingSvg,
      threemfBase64,
    };
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
  }
}

async function runLspQuery(fileName, source, workspaceRoot, line, col) {
  const cliBin = resolveCadCliBinary();
  if (cliBin) {
    const caps = await detectCliCapabilities(cliBin);
    if (caps.hasLsp) {
      const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-lsp-"));
      const kclPath = path.join(tmpDir, "query.kcl");
      try {
        await fs.writeFile(kclPath, source, "utf8");
        const args = ["lsp", kclPath, "--workspace-root", workspaceRoot];
        if (typeof line === "number" && typeof col === "number") {
          args.push("--line", String(line), "--col", String(col));
        }
        args.push("--json");
        const { stdout } = await execFileAsync(cliBin, args, {
          maxBuffer: 5 * 1024 * 1024,
        }).catch((err) => ({ stdout: err.stdout }));
        const parsed = JSON.parse(stdout || "null");
        if (Array.isArray(parsed?.completions)) {
          return { ...parsed, backend: "cad-cli-lsp", file: fileName || "query.kcl" };
        }
      } catch {
        // Fall through to built-in analyzer if cad-cli lsp fails
      } finally {
        await fs.rm(tmpDir, { recursive: true, force: true });
      }
    }
  }
  return {
    file: fileName || "query.kcl",
    ...analyzeKclLspFallback(source, line ?? 1, col ?? 1),
  };
}

const VALID_AGENT_MODES = new Set(["generate", "edit"]);
const VALID_LLM_PROVIDERS = new Set(["auto", "template", "openai", "gemini", "deterministic"]);
const MAX_SELECTED_REFS = 64;
const MAX_SELECTED_REF_LEN = 256;
const SAFE_TOPO_REF = /^[^\x00-\x1f\x7f]{1,256}$/;
const WINDOWS_RESERVED_SEGMENT = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i;

function getServerAiConfig(env = process.env) {
  const hasOpenaiKey = Boolean((env.OPENAI_API_KEY || "").trim());
  const hasGeminiKey = Boolean((env.GEMINI_API_KEY || "").trim());
  const configuredProvider = (env.CAD_AGENT_PROVIDER || "").trim().toLowerCase();

  let defaultProvider;
  if (
    configuredProvider === "openai" ||
    configuredProvider === "gemini" ||
    configuredProvider === "template"
  ) {
    defaultProvider = configuredProvider;
  } else if (hasOpenaiKey) {
    defaultProvider = "openai";
  } else if (hasGeminiKey) {
    defaultProvider = "gemini";
  } else {
    defaultProvider = "template";
  }

  return {
    default_provider: defaultProvider,
    has_openai_key: hasOpenaiKey,
    has_gemini_key: hasGeminiKey,
    openai_base_url: (env.OPENAI_BASE_URL || "").trim() || "https://api.openai.com/v1",
    openai_model: (env.OPENAI_MODEL || "").trim() || "gpt-4o-mini",
    gemini_model: (env.GEMINI_MODEL || "").trim() || "gemini-2.5-flash",
  };
}

export function resolveSubprocessLlmEnv(baseEnv, provider, llm) {
  const env = { ...baseEnv };
  const apiKey = typeof llm?.api_key === "string" ? llm.api_key.trim() : "";
  const model = typeof llm?.model === "string" ? llm.model.trim() : "";
  const baseUrl = typeof llm?.base_url === "string" ? llm.base_url.trim() : "";

  let resolvedProvider =
    typeof provider === "string" && VALID_LLM_PROVIDERS.has(provider) ? provider : "auto";

  if (resolvedProvider === "auto" && (apiKey || baseUrl)) {
    if (baseUrl) {
      resolvedProvider = /generativelanguage\.googleapis\.com/i.test(baseUrl)
        ? "gemini"
        : "openai";
    } else {
      const envProvider = (baseEnv.CAD_AGENT_PROVIDER || "").trim().toLowerCase();
      const looksGemini =
        /gemini/i.test(model) ||
        /^AIza/.test(apiKey) ||
        (!model && envProvider === "gemini");
      resolvedProvider = looksGemini ? "gemini" : "openai";
    }
  }

  if (resolvedProvider === "gemini") {
    delete env.CAD_AGENT_PROVIDER;
    delete env.OPENAI_API_KEY;
    if (apiKey) {
      env.GEMINI_API_KEY = apiKey;
    }
  } else if (resolvedProvider === "openai") {
    delete env.CAD_AGENT_PROVIDER;
    delete env.GEMINI_API_KEY;
    if (apiKey) {
      env.OPENAI_API_KEY = apiKey;
    }
  } else if (resolvedProvider === "template" || resolvedProvider === "deterministic") {
    delete env.OPENAI_API_KEY;
    delete env.GEMINI_API_KEY;
    env.CAD_AGENT_PROVIDER = resolvedProvider;
  }

  return { resolvedProvider, env };
}

function hasConfiguredRealLlm(resolvedProvider, llmEnv, llm) {
  if (resolvedProvider === "deterministic") {
    return true;
  }
  if (resolvedProvider === "template") {
    return false;
  }
  const hasBaseUrl = Boolean(typeof llm?.base_url === "string" && llm.base_url.trim());
  const hasOpenaiKey = Boolean((llmEnv.OPENAI_API_KEY || "").trim());
  const hasGeminiKey = Boolean((llmEnv.GEMINI_API_KEY || "").trim());
  if (resolvedProvider === "openai") {
    return hasOpenaiKey || hasBaseUrl;
  }
  if (resolvedProvider === "gemini") {
    return hasGeminiKey;
  }
  return hasOpenaiKey || hasGeminiKey || hasBaseUrl;
}

function isUnconfiguredTemplateRequest(mode, provider, llm, baseEnv = process.env) {
  if (mode !== "generate") {
    return false;
  }
  if (provider === "template") {
    return true;
  }
  if (provider === "openai" || provider === "gemini" || provider === "deterministic") {
    return false;
  }
  const hasRequestKey = Boolean(typeof llm?.api_key === "string" && llm.api_key.trim());
  const hasRequestBaseUrl = Boolean(typeof llm?.base_url === "string" && llm.base_url.trim());
  const hasProfileId = Boolean(typeof llm?.profile_id === "string" && llm.profile_id.trim());
  if (hasRequestKey || hasRequestBaseUrl || hasProfileId) {
    return false;
  }
  return getServerAiConfig(baseEnv).default_provider === "template";
}

const NATIVE_SUPPORTED_FEATURES = new Set(["ellipse", "spline", "spatial_curve"]);

function checkUnsupportedTemplateFeatures(prompt) {
  const res = checkTemplatePrompt(prompt);
  if (!res) {
    return null;
  }
  const remaining = Array.isArray(res.unsupported_features)
    ? res.unsupported_features.filter((id) => !NATIVE_SUPPORTED_FEATURES.has(id))
    : [];
  if (remaining.length === 0) {
    return null;
  }
  if (remaining.length === res.unsupported_features.length) {
    return res;
  }
  return {
    ...res,
    unsupported_features: remaining,
  };
}

function sanitizeTextSecrets(message, secrets = []) {
  const allSecrets = [
    ...(Array.isArray(secrets) ? secrets : []),
    process.env.OPENAI_API_KEY,
    process.env.GEMINI_API_KEY,
  ];
  let out = String(message || "");
  for (const secret of allSecrets) {
    if (typeof secret === "string" && secret.trim().length >= 4) {
      out = out.split(secret.trim()).join("[REDACTED]");
    }
  }
  return out;
}

function sanitizeSecrets(message, secrets = []) {
  return sanitizeTextSecrets(message, secrets);
}

function sanitizeValueSecrets(value, secrets = []) {
  if (typeof value === "string") {
    return sanitizeTextSecrets(value, secrets);
  }
  if (Array.isArray(value)) {
    return value.map((item) => sanitizeValueSecrets(item, secrets));
  }
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      out[k] = sanitizeValueSecrets(v, secrets);
    }
    return out;
  }
  return value;
}

const AGENT_PROGRESS_PREFIX = "CAD_AGENT_PROGRESS:";

export function parseAgentProgressLine(line) {
  if (typeof line !== "string") {
    return null;
  }
  const trimmed = line.trim();
  if (!trimmed.startsWith(AGENT_PROGRESS_PREFIX)) {
    return null;
  }
  const jsonText = trimmed.slice(AGENT_PROGRESS_PREFIX.length).trim();
  if (!jsonText) {
    return null;
  }
  try {
    const parsed = JSON.parse(jsonText);
    if (
      !parsed ||
      typeof parsed !== "object" ||
      Array.isArray(parsed) ||
      typeof parsed.stage !== "string" ||
      typeof parsed.message !== "string"
    ) {
      return null;
    }
    return {
      ...parsed,
      event: "progress",
      stage: parsed.stage,
      message: parsed.message,
    };
  } catch {
    return null;
  }
}

async function detectAgentCliSupportsProgress(pyBin, env, cliPyPath) {
  let mtime = 0;
  try {
    mtime = statSync(cliPyPath).mtimeMs;
  } catch {
    // Ignore stat failure
  }
  const cacheKey = `${pyBin}:${mtime}`;
  if (agentCliProgressCache.has(cacheKey)) {
    return agentCliProgressCache.get(cacheKey);
  }
  let supported = false;
  try {
    const res = await execFileAsync(pyBin, ["-m", "cad_agent.cli", "--help"], {
      env,
      cwd: REPO_ROOT,
      timeout: 10000,
    });
    const combined = `${res.stdout ?? ""}\n${res.stderr ?? ""}`;
    supported = combined.includes("--progress-stderr");
  } catch (err) {
    if (err && err.code === "ENOENT") {
      return false;
    }
    const combined = `${err?.stdout ?? ""}\n${err?.stderr ?? ""}`;
    supported = combined.includes("--progress-stderr");
  }
  agentCliProgressCache.set(cacheKey, supported);
  return supported;
}

function runAgentCliSpawn(pyBin, args, { env, cwd, onProgressLine, signal }) {
  return new Promise((resolve) => {
    if (signal?.aborted) {
      resolve({ stdout: "", stderr: "", exitCode: -1, aborted: true, enoent: false });
      return;
    }

    const child = spawn(pyBin, args, {
      env: withTraceEnv(env),
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const stdoutChunks = [];
    const stderrChunks = [];
    const stderrDecoder = new StringDecoder("utf8");
    let stderrLineBuf = "";
    let settled = false;
    let aborted = false;

    const finish = (result) => {
      if (settled) return;
      settled = true;
      if (signal) {
        signal.removeEventListener("abort", onAbort);
      }
      resolve(result);
    };

    const onAbort = () => {
      aborted = true;
      try {
        child.kill();
      } catch {
        // Ignore kill failure
      }
    };

    if (signal) {
      signal.addEventListener("abort", onAbort, { once: true });
    }

    child.stdout?.on("data", (chunk) => {
      stdoutChunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    });

    child.stderr?.on("data", (chunk) => {
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      stderrChunks.push(buf);
      if (onProgressLine) {
        stderrLineBuf += stderrDecoder.write(buf);
        const lines = stderrLineBuf.split(/\r?\n/);
        stderrLineBuf = lines.pop() ?? "";
        for (const line of lines) {
          onProgressLine(line);
        }
      }
    });

    child.on("error", (err) => {
      finish({
        stdout: Buffer.concat(stdoutChunks).toString("utf8"),
        stderr: Buffer.concat(stderrChunks).toString("utf8"),
        exitCode: -1,
        aborted,
        enoent: Boolean(err && err.code === "ENOENT"),
      });
    });

    child.on("close", (code) => {
      if (onProgressLine) {
        stderrLineBuf += stderrDecoder.end();
        const lines = stderrLineBuf.split(/\r?\n/);
        for (const line of lines) {
          if (line.trim()) {
            onProgressLine(line);
          }
        }
        stderrLineBuf = "";
      }
      finish({
        stdout: Buffer.concat(stdoutChunks).toString("utf8"),
        stderr: Buffer.concat(stderrChunks).toString("utf8"),
        exitCode: typeof code === "number" ? code : 1,
        aborted,
        enoent: false,
      });
    });
  });
}

function isValidFewShotK(value) {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 5;
}

export function buildAgentCliArgs({
  prompt,
  material,
  manufacturingMethod,
  mode,
  resolvedProvider,
  workDir,
  cliBin,
  workspaceRoot,
  projectPath,
  target,
  previewBranch,
  contextAsset,
  existingKclPath,
  llm,
  includeDrawings = true,
  progressStderr = false,
}) {
  const model = typeof llm?.model === "string" ? llm.model.trim() : "";
  const baseUrl = typeof llm?.base_url === "string" ? llm.base_url.trim() : "";
  const enableVlm = llm?.enable_vlm === true;
  const fewShotK = isValidFewShotK(llm?.few_shot_k) ? llm.few_shot_k : undefined;

  const args = [
    "-m",
    "cad_agent.cli",
    "--semantic-v2",
    `--prompt=${prompt}`,
    `--material=${material}`,
    `--manufacturing-method=${manufacturingMethod}`,
    `--mode=${mode}`,
    `--provider=${resolvedProvider}`,
    "--work-dir",
    workDir,
    "--cli-bin",
    cliBin,
    "--workspace-root",
    workspaceRoot,
    "--json",
  ];
  if (typeof projectPath === "string" && projectPath.trim()) {
    args.push("--project", projectPath.trim());
  }
  if (typeof target === "string" && target.trim()) {
    args.push("--target", target.trim());
  }
  if (typeof previewBranch === "string" && previewBranch.trim()) {
    args.push("--preview-branch", previewBranch.trim());
  }
  if (typeof contextAsset === "string" && contextAsset.trim()) {
    args.push("--context-asset", contextAsset.trim());
  }
  if (mode === "edit" && existingKclPath) {
    args.push("--existing-kcl", existingKclPath);
  }
  if (model) {
    args.push(`--model=${model}`);
  }
  if (baseUrl) {
    args.push(`--base-url=${baseUrl}`);
  }
  if (enableVlm) {
    args.push("--enable-vlm");
  }
  if (fewShotK !== undefined) {
    args.push(`--few-shot-k=${fewShotK}`);
  }
  if (!includeDrawings) {
    args.push("--no-drawings");
  }
  if (progressStderr) {
    args.push("--progress-stderr");
  }
  return args;
}

export function buildAgentSummary(payload, fallbackMode = "generate") {
  const p = payload && typeof payload === "object" ? payload : {};
  return {
    passed: p.passed,
    archetype: p.archetype,
    iterations_run: p.iterations_run,
    contract: p.contract,
    verdict: p.verdict ?? p.overall_status,
    mode: p.mode ?? fallbackMode,
    llm_provider: p.llm_provider ?? null,
    llm_model: p.llm_model ?? null,
    used_llm: Boolean(p.used_llm),
    vlm_enabled: Boolean(p.vlm_enabled),
    retrieved_examples: Array.isArray(p.retrieved_examples) ? p.retrieved_examples : [],
    contract_repair_attempts: Number.isFinite(p.contract_repair_attempts)
      ? p.contract_repair_attempts
      : 0,
    ...(typeof p.runtime_path === "string" ? { runtime_path: p.runtime_path } : {}),
    ...(typeof p.evaluator === "string" ? { evaluator: p.evaluator } : {}),
    ...(typeof p.overall_status === "string" ? { overall_status: p.overall_status } : {}),
    ...(typeof p.branch === "string" ? { branch: p.branch } : {}),
    ...(typeof p.preview_branch === "string" ? { preview_branch: p.preview_branch } : {}),
    ...(Number.isSafeInteger(p.revision) ? { revision: p.revision } : {}),
  };
}

function mcpErrorStatusCode(code) {
  if (code === "E409_REVISION_CONFLICT" || code === "E_PREVIEW_DIVERGED") return 409;
  if (code === "E404_BRANCH_NOT_FOUND" || code === "E404_NODE_NOT_FOUND") return 404;
  if (code === "E400_INVALID_ARGUMENT" || code === "E400_INVALID_COMMAND") return 400;
  return 422;
}

function parseMcpToolEnvelope(envelope) {
  if (!envelope || typeof envelope !== "object") {
    throw httpError(422, "cad-cli mcp returned an invalid JSON-RPC envelope");
  }
  if (envelope.error && typeof envelope.error === "object") {
    const msg = typeof envelope.error.message === "string"
      ? envelope.error.message
      : "cad-cli mcp JSON-RPC error";
    const err = httpError(422, msg);
    err.payload = { ok: false, error: msg, error_code: String(envelope.error.code ?? "mcp_error") };
    throw err;
  }
  const result = envelope.result;
  const text = Array.isArray(result?.content) && typeof result.content[0]?.text === "string"
    ? result.content[0].text
    : "";
  let parsed = null;
  if (text.trim()) {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = { raw: text };
    }
  }
  if (result?.isError) {
    const code = typeof parsed?.code === "string" ? parsed.code : "mcp_tool_error";
    const msg = typeof parsed?.message === "string"
      ? parsed.message
      : (text.trim() || "cad-cli mcp tool call failed");
    const err = httpError(mcpErrorStatusCode(code), msg);
    err.payload = {
      ok: false,
      error: msg,
      error_code: code,
      ...(parsed && typeof parsed === "object" ? { details: parsed } : {}),
    };
    throw err;
  }
  return parsed ?? result ?? {};
}

async function runMcpToolCalls({ workspaceRoot, projectPath, calls, cliBin = null, runner = null }) {
  if (typeof runner === "function") {
    return runner({ workspaceRoot, projectPath, calls });
  }
  const resolvedCli = cliBin || resolveCadCliBinary();
  if (!resolvedCli) {
    throw httpError(503, "Compiled cad-cli binary not found in target/debug or target/release.");
  }
  const requests = [
    {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-03-26",
        capabilities: {},
        clientInfo: { name: "aicad-studio-dev-server", version: "0.1.0" },
      },
    },
    ...calls.map((call, idx) => ({
      jsonrpc: "2.0",
      id: idx + 2,
      method: "tools/call",
      params: {
        name: call.name,
        arguments: call.arguments ?? {},
      },
    })),
  ];

  const stdinText = `${requests.map((r) => JSON.stringify(r)).join("\n")}\n`;
  const args = [
    "mcp",
    "--headless",
    "--project",
    projectPath,
    "--workspace",
    workspaceRoot,
  ];

  const { stdout, stderr, exitCode } = await new Promise((resolve, reject) => {
    const child = spawn(resolvedCli, args, {
      env: withTraceEnv(process.env),
      cwd: REPO_ROOT,
      stdio: ["pipe", "pipe", "pipe"],
    });
    const outChunks = [];
    const errChunks = [];
    const timer = setTimeout(() => {
      try { child.kill(); } catch { /* ignore */ }
      reject(httpError(422, "cad-cli mcp command exceeded timeout"));
    }, 30_000);
    child.stdout?.on("data", (c) => outChunks.push(Buffer.isBuffer(c) ? c : Buffer.from(c)));
    child.stderr?.on("data", (c) => errChunks.push(Buffer.isBuffer(c) ? c : Buffer.from(c)));
    child.on("error", (err) => {
      clearTimeout(timer);
      if (err?.code === "ENOENT") {
        reject(httpError(503, "Compiled cad-cli binary not found in target/debug or target/release."));
      } else {
        reject(httpError(422, err?.message || "Failed to start cad-cli mcp"));
      }
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({
        stdout: Buffer.concat(outChunks).toString("utf8"),
        stderr: Buffer.concat(errChunks).toString("utf8"),
        exitCode: typeof code === "number" ? code : 1,
      });
    });
    child.stdin?.end(stdinText, "utf8");
  });

  const byId = new Map();
  for (const rawLine of stdout.split(/\r?\n/)) {
    const trimmed = rawLine.trim();
    if (!trimmed) continue;
    try {
      const msg = JSON.parse(trimmed);
      if (msg && typeof msg === "object" && msg.id !== undefined) {
        byId.set(msg.id, msg);
      }
    } catch {
      // Ignore non-JSON log lines
    }
  }

  if (!byId.has(1)) {
    const msg = stderr.trim() || stdout.trim() || `cad-cli mcp exited with code ${exitCode}`;
    throw httpError(422, msg.slice(0, 2000));
  }
  const initEnvelope = byId.get(1);
  if (initEnvelope?.error) {
    parseMcpToolEnvelope(initEnvelope);
  }

  const results = [];
  for (let i = 0; i < calls.length; i += 1) {
    const envelope = byId.get(i + 2);
    if (!envelope) {
      const msg = stderr.trim() || `cad-cli mcp returned no response for ${calls[i].name}`;
      throw httpError(422, msg.slice(0, 2000));
    }
    results.push(parseMcpToolEnvelope(envelope));
  }
  return results;
}

async function stageExistingKclIntoProject({
  workspaceRoot,
  projectPath,
  existingSource,
  cliBin,
  mcpToolRunner,
}) {
  const results = await runMcpToolCalls({
    workspaceRoot,
    projectPath,
    cliBin,
    runner: mcpToolRunner,
    calls: [
      {
        name: "cad_sync_kcl_view",
        arguments: {
          action: "sync",
          expected_revision: 0,
          actor: { kind: "kcl_view" },
          kcl: existingSource,
        },
      },
      {
        name: "cad_inspect_and_slice",
        arguments: { granularity: "summary" },
      },
    ],
  });
  const envelope = results[1] && typeof results[1] === "object" ? results[1] : {};
  const summary = envelope.summary && typeof envelope.summary === "object"
    ? envelope.summary
    : envelope;
  const outputs = Array.isArray(summary.outputs) ? summary.outputs : [];
  for (let i = outputs.length - 1; i >= 0; i -= 1) {
    const out = outputs[i];
    if (typeof out === "string" && out.trim() && out.trim() !== "__design_contract") {
      return out.trim();
    }
  }
  const outline = Array.isArray(summary.feature_outline)
    ? summary.feature_outline
    : (Array.isArray(summary.nodes) ? summary.nodes : []);
  for (let i = outline.length - 1; i >= 0; i -= 1) {
    const node = outline[i];
    const id = typeof node?.node_id === "string"
      ? node.node_id
      : (typeof node?.id === "string" ? node.id : "");
    if (id.trim() && id.trim() !== "__design_contract") {
      return id.trim();
    }
  }
  const execOrder = Array.isArray(summary.validation?.execution_order)
    ? summary.validation.execution_order
    : [];
  for (let i = execOrder.length - 1; i >= 0; i -= 1) {
    const item = execOrder[i];
    if (
      typeof item === "string" &&
      item.trim() &&
      !item.startsWith("param:") &&
      item !== "__design_contract"
    ) {
      return item.trim();
    }
  }
  const params = Array.isArray(summary.parameters) ? summary.parameters : [];
  const userParam = params.find(
    (p) => typeof p?.name === "string" && p.name.trim() && !p.name.startsWith("dsl:") && p.formula == null,
  );
  if (userParam) {
    return userParam.name.trim();
  }
  if (typeof params[0]?.name === "string" && params[0].name.trim()) {
    return params[0].name.trim();
  }
  return "";
}

async function extractPreviewKclFromProject(projectPath, previewBranch, tmpDir, cliBin, projectCliRunner) {
  if (!existsSync(projectPath)) {
    return "";
  }
  const copyPath = path.join(tmpDir, "preview_read.aicad");
  await fs.copyFile(projectPath, copyPath);
  const runCli = async (args, json) => {
    if (typeof projectCliRunner === "function" && projectCliRunner !== runProjectCli) {
      return projectCliRunner(args, json);
    }
    const res = await execFileAsync(cliBin, args, {
      timeout: 30_000,
      maxBuffer: 8 * 1024 * 1024,
    });
    return json ? parseProjectCliJson(res.stdout) : textFromStdio(res.stdout);
  };
  if (typeof previewBranch === "string" && previewBranch.trim() && previewBranch.trim() !== "main") {
    try {
      await runCli(["project", "branch", copyPath, "checkout", previewBranch.trim(), "--json"], true);
    } catch {
      // Keep active branch of copied snapshot if branch is already checked out or absent
    }
  }
  try {
    const emitted = await runCli(["project", "emit-kcl", copyPath], false);
    if (typeof emitted === "string") return emitted;
    if (emitted && typeof emitted === "object" && typeof emitted.kcl === "string") return emitted.kcl;
  } catch {
    return "";
  }
  return "";
}

function buildChangePreviewPayload(payload, {
  mode,
  explicitProjectRelPath,
  selectedTopoRefs,
  selectedNodeIds,
  sessionId,
}) {
  const p = payload && typeof payload === "object" ? payload : {};
  const previewBranch = typeof p.preview_branch === "string" && p.preview_branch.trim()
    ? p.preview_branch.trim()
    : (typeof p.branch === "string" && p.branch.trim() ? p.branch.trim() : null);
  const verdict = typeof p.overall_status === "string"
    ? p.overall_status
    : (typeof p.verdict === "string" ? p.verdict : (p.passed ? "PASS" : "UNKNOWN"));
  return {
    runtime_path: typeof p.runtime_path === "string" ? p.runtime_path : "semantic_v2",
    mode: typeof p.mode === "string" ? p.mode : mode,
    project_path: explicitProjectRelPath || null,
    preview_branch: previewBranch,
    base_branch: typeof p.base_branch === "string" && p.base_branch.trim() ? p.base_branch.trim() : "main",
    revision: Number.isSafeInteger(p.revision) ? p.revision : null,
    passed: Boolean(p.passed),
    verdict,
    diff: p.diff ?? null,
    verification: p.verification ?? null,
    explanation: p.explanation ?? null,
    commands: Array.isArray(p.commands) ? p.commands : [],
    selected_topo_refs: selectedTopoRefs,
    selected_node_ids: selectedNodeIds,
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(Array.isArray(p.untrusted_asset_labels)
      ? { untrusted_asset_labels: p.untrusted_asset_labels }
      : {}),
  };
}

async function runAgentGenerate(prompt, material, manufacturingMethod, workspaceRoot, options = {}) {
  const mode = options.mode === "edit" ? "edit" : "generate";
  const existingSource =
    typeof options.existingSource === "string" ? options.existingSource : "";
  const llm = options.llm && typeof options.llm === "object" ? options.llm : {};
  const explicitProjectRelPath =
    typeof options.projectPath === "string" && options.projectPath.trim()
      ? options.projectPath.trim()
      : "";
  const selectedTopoRefs = Array.isArray(options.selectedTopoRefs) ? options.selectedTopoRefs : [];
  const selectedNodeIds = Array.isArray(options.selectedNodeIds) ? options.selectedNodeIds : [];
  const sessionId = typeof options.sessionId === "string" && options.sessionId.trim()
    ? options.sessionId.trim()
    : "";
  const contextAsset = typeof options.contextAsset === "string" && options.contextAsset.trim()
    ? options.contextAsset.trim()
    : "";
  const previewBranch = typeof options.previewBranch === "string" && options.previewBranch.trim()
    ? options.previewBranch.trim()
    : "";
  const streamProgress = options.streamProgress === true;
  const onProgress = typeof options.onProgress === "function" ? options.onProgress : null;
  const signal = options.signal || null;
  const agentCliRunner = typeof options.agentCliRunner === "function" ? options.agentCliRunner : null;
  const mcpToolRunner = typeof options.mcpToolRunner === "function" ? options.mcpToolRunner : null;
  const cadCliRunner = typeof options.cadCliRunner === "function" ? options.cadCliRunner : null;
  const projectCliRunner = typeof options.projectCliRunner === "function" ? options.projectCliRunner : null;

  const provider =
    typeof llm.provider === "string" && VALID_LLM_PROVIDERS.has(llm.provider)
      ? llm.provider
      : "auto";
  const secretsToRedact = [
    llm.api_key,
    process.env.OPENAI_API_KEY,
    process.env.GEMINI_API_KEY,
  ];
  const emitProgress = (payload) => {
    if (!onProgress || signal?.aborted) {
      return;
    }
    onProgress(sanitizeValueSecrets({ event: "progress", ...payload }, secretsToRedact));
  };

  const { resolvedProvider, env: llmEnv } = resolveSubprocessLlmEnv(
    process.env,
    provider,
    llm,
  );

  // Never silently fabricate KCL with offline hardcoded templates when LLM is unconfigured.
  if (!hasConfiguredRealLlm(resolvedProvider, llmEnv, llm)) {
    const msg = mode === "edit"
      ? "修改模型 (edit) 需要配置真实 LLM（OpenAI / Gemini），已禁用离线假模板静默回退。"
      : "AI 建模需要配置真实 LLM（OpenAI / Gemini）API Key 或兼容接口，已禁用离线假模板静默回退。";
    const err = new Error(msg);
    err.statusCode = 422;
    err.payload = {
      ok: false,
      error: msg,
      error_code: mode === "edit" ? "edit_requires_llm" : "llm_not_configured",
      mode,
    };
    throw err;
  }

  let resolvedExplicitProjectPath = "";
  if (explicitProjectRelPath) {
    resolvedExplicitProjectPath = await canonicalProjectPath(
      workspaceRoot,
      explicitProjectRelPath,
      "projectPath",
    );
  }
  if (contextAsset) {
    const assetsDir = await openAssetDirectory(workspaceRoot);
    await readAssetPath(assetsDir, contextAsset);
  }

  const pythonCandidates = [
    ...(process.env.CAD_AGENT_PYTHON ? [process.env.CAD_AGENT_PYTHON] : []),
    "python",
    "python3",
    "py",
  ];
  const agentSrc = path.join(REPO_ROOT, "services", "cad-agent", "src");
  const cliPyPath = path.join(agentSrc, "cad_agent", "cli.py");
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-agent-ui-"));
  let ephemeralProjectDir = null;

  try {
    let generatedSource = "";
    let generatorBackend = "cad-agent-v2";
    let agentSummary = null;
    let changePreview = null;
    let agentError = null;
    let v2Payload = null;

    if (streamProgress) {
      emitProgress({
        stage: "init",
        message:
          mode === "edit"
            ? "正在启动 AI V2 语义增量修改流程..."
            : "正在启动 AI V2 语义参数化建模流程...",
      });
    }
    if (signal?.aborted) {
      return null;
    }

    const cliBin = resolveCadCliBinary() || (agentCliRunner || cadCliRunner ? "cad-cli" : null);
    if (!cliBin) {
      const err = new Error("Compiled cad-cli binary not found in target/debug or target/release.");
      err.statusCode = 503;
      err.payload = { ok: false, error: err.message, error_code: "cad_cli_unavailable", mode };
      throw err;
    }

    let effectiveProjectPath = resolvedExplicitProjectPath;
    if (!effectiveProjectPath) {
      ephemeralProjectDir = await fs.mkdtemp(path.join(workspaceRoot, ".aicad-v2-tmp-"));
      effectiveProjectPath = path.join(ephemeralProjectDir, "session.aicad");
    }

    let target = selectedNodeIds[0] || selectedTopoRefs[0] || "";
    if (mode === "edit" && !resolvedExplicitProjectPath && existingSource.trim() && !agentCliRunner) {
      const discoveredTarget = await stageExistingKclIntoProject({
        workspaceRoot,
        projectPath: effectiveProjectPath,
        existingSource,
        cliBin,
        mcpToolRunner,
      });
      if (!target && discoveredTarget) {
        target = discoveredTarget;
      }
    }

    if (existsSync(cliPyPath) || agentCliRunner) {
      const caps = agentCliRunner
        ? { hasOutDxf: false, hasOutDrawing: false }
        : await detectCliCapabilities(cliBin, cadCliRunner);
      // Security: inject API key strictly via child process env, never CLI args.
      const env = {
        ...llmEnv,
        PYTHONPATH: agentSrc,
        PYTHONUTF8: "1",
        AICAD_PROJECT_PATH: effectiveProjectPath,
        ...(selectedTopoRefs.length > 0
          ? { AICAD_SELECTED_TOPO_REFS: JSON.stringify(selectedTopoRefs) }
          : {}),
        ...(selectedNodeIds.length > 0
          ? { AICAD_SELECTED_NODE_IDS: JSON.stringify(selectedNodeIds) }
          : {}),
        ...(sessionId ? { AICAD_SESSION_ID: sessionId } : {}),
      };

      const candidates = agentCliRunner ? [pythonCandidates[0] || "python"] : pythonCandidates;
      for (const pyBin of candidates) {
        if (signal?.aborted) {
          return null;
        }
        const supportsProgress = streamProgress
          ? (agentCliRunner ? true : await detectAgentCliSupportsProgress(pyBin, env, cliPyPath))
          : false;
        const args = buildAgentCliArgs({
          prompt,
          material,
          manufacturingMethod,
          mode,
          resolvedProvider,
          workDir: tmpDir,
          cliBin,
          workspaceRoot,
          projectPath: effectiveProjectPath,
          target: target || undefined,
          previewBranch: previewBranch || undefined,
          contextAsset: contextAsset || undefined,
          llm,
          includeDrawings: caps.hasOutDxf && caps.hasOutDrawing,
          progressStderr: supportsProgress,
        });

        let stdout = "";
        if (agentCliRunner) {
          const customRes = await agentCliRunner(pyBin, args, {
            env,
            cwd: REPO_ROOT,
            signal,
            onProgressLine: streamProgress
              ? (line) => {
                  const ev = parseAgentProgressLine(line);
                  if (ev) emitProgress(ev);
                }
              : undefined,
          });
          if (customRes?.aborted || signal?.aborted) {
            return null;
          }
          if (customRes?.enoent) {
            continue;
          }
          stdout = customRes?.stdout ?? "";
        } else if (streamProgress) {
          const spawnRes = await runAgentCliSpawn(pyBin, args, {
            env,
            cwd: REPO_ROOT,
            signal,
            onProgressLine: (line) => {
              const ev = parseAgentProgressLine(line);
              if (ev) {
                emitProgress(ev);
              }
            },
          });
          if (spawnRes.aborted || signal?.aborted) {
            return null;
          }
          if (spawnRes.enoent) {
            continue;
          }
          stdout = spawnRes.stdout ?? "";
        } else {
          try {
            const res = await execFileAsync(pyBin, args, {
              env, cwd: REPO_ROOT, maxBuffer: 10 * 1024 * 1024,
            });
            stdout = res.stdout ?? "";
          } catch (err) {
            if (err && err.code === "ENOENT") {
              continue;
            }
            stdout = err.stdout ?? "";
          }
        }
        try {
          const payload = JSON.parse(stdout || "{}");
          if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
            continue;
          }
          const verdictStr = String(payload.overall_status ?? payload.verdict ?? "").toUpperCase();
          const hasExplicitError =
            (typeof payload.error === "string" && payload.error.trim().length > 0) ||
            (typeof payload.error_code === "string" && payload.error_code.trim().length > 0);
          if (hasExplicitError) {
            const remainingUnsupported = Array.isArray(payload.unsupported_features)
              ? payload.unsupported_features.filter((id) => !NATIVE_SUPPORTED_FEATURES.has(id))
              : null;
            agentError = remainingUnsupported
              ? { ...payload, unsupported_features: remainingUnsupported }
              : payload;
            break;
          }
          if (verdictStr === "FAIL" || (payload.passed === false && verdictStr !== "UNKNOWN")) {
            agentError = {
              ...payload,
              error: typeof payload.explanation === "string" && payload.explanation.trim()
                ? payload.explanation
                : (mode === "edit"
                    ? "V2 语义修改未通过几何门禁校验，已保留原分支。"
                    : "V2 语义建模未通过几何门禁校验。"),
              error_code: mode === "edit" ? "edit_validation_failed" : "verification_failed",
            };
            break;
          }
          if (
            typeof payload.kcl_code === "string" ||
            payload.runtime_path === "semantic_v2" ||
            typeof payload.branch === "string" ||
            typeof payload.preview_branch === "string" ||
            payload.verification !== undefined
          ) {
            v2Payload = payload;
            agentSummary = buildAgentSummary(payload, mode);
            changePreview = buildChangePreviewPayload(payload, {
              mode,
              explicitProjectRelPath,
              selectedTopoRefs,
              selectedNodeIds,
              sessionId,
            });
            if (typeof payload.kcl_code === "string" && payload.kcl_code.trim()) {
              generatedSource = payload.kcl_code;
            } else {
              generatedSource = await extractPreviewKclFromProject(
                effectiveProjectPath,
                changePreview.preview_branch,
                tmpDir,
                cliBin,
                projectCliRunner,
              );
            }
            break;
          }
        } catch {
          // Try the next interpreter when no structured result was returned.
        }
      }
    }

    if (signal?.aborted) {
      return null;
    }

    if (agentError) {
      const safeMsg = sanitizeSecrets(agentError.error || "AI agent execution failed", secretsToRedact);
      const err = new Error(safeMsg);
      err.statusCode = 422;
      err.payload = {
        ok: false,
        error: safeMsg,
        error_code: agentError.error_code || "agent_error",
        ...(Array.isArray(agentError.unsupported_features)
          ? { unsupported_features: agentError.unsupported_features }
          : {}),
        mode: agentError.mode ?? mode,
        agentSummary: buildAgentSummary(agentError, mode),
        ...(agentError.verification || agentError.diff || agentError.branch || agentError.preview_branch
          ? {
              changePreview: buildChangePreviewPayload(agentError, {
                mode,
                explicitProjectRelPath,
                selectedTopoRefs,
                selectedNodeIds,
                sessionId,
              }),
            }
          : {}),
      };
      throw err;
    }

    if (!v2Payload || !generatedSource.trim()) {
      const msg = mode === "edit"
        ? "AI V2 语义修改未返回有效模型预览，已保留当前工程状态。"
        : `AI V2 语义建模 (${resolvedProvider}) 未返回有效几何结果，已中止（不静默回退到离线假模板）。请检查 API Key、模型配置与后端日志。`;
      const err = new Error(msg);
      err.statusCode = 422;
      err.payload = {
        ok: false,
        error: msg,
        error_code: mode === "edit" ? "edit_validation_failed" : "llm_generation_failed",
        mode,
        llm_provider: resolvedProvider,
      };
      throw err;
    }

    if (streamProgress) {
      emitProgress({
        stage: "compiling",
        message: "正在编译 V2 预览几何体并生成视口网格...",
      });
    }
    if (signal?.aborted) {
      return null;
    }

    const compileResult = await runCadCliBuild(
      mode === "edit" ? "ai_edited.kcl" : "ai_generated.kcl",
      generatedSource,
      workspaceRoot,
      undefined,
      /\bimport\s*::\s*mesh\b/.test(generatedSource),
      cadCliRunner,
      signal,
    );
    if (signal?.aborted) {
      return null;
    }
    if (!compileResult.ok) {
      const diagMsg = compileResult.diagnostics?.diagnostics?.[0]?.message || "";
      const safeErrMsg = sanitizeSecrets(
        diagMsg || "V2 预览模型未通过编译校验，已保留当前工程状态。",
        secretsToRedact,
      );
      const err = new Error(safeErrMsg);
      err.statusCode = 422;
      err.payload = {
        ok: false,
        error: safeErrMsg,
        error_code: mode === "edit" ? "edit_validation_failed" : "compile_failed",
        mode,
        generatorBackend,
        agentSummary,
        changePreview,
        candidate_source: generatedSource,
        diagnostics: compileResult.diagnostics,
      };
      throw err;
    }

    if (streamProgress) {
      emitProgress({
        stage: "completed",
        message:
          mode === "edit"
            ? "V2 语义修改与预览网格生成完成"
            : "V2 语义建模与预览网格生成完成",
        passed: compileResult.ok && agentSummary?.passed !== false,
      });
    }

    return {
      ok: compileResult.ok,
      prompt,
      mode,
      generatorBackend,
      agentSummary,
      changePreview,
      source: generatedSource,
      ...compileResult,
    };
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
    if (ephemeralProjectDir) {
      await fs.rm(ephemeralProjectDir, { recursive: true, force: true });
    }
  }
}

function validateSelectionStringList(raw, fieldName, { nodeIds = false } = {}) {
  if (raw === undefined || raw === null) {
    return { ok: true, values: [] };
  }
  if (!Array.isArray(raw)) {
    return { ok: false, error: `Invalid '${fieldName}': expected string array` };
  }
  if (raw.length > MAX_SELECTED_REFS) {
    return {
      ok: false,
      error: `Invalid '${fieldName}': at most ${MAX_SELECTED_REFS} items allowed`,
    };
  }
  const seen = new Set();
  const out = [];
  for (const item of raw) {
    if (typeof item !== "string") {
      return { ok: false, error: `Invalid '${fieldName}': expected string items` };
    }
    const trimmed = item.trim();
    if (!trimmed || trimmed.length > MAX_SELECTED_REF_LEN || trimmed.includes("..")) {
      return { ok: false, error: `Invalid '${fieldName}' entry` };
    }
    if (nodeIds ? !SAFE_NODE_ID.test(trimmed) : !SAFE_TOPO_REF.test(trimmed)) {
      return { ok: false, error: `Invalid '${fieldName}' entry` };
    }
    if (!seen.has(trimmed)) {
      seen.add(trimmed);
      out.push(trimmed);
    }
  }
  return { ok: true, values: out };
}

function validateAgentGenerateBody(body) {
  if (!body || typeof body !== "object" || typeof body.prompt !== "string" || !body.prompt.trim()) {
    return {
      ok: false,
      status: 400,
      payload: { ok: false, error: "Missing non-empty 'prompt' field", error_code: "invalid_request" },
    };
  }
  const mode = body.mode === undefined ? "generate" : body.mode;
  if (typeof mode !== "string" || !VALID_AGENT_MODES.has(mode)) {
    return {
      ok: false,
      status: 400,
      payload: {
        ok: false,
        error: "Invalid 'mode': expected 'generate' or 'edit'",
        error_code: "invalid_request",
      },
    };
  }

  const rawProjectPath = body.projectPath !== undefined ? body.projectPath : body.project_path;
  let projectPath = "";
  if (rawProjectPath !== undefined && rawProjectPath !== null) {
    if (typeof rawProjectPath !== "string" || !rawProjectPath.trim()) {
      return {
        ok: false,
        status: 400,
        payload: { ok: false, error: "Invalid 'projectPath': expected non-empty workspace-relative path", error_code: "invalid_request" },
      };
    }
    try {
      assertWorkspaceRelative(rawProjectPath.trim(), "projectPath");
      if (/[\x00-\x1f\x7f]/.test(rawProjectPath)) {
        throw httpError(400, "Invalid 'projectPath': control characters are not allowed");
      }
      for (const seg of rawProjectPath.trim().split(/[\\/]/)) {
        if (seg && WINDOWS_RESERVED_SEGMENT.test(seg)) {
          throw httpError(400, "Invalid 'projectPath': reserved device name");
        }
      }
    } catch (pathErr) {
      return {
        ok: false,
        status: 400,
        payload: {
          ok: false,
          error: pathErr instanceof Error ? pathErr.message : "Invalid 'projectPath'",
          error_code: "invalid_request",
        },
      };
    }
    projectPath = rawProjectPath.trim();
  }

  const rawTopoRefs = body.selectedTopoRefs !== undefined ? body.selectedTopoRefs : body.selected_topo_refs;
  const topoRes = validateSelectionStringList(rawTopoRefs, "selectedTopoRefs", { nodeIds: false });
  if (!topoRes.ok) {
    return {
      ok: false,
      status: 400,
      payload: { ok: false, error: topoRes.error, error_code: "invalid_request" },
    };
  }

  const rawNodeIds = body.selectedNodeIds !== undefined ? body.selectedNodeIds : body.selected_node_ids;
  const nodeRes = validateSelectionStringList(rawNodeIds, "selectedNodeIds", { nodeIds: true });
  if (!nodeRes.ok) {
    return {
      ok: false,
      status: 400,
      payload: { ok: false, error: nodeRes.error, error_code: "invalid_request" },
    };
  }

  const rawSessionId = body.sessionId !== undefined ? body.sessionId : body.session_id;
  let sessionId = "";
  if (rawSessionId !== undefined && rawSessionId !== null) {
    try {
      assertSessionId(rawSessionId);
      sessionId = rawSessionId;
    } catch {
      return {
        ok: false,
        status: 400,
        payload: { ok: false, error: "Invalid 'sessionId'", error_code: "invalid_request" },
      };
    }
  }

  const rawContextAsset = body.contextAsset !== undefined ? body.contextAsset : body.context_asset;
  let contextAsset = "";
  if (rawContextAsset !== undefined && rawContextAsset !== null) {
    if (
      typeof rawContextAsset !== "string" ||
      !rawContextAsset.startsWith("assets/") ||
      !isAssetName(rawContextAsset.slice(7))
    ) {
      return {
        ok: false,
        status: 400,
        payload: { ok: false, error: "Invalid 'contextAsset': expected normalized assets/<name> path", error_code: "invalid_request" },
      };
    }
    contextAsset = rawContextAsset;
  }

  const rawPreviewBranch = body.previewBranch !== undefined ? body.previewBranch : body.preview_branch;
  let previewBranch = "";
  if (rawPreviewBranch !== undefined && rawPreviewBranch !== null) {
    try {
      previewBranch = requireSafeBranchName(rawPreviewBranch, "previewBranch");
    } catch (branchErr) {
      return {
        ok: false,
        status: 400,
        payload: {
          ok: false,
          error: branchErr instanceof Error ? branchErr.message : "Invalid 'previewBranch'",
          error_code: "invalid_request",
        },
      };
    }
  }

  const existingSource = typeof body.existing_source === "string" ? body.existing_source : "";
  if (mode === "edit" && !existingSource.trim() && !projectPath) {
    return {
      ok: false,
      status: 400,
      payload: {
        ok: false,
        error: "Edit mode requires non-empty 'existing_source' or 'projectPath' field",
        error_code: "invalid_request",
      },
    };
  }

  let llm = {};
  if (body.llm !== undefined && body.llm !== null) {
    if (typeof body.llm !== "object" || Array.isArray(body.llm)) {
      return {
        ok: false,
        status: 400,
        payload: { ok: false, error: "Invalid 'llm' field: expected object", error_code: "invalid_request" },
      };
    }
    const provider = body.llm.provider === undefined ? "auto" : body.llm.provider;
    if (typeof provider !== "string" || !VALID_LLM_PROVIDERS.has(provider)) {
      return {
        ok: false,
        status: 400,
        payload: {
          ok: false,
          error: "Invalid 'llm.provider': expected 'auto', 'template', 'openai', 'gemini', or 'deterministic'",
          error_code: "invalid_request",
        },
      };
    }
    for (const field of ["model", "base_url", "api_key", "profile_id"]) {
      const val = body.llm[field];
      if (val !== undefined && val !== null && typeof val !== "string") {
        return {
          ok: false,
          status: 400,
          payload: { ok: false, error: `Invalid 'llm.${field}': expected string`, error_code: "invalid_request" },
        };
      }
    }
    if (
      body.llm.enable_vlm !== undefined &&
      body.llm.enable_vlm !== null &&
      typeof body.llm.enable_vlm !== "boolean"
    ) {
      return {
        ok: false,
        status: 400,
        payload: { ok: false, error: "Invalid 'llm.enable_vlm': expected boolean", error_code: "invalid_request" },
      };
    }
    if (
      body.llm.few_shot_k !== undefined &&
      body.llm.few_shot_k !== null &&
      !isValidFewShotK(body.llm.few_shot_k)
    ) {
      return {
        ok: false,
        status: 400,
        payload: {
          ok: false,
          error: "Invalid 'llm.few_shot_k': expected integer between 0 and 5",
          error_code: "invalid_request",
        },
      };
    }
    llm = {
      provider,
      model: typeof body.llm.model === "string" ? body.llm.model : undefined,
      base_url: typeof body.llm.base_url === "string" ? body.llm.base_url : undefined,
      api_key: typeof body.llm.api_key === "string" ? body.llm.api_key : undefined,
      profile_id: typeof body.llm.profile_id === "string" ? body.llm.profile_id : undefined,
      enable_vlm: body.llm.enable_vlm === true,
      few_shot_k:
        typeof body.llm.few_shot_k === "number" ? body.llm.few_shot_k : undefined,
    };
  }
  const provider = llm.provider || "auto";
  if (isUnconfiguredTemplateRequest(mode, provider, llm)) {
    const unsupported = checkUnsupportedTemplateFeatures(body.prompt);
    if (unsupported) {
      return {
        ok: false,
        status: 422,
        payload: { ok: false, ...unsupported },
      };
    }
  }
  return {
    ok: true,
    prompt: body.prompt,
    material: typeof body.material === "string" ? body.material : "Al-6061-T6",
    manufacturingMethod:
      typeof body.manufacturing_method === "string"
        ? body.manufacturing_method
        : "CNC_3AXIS_MILLING",
    mode,
    existingSource,
    projectPath,
    selectedTopoRefs: topoRes.values,
    selectedNodeIds: nodeRes.values,
    sessionId,
    contextAsset,
    previewBranch,
    llm,
  };
}

function writeSseEvent(res, eventName, data, secretsToRedact = []) {
  if (res.writableEnded || res.destroyed) {
    return;
  }
  const safeData = sanitizeValueSecrets(data, secretsToRedact);
  res.write(`event: ${eventName}\ndata: ${JSON.stringify(safeData)}\n\n`);
}

async function readBody(req, limit) {
  if (Number(req.headers["content-length"]) > limit) {
    req.resume();
    throw httpError(413, "Request body is too large");
  }
  const chunks = [];
  let size = 0;
  for await (const chunk of req.iterator({ destroyOnReturn: false })) {
    size += chunk.length;
    if (size > limit) {
      req.resume();
      throw httpError(413, "Request body is too large");
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, size);
}

async function readJsonBody(req) {
  const raw = await readBody(req, MAX_JSON_BYTES);
  if (!raw.length) return {};
  let value;
  try { value = JSON.parse(raw.toString("utf8")); } catch { throw httpError(400, "Invalid JSON request body"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw httpError(400, "Expected a JSON object");
  }
  return value;
}

function sendJson(res, statusCode, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(statusCode, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  res.end(body);
}

const INDEX_HTML = `<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>AI-Native CAD Studio v0.2 — Full-Stack Local Workbench</title>
  <style>
    :root {
      color-scheme: dark;
      --bg-root: #090d14;
      --bg-panel: #101722;
      --bg-card: #151e2d;
      --bg-elev: #1c283b;
      --border: #26334a;
      --text: #e6edf3;
      --muted: #8b9bb4;
      --blue: #3b82f6;
      --cyan: #38bdf8;
      --amber: #f59e0b;
      --green: #22c55e;
      --red: #ef4444;
      font-family: Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    * { box-sizing: border-box; }
    body { margin: 0; background: var(--bg-root); color: var(--text); font-size: 13px; }
    .shell { display: flex; flex-direction: column; height: 100vh; overflow: hidden; }
    .topbar {
      display: flex; align-items: center; justify-content: space-between; gap: 10px;
      padding: 8px 14px; background: #0d131d; border-bottom: 1px solid var(--border);
    }
    .brand { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
    .brand-dot { width: 9px; height: 9px; border-radius: 50%; background: var(--cyan); box-shadow: 0 0 8px var(--cyan); }
    .select, .btn {
      background: var(--bg-elev); color: var(--text); border: 1px solid var(--border);
      border-radius: 6px; padding: 5px 10px; font-size: 12px; cursor: pointer;
    }
    .btn-primary { background: var(--blue); border-color: var(--blue); font-weight: 600; color: #fff; }
    .btn-primary:hover { filter: brightness(1.08); }
    .btn:hover { border-color: var(--cyan); }
    .badge {
      display: inline-flex; align-items: center; padding: 2px 8px; border-radius: 999px;
      font-size: 11px; font-weight: 600; background: var(--bg-elev); color: var(--muted);
    }
    .badge.ok { background: rgba(34,197,94,0.18); color: #86efac; }
    .badge.err { background: rgba(239,68,68,0.2); color: #fca5a5; }
    .badge.info { background: rgba(56,189,248,0.16); color: #7dd3fc; }
    .columns {
      display: grid;
      grid-template-columns: minmax(350px, 1.15fr) minmax(470px, 1.6fr) minmax(305px, 0.95fr);
      gap: 8px; padding: 8px; flex: 1; min-height: 0;
    }
    .col { display: flex; flex-direction: column; gap: 8px; min-height: 0; overflow-y: auto; }
    .card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 10px 12px; }
    .card-hd { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 8px; }
    .card-title { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.05em; color: #cbd5e1; }
    .kv-grid { display: grid; grid-template-columns: repeat(2, minmax(0,1fr)); gap: 6px 10px; margin: 0; }
    .kv-grid dt { font-size: 11px; color: var(--muted); }
    .kv-grid dd { margin: 2px 0 0; font-weight: 500; word-break: break-word; }
    .ai-prompt {
      width: 100%; background: #0d1117; color: var(--text); border: 1px solid var(--border);
      border-radius: 6px; padding: 6px 8px; font-size: 12px; resize: vertical; margin-bottom: 6px;
    }
    .ai-row { display: grid; grid-template-columns: 1fr 1fr auto; gap: 6px; }
    .ai-mode-tabs {
      display: grid; grid-template-columns: 1fr 1fr; gap: 4px; margin-bottom: 6px;
    }
    .ai-mode-btn {
      background: var(--bg-panel); color: var(--muted); border: 1px solid var(--border);
      border-radius: 6px; padding: 4px 8px; font-size: 11px; font-weight: 600; cursor: pointer;
    }
    .ai-mode-btn.active {
      background: rgba(59,130,246,0.18); border-color: var(--blue); color: #bfdbfe;
    }
    .ai-config-details {
      margin-top: 6px; background: var(--bg-panel); border: 1px solid var(--border);
      border-radius: 6px; padding: 6px 8px;
    }
    .ai-config-details summary {
      cursor: pointer; font-size: 11px; color: var(--muted); user-select: none; font-weight: 600;
    }
    .ai-config-grid {
      display: grid; grid-template-columns: 1fr 1fr; gap: 6px; margin-top: 6px;
    }
    .ai-field { display: flex; flex-direction: column; gap: 2px; }
    .ai-field.full { grid-column: 1 / -1; }
    .ai-field label { font-size: 10px; color: var(--muted); }
    .ai-input {
      width: 100%; background: #0d1117; color: var(--text); border: 1px solid var(--border);
      border-radius: 5px; padding: 4px 7px; font-size: 11px;
    }
    .ai-run-summary {
      margin-top: 6px; font-size: 11px; color: var(--muted); line-height: 1.4;
      padding: 4px 6px; border-radius: 5px; background: rgba(13,17,23,0.7); border: 1px solid var(--border);
    }
    .ai-run-summary.err { color: #fca5a5; border-color: rgba(239,68,68,0.45); background: rgba(239,68,68,0.1); }
    .ai-run-summary.ok { color: #86efac; border-color: rgba(34,197,94,0.35); background: rgba(34,197,94,0.08); }
    .ai-progress-log {
      margin-top: 6px; padding: 6px 8px; border-radius: 6px;
      background: #0b1018; border: 1px solid var(--border);
      max-height: 120px; overflow-y: auto; display: flex; flex-direction: column; gap: 4px;
      font-size: 11px; line-height: 1.35;
    }
    .ai-progress-step {
      display: flex; align-items: flex-start; gap: 6px; color: #cbd5e1;
    }
    .ai-progress-dot {
      width: 6px; height: 6px; border-radius: 50%; margin-top: 5px; flex-shrink: 0; background: var(--cyan);
    }
    .ai-progress-dot.ok { background: var(--green); }
    .ai-progress-dot.err { background: var(--red); }
    .ai-progress-iter {
      font-family: ui-monospace, monospace; font-size: 10px; color: #7dd3fc;
      background: rgba(56,189,248,0.14); border-radius: 3px; padding: 0 4px; flex-shrink: 0;
    }
    .diag-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 5px; max-height: 125px; overflow-y: auto; }
    .diag-btn {
      width: 100%; text-align: left; background: var(--bg-panel); border: 1px solid var(--border);
      border-radius: 6px; padding: 6px 8px; color: var(--text); cursor: pointer;
    }
    .diag-btn:hover { border-color: var(--cyan); }
    .diag-top { display: flex; align-items: center; gap: 6px; font-size: 11px; margin-bottom: 2px; }
    .dot { width: 7px; height: 7px; border-radius: 50%; display: inline-block; }
    .dot.error { background: var(--red); }
    .dot.warning { background: var(--amber); }
    .dot.info { background: var(--cyan); }
    .editor-card { display: flex; flex-direction: column; flex: 1; min-height: 230px; }
    .lsp-panel {
      background: #0d1117; border: 1px solid var(--border); border-radius: 6px; padding: 6px 8px; margin-bottom: 6px; font-size: 11px;
    }
    .code-view {
      flex: 1; overflow: auto; background: #0d1117; border: 1px solid var(--border); border-radius: 6px;
      padding: 6px 0; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; line-height: 1.5;
    }
    .code-line { display: flex; padding: 0 8px; white-space: pre; cursor: pointer; }
    .code-line.active { background: rgba(56,189,248,0.18); box-shadow: inset 3px 0 0 var(--cyan); }
    .gutter { width: 32px; flex-shrink: 0; color: #6e7681; text-align: right; margin-right: 10px; user-select: none; }
    .raw-editor {
      flex: 1; width: 100%; min-height: 210px; resize: none; background: #0d1117; color: var(--text);
      border: 1px solid var(--border); border-radius: 6px; padding: 8px;
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; line-height: 1.5;
    }
    .tok-comment { color: #6a9955; font-style: italic; }
    .tok-annotation { color: #dcdcaa; }
    .tok-keyword { color: #c586c0; font-weight: 600; }
    .tok-type { color: #4ec9b0; }
    .tok-namespace { color: #79c0ff; }
    .tok-dimension { color: #b5cea8; font-weight: 600; }
    .tok-number { color: #b5cea8; }
    .tok-constant { color: #ffa657; font-weight: 600; }
    .tok-pipe { color: #ff7b72; font-weight: 700; }
    .tok-string { color: #ce9178; }
    .viewport-bar {
      display: flex; align-items: center; justify-content: space-between; gap: 6px; flex-wrap: wrap;
      background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 6px 10px;
    }
    .stage {
      position: relative; flex: 1; min-height: 360px; background: #090d14;
      border: 1px solid var(--border); border-radius: 8px; overflow: hidden;
    }
    #viewport-canvas { width: 100%; height: 100%; display: block; cursor: grab; touch-action: none; }
    .svg-grid {
      display: grid; grid-template-columns: repeat(2, minmax(0,1fr)); grid-template-rows: repeat(2, minmax(0,1fr));
      gap: 6px; padding: 8px; width: 100%; height: 100%; background: #0b1018;
    }
    .svg-pane {
      background: #0f1623; border: 1px solid var(--border); border-radius: 6px;
      display: flex; flex-direction: column; overflow: hidden;
    }
    .svg-pane-title { padding: 3px 8px; font-size: 11px; color: var(--muted); border-bottom: 1px solid var(--border); }
    .svg-pane-body { flex: 1; display: flex; align-items: center; justify-content: center; padding: 4px; overflow: hidden; }
    .svg-pane-body img { width: 100%; height: 100%; max-height: 100%; object-fit: contain; }
    .drawing-panel {
      width: 100%; height: 100%; overflow: auto; display: flex; align-items: center; justify-content: center;
      padding: 8px; background: #090d14;
    }
    .drawing-panel img { max-width: 100%; max-height: 100%; height: auto; }
    .drag-hud {
      position: absolute; left: 12px; top: 12px; background: rgba(15,23,42,0.92);
      border: 1px solid var(--cyan); border-radius: 6px; padding: 4px 10px;
      font-family: monospace; font-size: 12px; color: #e0f2fe; pointer-events: none;
    }
    .handle-overlay {
      position: absolute; top: 10px; right: 10px; width: 315px; max-height: calc(100% - 20px); overflow-y: auto;
      background: rgba(16,23,34,0.93); backdrop-filter: blur(8px);
      border: 1px solid var(--border); border-radius: 8px; padding: 10px;
    }
    .tier-badge { padding: 2px 6px; border-radius: 4px; font-size: 10px; font-weight: 700; color: #fff; }
    .tier-1 { background: #2563eb; }
    .tier-2 { background: #d97706; }
    .tier-3 { background: #475569; }
    .chip-row { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 6px; }
    .chip {
      border: 1px solid var(--border); background: var(--bg-panel); color: var(--muted);
      border-radius: 4px; padding: 2px 6px; font-size: 11px; cursor: pointer;
    }
    .chip.active { border-color: var(--cyan); color: var(--text); }
    .popover { margin-top: 8px; padding: 8px; background: #0d131d; border: 1px solid var(--border); border-radius: 6px; }
    .choice-btn {
      width: 100%; text-align: left; margin-top: 5px; padding: 6px 8px; border-radius: 5px;
      background: var(--bg-card); border: 1px solid var(--border); color: var(--text); cursor: pointer; font-size: 11px;
    }
    .choice-btn:hover { border-color: var(--amber); }
    .kpi-grid { display: grid; grid-template-columns: repeat(2, minmax(0,1fr)); gap: 8px; }
    .kpi { background: var(--bg-panel); border: 1px solid var(--border); border-radius: 6px; padding: 7px 9px; }
    .kpi-label { font-size: 11px; color: var(--muted); }
    .kpi-val { display: block; font-size: 15px; font-weight: 700; margin-top: 2px; }
    .kpi-sub { font-size: 11px; color: var(--muted); }
    .progress-track { height: 6px; background: var(--bg-panel); border-radius: 999px; overflow: hidden; margin-top: 4px; }
    .progress-fill.normal { height: 100%; background: var(--green); }
    .progress-fill.over { height: 100%; background: var(--red); }
    .bom-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 5px; }
    .bom-item { display: flex; align-items: center; justify-content: space-between; gap: 6px; padding: 5px 8px; background: var(--bg-panel); border: 1px solid var(--border); border-radius: 6px; }
    .bom-item.active { border-color: var(--cyan); }
    .step-preview {
      margin: 8px 0 0; max-height: 150px; overflow: auto; background: #0d1117; border: 1px solid var(--border);
      border-radius: 6px; padding: 8px; font-family: ui-monospace, monospace; font-size: 11px; color: #94a3b8;
    }
  </style>
</head>
<body>
<div class="shell">
  <header class="topbar">
    <div class="brand">
      <span class="brand-dot"></span>
      <strong>AI-Native CAD Studio v0.2</strong>
      <select id="example-select" class="select" aria-label="选择黄金样本零件"></select>
      <button id="recompile-btn" class="btn btn-primary" type="button">编译并刷新 3D</button>
    </div>
    <div style="display:flex;align-items:center;gap:8px;">
      <span id="kind-badge" class="badge info">part</span>
      <span id="sync-status" class="badge info">初始化中...</span>
      <span id="cli-badge" class="badge">cad-cli</span>
    </div>
  </header>

  <div class="columns">
    <!-- 左栏：AI 生成 + 契约 + 诊断 + .kcl 编辑器 + LSP -->
    <aside class="col">
      <section class="card">
        <div class="card-hd">
          <span class="card-title">AI 自然语言生成 / 修改 (L4 Agent)</span>
          <span id="ai-backend-badge" class="badge info">检测中...</span>
        </div>
        <div class="ai-mode-tabs" role="tablist" aria-label="AI 运行模式">
          <button id="ai-mode-generate" class="ai-mode-btn active" type="button" data-mode="generate">新建生成 (generate)</button>
          <button id="ai-mode-edit" class="ai-mode-btn" type="button" data-mode="edit">修改当前代码 (edit)</button>
        </div>
        <textarea id="ai-prompt-input" class="ai-prompt" rows="2" placeholder="描述你想新建的机械零件或装配体...">设计一个 96x68x36mm 壁厚 2.5mm 的铝合金抽壳控制盒，带镜像安装耳和 M4x0.7 螺纹孔</textarea>
        <div class="chip-row" style="margin-top:0;margin-bottom:6px;">
          <button type="button" class="chip ai-preset" data-prompt="设计一个 96x68x36mm 壁厚 2.5mm 的铝合金抽壳控制盒，带镜像安装耳和 M4x0.7 螺纹孔">抽壳控制盒</button>
          <button type="button" class="chip ai-preset" data-prompt="设计一个带底座法兰与弯头的扫掠流道管道歧管">扫掠管道歧管</button>
          <button type="button" class="chip ai-preset" data-prompt="设计一个包含底座和端盖的双零件轴承座装配体，内孔 30mm H7 同轴配合">轴承座装配体</button>
        </div>
        <div class="ai-row">
          <select id="ai-material" class="select"><option value="Al-6061-T6">Al-6061-T6</option><option value="Ti-6Al-4V">Ti-6Al-4V</option><option value="SUS304">SUS304</option></select>
          <select id="ai-method" class="select"><option value="CNC_3AXIS_MILLING">CNC 3轴</option><option value="CNC_5AXIS_MILLING">CNC 5轴</option><option value="ADDITIVE_SLM">3D 打印</option></select>
          <button id="ai-generate-btn" class="btn btn-primary" type="button">AI 生成</button>
        </div>
        <details id="ai-config-details" class="ai-config-details">
          <summary>模型配置 (Provider / Model / Base URL / API Key / VLM / Few-shot)</summary>
          <div class="ai-config-grid">
            <div class="ai-field">
              <label for="ai-provider">Provider</label>
              <select id="ai-provider" class="ai-input">
                <option value="auto">auto (自动检测)</option>
                <option value="template">template (离线模板)</option>
                <option value="openai">openai (OpenAI 兼容)</option>
                <option value="gemini">gemini (Gemini)</option>
              </select>
            </div>
            <div class="ai-field">
              <label for="ai-model">Model (可选)</label>
              <input id="ai-model" class="ai-input" type="text" placeholder="留空使用默认模型" />
            </div>
            <div class="ai-field full">
              <label for="ai-base-url">Base URL (可选，OpenAI 兼容接口)</label>
              <input id="ai-base-url" class="ai-input" type="text" placeholder="https://api.openai.com/v1" />
            </div>
            <div class="ai-field full">
              <label for="ai-api-key">API Key (仅存当前页面内存，留空使用服务端环境变量)</label>
              <input id="ai-api-key" class="ai-input" type="password" autocomplete="off" placeholder="留空使用服务端环境变量" />
            </div>
            <div class="ai-field">
              <label for="ai-few-shot-k">Few-shot 参考样本数</label>
              <select id="ai-few-shot-k" class="ai-input">
                <option value="0">0 (不使用)</option>
                <option value="1">1</option>
                <option value="2" selected>2 (默认)</option>
                <option value="3">3</option>
              </select>
            </div>
            <div class="ai-field" style="justify-content:flex-end;">
              <label for="ai-enable-vlm" style="display:inline-flex;align-items:center;gap:6px;cursor:pointer;padding:4px 0;">
                <input id="ai-enable-vlm" type="checkbox" /> 启用 VLM 四视图审查
              </label>
            </div>
          </div>
        </details>
        <div id="ai-progress-log" class="ai-progress-log" style="display:none;"></div>
        <div id="ai-run-summary" class="ai-run-summary" style="display:none;"></div>
      </section>

      <section class="card">
        <div class="card-hd">
          <span class="card-title">工程设计契约 (Design Contract)</span>
          <span id="mfg-badge" class="badge info">-</span>
        </div>
        <dl id="contract-grid" class="kv-grid"></dl>
      </section>

      <section class="card">
        <div class="card-hd">
          <span class="card-title">编译器与门禁诊断 (<span id="diag-count">0</span>)</span>
          <span id="compile-status-badge" class="badge">-</span>
        </div>
        <ul id="diag-list" class="diag-list"></ul>
      </section>

      <details class="card" id="assets-card">
        <summary class="card-title">外部 3D 资产</summary>
        <p>上传 STEP、BREP、IGES、STL、3MF、OBJ 或 GLB / 内嵌 glTF（最多 32 MiB）</p>
        <input id="asset-file" type="file" aria-label="上传外部 3D 资产" accept=".step,.stp,.brep,.iges,.igs,.stl,.3mf,.obj,.gltf,.glb" />
        <button id="assets-refresh" class="btn" type="button">刷新</button>
        <p id="assets-notice" role="status"></p>
        <ul id="assets-list" class="diag-list"></ul>
        <pre id="assets-inspection" hidden style="white-space:pre-wrap;max-height:180px;overflow:auto;"></pre>
      </details>

      <section class="card editor-card">
        <div class="card-hd">
          <div>
            <span class="card-title">CAD-DSL (.kcl)</span>
            <span id="span-badge" class="badge info" style="display:none;margin-left:6px;"></span>
          </div>
          <div style="display:flex;gap:4px;">
            <button id="lsp-toggle-btn" class="btn" type="button">LSP 补全</button>
            <button id="mode-highlight-btn" class="btn" type="button">高亮视图</button>
            <button id="mode-raw-btn" class="btn" type="button">编辑源码</button>
          </div>
        </div>
        <div id="lsp-panel" class="lsp-panel" style="display:none;"></div>
        <div id="code-view" class="code-view"></div>
        <textarea id="raw-editor" class="raw-editor" style="display:none;" spellcheck="false"></textarea>
      </section>
    </aside>

    <!-- 中栏：3D Canvas (直接拖拽手柄 + 剖切 + 爆炸图) + SVG 四视图 + 2D 工程图 -->
    <main class="col">
      <div class="viewport-bar">
        <div style="display:flex;gap:4px;flex-wrap:wrap;">
          <button id="tab-3d-btn" class="btn btn-primary" type="button">3D 视口</button>
          <button id="tab-svg-btn" class="btn" type="button">四视图 SVG</button>
          <button id="tab-drawing-btn" class="btn" type="button">2D 工程图</button>
          <button class="btn cam-btn" data-cam="iso" type="button">ISO</button>
          <button class="btn cam-btn" data-cam="top" type="button">TOP</button>
          <button class="btn cam-btn" data-cam="front" type="button">FRONT</button>
          <button class="btn cam-btn" data-cam="right" type="button">RIGHT</button>
        </div>
        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;font-size:11px;">
          <label style="display:inline-flex;align-items:center;gap:4px;">
            <input id="section-toggle" type="checkbox" /> 剖切
          </label>
          <select id="section-axis" class="select" style="padding:2px 6px;display:none;">
            <option value="Z">Z 轴</option>
            <option value="X">X 轴</option>
          </select>
          <input id="section-slider" type="range" min="0.05" max="0.95" step="0.02" value="0.55" style="width:76px;display:none;" />
          <label id="explode-wrap" style="display:none;align-items:center;gap:4px;">
            爆炸图 <input id="explode-slider" type="range" min="0" max="1" step="0.05" value="0" style="width:76px;" />
          </label>
        </div>
      </div>

      <div class="stage">
        <canvas id="viewport-canvas" width="840" height="500"></canvas>
        <div id="drag-hud" class="drag-hud" style="display:none;"></div>
        <div id="svg-views-panel" class="svg-grid" style="display:none;">
          <div class="svg-pane"><div class="svg-pane-title">FRONT</div><div id="svg-front" class="svg-pane-body"></div></div>
          <div class="svg-pane"><div class="svg-pane-title">TOP</div><div id="svg-top" class="svg-pane-body"></div></div>
          <div class="svg-pane"><div class="svg-pane-title">RIGHT</div><div id="svg-right" class="svg-pane-body"></div></div>
          <div class="svg-pane"><div class="svg-pane-title">ISO</div><div id="svg-iso" class="svg-pane-body"></div></div>
        </div>
        <div id="drawing-panel" class="drawing-panel" style="display:none;"></div>

        <div id="handle-overlay" class="handle-overlay" style="display:none;">
          <div class="card-hd" style="margin-bottom:6px;">
            <span id="handle-tier-badge" class="tier-badge tier-1">Tier 1</span>
            <strong id="handle-name" style="font-size:12px;">-</strong>
          </div>
          <div style="display:flex;align-items:center;gap:8px;">
            <input id="handle-slider" type="range" style="flex:1;" />
            <span id="handle-val-label" style="font-family:monospace;min-width:66px;text-align:right;">0</span>
          </div>
          <div style="display:flex;gap:6px;margin-top:6px;">
            <input id="handle-num-input" type="number" step="0.5" class="select" style="flex:1;" />
            <button id="handle-apply-btn" class="btn btn-primary" type="button">写回 AST</button>
          </div>
          <div id="handle-chips" class="chip-row"></div>
          <div id="writeback-popover" class="popover" style="display:none;"></div>
        </div>
      </div>

      <section class="card">
        <div id="selection-status" style="font-size:12px;margin-bottom:6px;color:var(--muted);">
          可在 3D 视口直接拖拽参数锚点 · 点击面/下方标签双向定位源码
        </div>
        <div id="face-chips" class="chip-row"></div>
      </section>
    </main>

    <!-- 右栏：物理仪表盘 + BOM 拆件 + 材料 + STEP/DXF/Drawing 导出 -->
    <aside class="col">
      <section class="card">
        <div class="card-hd">
          <span class="card-title">几何指标与门禁状态</span>
          <span id="watertight-badge" class="badge">-</span>
        </div>
        <div class="kpi-grid">
          <div class="kpi"><span class="kpi-label">体积</span><strong id="kpi-vol" class="kpi-val">-</strong><span id="kpi-vol-mm3" class="kpi-sub">-</span></div>
          <div class="kpi"><span class="kpi-label">估算质量</span><strong id="kpi-mass" class="kpi-val">-</strong><span id="kpi-mass-kg" class="kpi-sub">-</span></div>
          <div class="kpi"><span class="kpi-label">拓扑面 / 边</span><strong id="kpi-topo" class="kpi-val">-</strong><span id="kpi-open-edges" class="kpi-sub">-</span></div>
          <div class="kpi"><span class="kpi-label">包围盒 (mm)</span><strong id="kpi-bbox" class="kpi-val" style="font-size:12px;">-</strong><span id="kpi-time" class="kpi-sub">-</span></div>
        </div>
        <div style="margin-top:8px;">
          <div style="display:flex;justify-content:space-between;font-size:11px;color:var(--muted);">
            <span id="env-label">Keep-In 包络占用率</span>
            <span id="env-pct">0%</span>
          </div>
          <div class="progress-track"><div id="env-bar" class="progress-fill normal" style="width:0%;"></div></div>
        </div>
      </section>

      <section id="bom-card" class="card" style="display:none;">
        <div class="card-hd">
          <span class="card-title">装配体 BOM 拆件 (<span id="bom-count">0</span>)</span>
          <button id="bom-reset-btn" class="btn" style="padding:2px 8px;display:none;" type="button">返回整装</button>
        </div>
        <ul id="bom-list" class="bom-list"></ul>
      </section>

      <section class="card">
        <div class="card-hd"><span class="card-title">工程材料属性</span></div>
        <select id="material-select" class="select" style="width:100%;margin-bottom:6px;">
          <option value="2.70" data-material="Al-6061-T6">Al-6061-T6 (2.70 g/cm³)</option>
          <option value="2.81" data-material="Al-7075-T651">Al-7075-T651 (2.81 g/cm³)</option>
          <option value="7.93" data-material="SUS304">SUS304 不锈钢 (7.93 g/cm³)</option>
          <option value="4.43" data-material="Ti-6Al-4V">Ti-6Al-4V 钛合金 (4.43 g/cm³)</option>
        </select>
        <div style="font-size:11px;color:var(--muted);">质量实时基于 cad-cli 精确体积计算</div>
      </section>

      <section class="card" style="flex:1;display:flex;flex-direction:column;">
        <div class="card-hd">
          <span class="card-title">工程交付导出 (STEP / DXF / 图纸)</span>
          <span id="step-size-badge" class="badge">0 B</span>
        </div>
        <div style="display:flex;flex-direction:column;gap:6px;">
          <button id="download-step-btn" class="btn btn-primary" type="button">下载真实 STEP AP242 (.step)</button>
          <button id="download-dxf-btn" class="btn" type="button">下载 1:1 激光切割 DXF (.dxf)</button>
          <button id="download-drawing-btn" class="btn" type="button">下载 2D 工程制图 (.svg)</button>
          <button id="download-3mf-btn" class="btn" type="button">下载 3D 打印 3MF (.3mf)</button>
          <button id="download-mesh-btn" class="btn" type="button">下载 Scene-Mesh JSON</button>
        </div>
        <pre id="step-preview" class="step-preview">等待编译生成 STEP AP242 内容...</pre>
      </section>
    </aside>
  </div>
</div>

<script>
const state = {
  examples: [],
  currentFile: "",
  source: "",
  aiMode: "generate",
  aiConfig: null,
  compileData: null,
  selectedFaceId: null,
  selectedComponentId: null,
  activeComponentFilter: null,
  highlightedSpan: null,
  activeHandleId: null,
  editorMode: "highlight",
  lspVisible: false,
  lspRequest: 0,
  modelRequest: 0,
  compiledSource: null,
  assemblyComponents: [],
  viewportTab: "3d",
  camera: { yaw: 0.65, pitch: 0.58, zoom: 1.0 },
  sectionCut: { enabled: false, axis: "Z", ratio: 0.55 },
  explodeFactor: 0,
  projectedTris: [],
  projectedHandles: [],
  draftValues: {},
};

const KW = new Set(["part","assembly","component","sketch","let","return","import","const","for","in","true","false","ellipse","profile::ellipse","spline","profile::spline","path::helix","path::spline3d"]);
const TY = new Set(["Length","Angle","Force","Pressure","Solid","Assembly","Component","Sketch","Face","Edge","Point","u32","i32","f64","bool","String"]);
const CO = new Set(["X_AXIS","Y_AXIS","Z_AXIS","XY_PLANE","XZ_PLANE","YZ_PLANE"]);
const TOK_RE = /(\\/\\/.*$|@[a-zA-Z_][\\w]*(?:::[a-zA-Z_][\\w]*)*|"(?:[^"\\\\]|\\\\.)*"|\\|>|->|[+-][XYZ]\\b|\\b[a-zA-Z_][\\w]*::[a-zA-Z_][\\w]*|\\b\\d[\\d_]*(?:\\.\\d[\\d_]*)?(?:[eE][+-]?\\d[\\d_]*)?(?:mm|kN|MPa|deg|rad|in|m|N)\\b|\\b\\d[\\d_]*(?:\\.\\d[\\d_]*)?(?:[eE][+-]?\\d[\\d_]*)?\\b|\\b[a-zA-Z_][\\w]*\\b)/g;

function escapeHtml(str) {
  return String(str).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#39;");
}

function highlightLineHtml(line) {
  let out = "";
  let last = 0;
  for (const m of line.matchAll(TOK_RE)) {
    const idx = m.index ?? 0;
    if (idx > last) out += escapeHtml(line.slice(last, idx));
    const t = m[0];
    let cls = "";
    if (t.startsWith("//")) cls = "tok-comment";
    else if (t.startsWith("@")) cls = "tok-annotation";
    else if (t.startsWith('"')) cls = "tok-string";
    else if (t === "|>") cls = "tok-pipe";
    else if (/^[+-][XYZ]$/.test(t) || CO.has(t)) cls = "tok-constant";
    else if (t.includes("::")) cls = "tok-namespace";
    else if (/^\\d[\\d_]*(?:\\.\\d[\\d_]*)?(?:[eE][+-]?\\d[\\d_]*)?(?:mm|kN|MPa|deg|rad|in|m|N)$/.test(t)) cls = "tok-dimension";
    else if (/^\\d/.test(t)) cls = "tok-number";
    else if (KW.has(t)) cls = "tok-keyword";
    else if (TY.has(t)) cls = "tok-type";
    out += cls ? '<span class="' + cls + '">' + escapeHtml(t) + "</span>" : escapeHtml(t);
    last = idx + t.length;
  }
  if (last < line.length) out += escapeHtml(line.slice(last));
  return out || "&nbsp;";
}

function parseContractFromSource(source) {
  const partMatch = /^\\s*assembly\\s+([A-Za-z_][\\w]*)/m.exec(source) || /^\\s*part\\s+([A-Za-z_][\\w]*)/m.exec(source);
  const matMatch = /@material\\("([^"]+)"\\)/.exec(source);
  const mfgMatch = /@manufacturing\\([^)]*method\\s*=\\s*"([^"]+)"/.exec(source);
  const unitMatch = /@unit\\(([^)]+)\\)/.exec(source);
  const tolMatches = [...source.matchAll(/@pmi::tolerance\\("([^"]+)"/g)].map(m => m[1]);
  return {
    partName: partMatch ? partMatch[1] : "UnknownPart",
    material: matMatch ? matMatch[1] : "Al-6061-T6",
    method: mfgMatch ? mfgMatch[1] : "CNC_3AXIS_MILLING",
    units: unitMatch ? unitMatch[1] : 'length="mm"',
    tolerances: tolMatches.length ? tolMatches.join(", ") : "ISO 2768-mK",
  };
}

function renderContract() {
  const c = parseContractFromSource(state.source);
  document.getElementById("mfg-badge").textContent = c.method;
  document.getElementById("contract-grid").innerHTML =
    "<div><dt>零件 / 装配</dt><dd>" + escapeHtml(state.compileData?.sceneMesh?.part_name || c.partName) + "</dd></div>" +
    "<div><dt>材料</dt><dd>" + escapeHtml(c.material) + "</dd></div>" +
    "<div><dt>量纲体系</dt><dd>" + escapeHtml(c.units) + "</dd></div>" +
    "<div><dt>PMI 公差</dt><dd>" + escapeHtml(c.tolerances) + "</dd></div>";
}

async function postJson(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || response.statusText);
  return data;
}

async function postAgentStream(body, onProgress) {
  const response = await fetch("/api/agent-generate-stream", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (response.status === 404 || response.status === 405) {
    return await postJson("/api/agent-generate", body);
  }
  const ctype = response.headers.get("content-type") || "";
  if (!ctype.includes("text/event-stream") || !response.body) {
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || response.statusText);
    return data;
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let finalResult = null;
  let streamError = null;
  const handleBlock = (block) => {
    const lines = block.split(/\\r?\\n/);
    let eventName = "message";
    const dataLines = [];
    for (const line of lines) {
      if (line.startsWith("event:")) eventName = line.slice(6).trim();
      else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
    }
    if (!dataLines.length) return;
    let parsed;
    try { parsed = JSON.parse(dataLines.join("\\n")); } catch { return; }
    if (eventName === "progress" && typeof onProgress === "function") {
      onProgress(parsed);
    } else if (eventName === "result") {
      finalResult = parsed;
    } else if (eventName === "error") {
      streamError = parsed;
    }
  };
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const parts = buffer.split(/\\r?\\n\\r?\\n/);
    buffer = parts.pop() || "";
    for (const part of parts) {
      if (part.trim()) handleBlock(part);
    }
  }
  buffer += decoder.decode();
  if (buffer.trim()) handleBlock(buffer);
  if (streamError) throw new Error(streamError.error || "AI 请求失败");
  if (!finalResult) throw new Error("SSE 流意外结束，未收到最终结果");
  return finalResult;
}

${applyKclCompletion.toString()}

async function queryLspAt(line, col) {
  const request = ++state.lspRequest;
  const panel = document.getElementById("lsp-panel");
  if (!state.lspVisible) { panel.style.display = "none"; return; }
  const { source, currentFile: file } = state;
  panel.style.display = "block";
  let data;
  try {
    data = await postJson("/api/lsp", { file, source, line, col });
  } catch (err) {
    if (request === state.lspRequest) panel.textContent = err.message;
    return;
  }
  if (request !== state.lspRequest || source !== state.source || file !== state.currentFile) return;
  let html = '<div style="display:flex;justify-content:space-between;margin-bottom:4px;"><strong style="color:var(--cyan);">LSP (' + escapeHtml(data.backend || "analyzer") + ")</strong><span>L" + line + ":" + col + "</span></div>";
  if (data.hover) {
    html += '<div style="margin-bottom:4px;color:#e2e8f0;"><strong>' + escapeHtml(data.hover.symbol) + '</strong> <span class="badge info">' + escapeHtml(data.hover.dimension_type || "") + "</span><div>" + escapeHtml(data.hover.summary || "") + "</div></div>";
  }
  const comps = data.completions || [];
  if (comps.length) {
    html += '<div class="chip-row">' + comps.slice(0, 8).map(c =>
      '<button type="button" class="chip lsp-insert" data-ins="' + escapeHtml(c.insert_text || c.label) + '" title="' + escapeHtml(c.detail || "") + '">' + escapeHtml(c.label) + "</button>"
    ).join("") + "</div>";
  }
  panel.innerHTML = html;
  panel.querySelectorAll(".lsp-insert").forEach(btn => {
    btn.addEventListener("click", () => {
      const ins = btn.getAttribute("data-ins") || "";
      if (source !== state.source || file !== state.currentFile) return;
      const rawEl = document.getElementById("raw-editor");
      const edit = applyKclCompletion(source, line, col, ins);
      state.source = edit.source;
      state.editorMode = "raw";
      renderCodeEditor();
      rawEl.focus();
      const caret = state.source.slice(0, edit.caret).replace(/\\r\\n/g, "\\n").length;
      rawEl.setSelectionRange(caret, caret);
      refreshEditorLsp();
    });
  });
}

let lspTimer;
function refreshEditorLsp() {
  clearTimeout(lspTimer);
  ++state.lspRequest;
  if (!state.lspVisible || state.editorMode !== "raw") return;
  lspTimer = setTimeout(() => {
    const raw = document.getElementById("raw-editor");
    const lines = raw.value.slice(0, raw.selectionStart).split("\\n");
    void queryLspAt(lines.length, lines.at(-1).length + 1);
  }, 150);
}

function renderCodeEditor() {
  const codeView = document.getElementById("code-view");
  const rawEl = document.getElementById("raw-editor");
  const spanBadge = document.getElementById("span-badge");
  if (state.highlightedSpan) {
    spanBadge.style.display = "inline-flex";
    spanBadge.textContent = "L" + state.highlightedSpan.start_line + ":" + state.highlightedSpan.start_col;
  } else {
    spanBadge.style.display = "none";
  }

  if (state.editorMode === "raw") {
    codeView.style.display = "none";
    rawEl.style.display = "block";
    if (rawEl.value !== state.source) rawEl.value = state.source;
    return;
  }

  rawEl.style.display = "none";
  codeView.style.display = "block";
  const lines = state.source.split(/\\r?\\n/);
  codeView.innerHTML = lines.map((line, i) => {
    const ln = i + 1;
    const active = state.highlightedSpan && ln >= state.highlightedSpan.start_line && ln <= state.highlightedSpan.end_line;
    return '<div class="code-line' + (active ? " active" : "") + '" data-line="' + ln + '">' +
      '<span class="gutter">' + ln + "</span><code>" + highlightLineHtml(line) + "</code></div>";
  }).join("");

  codeView.querySelectorAll(".code-line").forEach(el => {
    el.addEventListener("click", () => {
      const ln = Number(el.getAttribute("data-line") || "1");
      const m = /[a-zA-Z_][\\w:]*/.exec(lines[ln - 1] || "");
      void queryLspAt(ln, m ? (m.index || 0) + 1 : 1);
    });
  });

  const activeEl = codeView.querySelector(".code-line.active");
  if (activeEl) activeEl.scrollIntoView({ block: "nearest" });
}

function renderDiagnostics() {
  const d = state.compileData?.diagnostics;
  const list = document.getElementById("diag-list");
  const countEl = document.getElementById("diag-count");
  const statusBadge = document.getElementById("compile-status-badge");
  if (!d) return;

  countEl.textContent = String(d.diagnostics.length);
  statusBadge.textContent = d.status === "ok" ? "编译通过" : "存在错误";
  statusBadge.className = "badge " + (d.status === "ok" ? "ok" : "err");

  if (d.diagnostics.length === 0) {
    list.innerHTML = '<li style="color:var(--muted);font-size:12px;">零报错、零警告（全部约束与门禁通过）</li>';
    return;
  }

  list.innerHTML = d.diagnostics.map((item, idx) =>
    '<li><button class="diag-btn" data-diag-idx="' + idx + '" type="button">' +
    '<div class="diag-top"><span class="dot ' + escapeHtml(item.severity) + '"></span>' +
    '<strong>' + escapeHtml(item.code) + '</strong>' +
    '<span style="margin-left:auto;color:var(--muted);font-family:monospace;">' + (item.span ? "L" + item.span.start_line + ":" + item.span.start_col : "IPC") + "</span></div>" +
    '<div style="color:var(--muted);font-size:12px;">' + escapeHtml(item.message) + "</div></button></li>"
  ).join("");

  list.querySelectorAll(".diag-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const idx = Number(btn.getAttribute("data-diag-idx"));
      const item = d.diagnostics[idx];
      if (item?.span) {
        state.highlightedSpan = item.span;
        state.editorMode = "highlight";
        renderCodeEditor();
      }
    });
  });
}

${clipToSection.toString()}
${handleRange.toString()}

function projectPt(x, y, z, cam, cx, cy, center, scale) {
  const dx = x - center[0], dy = y - center[1], dz = z - center[2];
  const cosY = Math.cos(cam.yaw), sinY = Math.sin(cam.yaw);
  const cosP = Math.cos(cam.pitch), sinP = Math.sin(cam.pitch);
  const x1 = dx * cosY - dy * sinY;
  const y1 = dx * sinY + dy * cosY;
  const y2 = y1 * cosP - dz * sinP;
  const z2 = y1 * sinP + dz * cosP;
  return [cx + x1 * scale, cy - z2 * scale, y2];
}

function pointInTri(px, py, a, b, c) {
  const v0x = c[0]-a[0], v0y = c[1]-a[1], v1x = b[0]-a[0], v1y = b[1]-a[1], v2x = px-a[0], v2y = py-a[1];
  const d00 = v0x*v0x+v0y*v0y, d01 = v0x*v1x+v0y*v1y, d02 = v0x*v2x+v0y*v2y, d11 = v1x*v1x+v1y*v1y, d12 = v1x*v2x+v1y*v2y;
  const denom = d00*d11 - d01*d01;
  if (Math.abs(denom) < 1e-6) return false;
  const inv = 1 / denom;
  const u = (d11*d02 - d01*d12) * inv, v = (d00*d12 - d01*d02) * inv;
  return u >= 0 && v >= 0 && u + v <= 1;
}

function getCompOffset(compId, orderMap) {
  if (!compId || state.explodeFactor <= 0) return [0,0,0];
  const idx = orderMap.get(compId) || 0;
  if (idx === 0) return [0,0,0];
  return [idx * 14 * state.explodeFactor, 0, idx * 28 * state.explodeFactor];
}

function renderCanvas3D() {
  const canvas = document.getElementById("viewport-canvas");
  const ctx = canvas.getContext("2d");
  const w = canvas.width, h = canvas.height;
  const bg = ctx.createRadialGradient(w/2, h/2-30, 30, w/2, h/2, w*0.7);
  bg.addColorStop(0, "#162032");
  bg.addColorStop(1, "#090d14");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, w, h);

  const mesh = state.compileData?.sceneMesh;
  const bbox = state.compileData?.metrics?.bbox_mm;
  if (!mesh || !bbox) {
    state.projectedTris = [];
    state.projectedHandles = [];
    return;
  }

  const orderMap = new Map();
  for (const c of mesh.components || []) orderMap.set(c.component_id, orderMap.size);
  for (const f of mesh.faces || []) {
    if (f.component_id && !orderMap.has(f.component_id)) orderMap.set(f.component_id, orderMap.size);
  }

  const center = [(bbox.min[0]+bbox.max[0])/2, (bbox.min[1]+bbox.max[1])/2, (bbox.min[2]+bbox.max[2])/2];
  const spanX = Math.max(10, bbox.max[0]-bbox.min[0]);
  const spanY = Math.max(10, bbox.max[1]-bbox.min[1]);
  const spanZ = Math.max(10, bbox.max[2]-bbox.min[2]);
  const maxSpan = Math.max(spanX, spanY, spanZ);
  const scale = (Math.min(w, h) * 0.56 / maxSpan) * state.camera.zoom;
  const cx = w / 2 - 40, cy = h / 2 + 10;

  const cutCoord = state.sectionCut.axis === "Z"
    ? bbox.min[2] + state.sectionCut.ratio * (bbox.max[2] - bbox.min[2])
    : bbox.min[0] + state.sectionCut.ratio * (bbox.max[0] - bbox.min[0]);

  const tris = [];
  const light = [0.42, -0.52, 0.74];
  for (const face of mesh.faces || []) {
    const [ox, oy, oz] = getCompOffset(face.component_id, orderMap);
    const v = face.vertices, n = face.normals, idx = face.indices;
    for (let t = 0; t + 2 < idx.length; t += 3) {
      const i0 = idx[t]*3, i1 = idx[t+1]*3, i2 = idx[t+2]*3;
      const v0x = v[i0]+ox, v0y = v[i0+1]+oy, v0z = v[i0+2]+oz;
      const v1x = v[i1]+ox, v1y = v[i1+1]+oy, v1z = v[i1+2]+oz;
      const v2x = v[i2]+ox, v2y = v[i2+1]+oy, v2z = v[i2+2]+oz;
      const points = [[v0x, v0y, v0z], [v1x, v1y, v1z], [v2x, v2y, v2z]];
      const visible = state.sectionCut.enabled
        ? clipToSection(points, state.sectionCut.axis === "Z" ? 2 : 0, cutCoord)
        : points;
      if (visible.length < 3) continue;
      const projected = visible.map(pt => projectPt(...pt, state.camera, cx, cy, center, scale));
      const nx = (n[i0]+n[i1]+n[i2])/3, ny = (n[i0+1]+n[i1+1]+n[i2+1])/3, nz = (n[i0+2]+n[i1+2]+n[i2+2])/3;
      const shade = Math.min(1, 0.34 + Math.abs(nx*light[0] + ny*light[1] + nz*light[2])*0.66);
      for (let i = 1; i + 1 < projected.length; i += 1) {
        const pts = [projected[0], projected[i], projected[i + 1]];
        tris.push({ face, pts, depth: (pts[0][2] + pts[1][2] + pts[2][2]) / 3, shade });
      }
    }
  }
  tris.sort((a,b) => a.depth - b.depth);
  state.projectedTris = tris;

  for (const tri of tris) {
    const selected = state.selectedFaceId === tri.face.face_id || (state.selectedComponentId && tri.face.component_id === state.selectedComponentId);
    const tagged = (tri.face.provenance_tags || []).length > 0;
    if (selected) ctx.fillStyle = "rgba(250, 204, 21, " + (0.55 + tri.shade*0.35).toFixed(2) + ")";
    else if (tagged) ctx.fillStyle = "rgb(64," + Math.round(135+tri.shade*70) + "," + Math.round(185+tri.shade*60) + ")";
    else {
      const b = Math.round(92 + tri.shade*95);
      ctx.fillStyle = "rgb(" + b + "," + Math.round(b*1.07) + "," + Math.round(b*1.18) + ")";
    }
    ctx.beginPath();
    ctx.moveTo(tri.pts[0][0], tri.pts[0][1]);
    ctx.lineTo(tri.pts[1][0], tri.pts[1][1]);
    ctx.lineTo(tri.pts[2][0], tri.pts[2][1]);
    ctx.closePath();
    ctx.fill();
  }

  for (const edge of mesh.edges || []) {
    const [ox, oy, oz] = getCompOffset(edge.component_id, orderMap);
    ctx.strokeStyle = (edge.provenance_tags || []).length ? "#38bdf8" : "#cbd5e1";
    ctx.lineWidth = 1.25;
    const pl = edge.polyline || [];
    ctx.beginPath();
    const stepPts = pl.length && Array.isArray(pl[0]) ? pl : (() => {
      const arr = [];
      for (let i=0; i+2<pl.length; i+=3) arr.push([pl[i],pl[i+1],pl[i+2]]);
      return arr;
    })();
    const points = stepPts.map(pt => [pt[0] + ox, pt[1] + oy, pt[2] + oz]);
    for (let i = 1; i < points.length; i += 1) {
      const segment = [points[i - 1], points[i]];
      const visible = state.sectionCut.enabled
        ? clipToSection(segment, state.sectionCut.axis === "Z" ? 2 : 0, cutCoord, false)
        : segment;
      if (visible.length < 2) continue;
      const [a, b] = visible.map(pt => projectPt(...pt, state.camera, cx, cy, center, scale));
      ctx.moveTo(a[0], a[1]);
      ctx.lineTo(b[0], b[1]);
    }
    ctx.stroke();
  }

  if (state.sectionCut.enabled) {
    const r = maxSpan * 0.65;
    const quad = state.sectionCut.axis === "Z"
      ? [[center[0]-r,center[1]-r,cutCoord],[center[0]+r,center[1]-r,cutCoord],[center[0]+r,center[1]+r,cutCoord],[center[0]-r,center[1]+r,cutCoord]]
      : [[cutCoord,center[1]-r,bbox.min[2]-4],[cutCoord,center[1]+r,bbox.min[2]-4],[cutCoord,center[1]+r,bbox.max[2]+4],[cutCoord,center[1]-r,bbox.max[2]+4]];
    ctx.save();
    ctx.strokeStyle = "rgba(34,211,238,0.88)";
    ctx.fillStyle = "rgba(34,211,238,0.08)";
    ctx.beginPath();
    quad.forEach((pt, idx) => {
      const p = projectPt(pt[0], pt[1], pt[2], state.camera, cx, cy, center, scale);
      if (idx === 0) ctx.moveTo(p[0], p[1]); else ctx.lineTo(p[0], p[1]);
    });
    ctx.closePath();
    ctx.fill();
    ctx.stroke();
    ctx.restore();
  }

  // Project interactive 3D Edit Handle anchors
  const anchors = [];
  const handles = mesh.edit_handles || [];
  handles.forEach((handle, idx) => {
    let ap = handle.anchor_point;
    if (!ap) {
      const ang = (idx * Math.PI) / 3;
      const rr = Math.max(12, maxSpan * 0.42);
      const zz = bbox.min[2] + ((idx + 1) / (handles.length + 1)) * spanZ;
      ap = [center[0] + rr * Math.cos(ang), center[1] + rr * Math.sin(ang), zz];
    }
    const p = projectPt(ap[0], ap[1], ap[2], state.camera, cx, cy, center, scale);
    const active = state.activeHandleId === handle.handle_id;
    const radius = active ? 8 : 6;
    anchors.push({ handle, x: p[0], y: p[1], radius });
    ctx.save();
    ctx.beginPath();
    if (handle.tier === 1) {
      ctx.fillStyle = active ? "#60a5fa" : "#2563eb";
      ctx.strokeStyle = "#eff6ff";
      ctx.lineWidth = active ? 2.5 : 1.5;
      ctx.arc(p[0], p[1], radius, 0, Math.PI * 2);
      ctx.fill(); ctx.stroke();
    } else if (handle.tier === 2) {
      ctx.strokeStyle = "#f59e0b"; ctx.lineWidth = 2;
      ctx.arc(p[0], p[1], radius + 1.5, 0, Math.PI * 2); ctx.stroke();
      ctx.beginPath(); ctx.fillStyle = "#0ea5e9";
      ctx.arc(p[0], p[1], 3.5, 0, Math.PI * 2); ctx.fill();
    } else {
      ctx.fillStyle = "#64748b"; ctx.strokeStyle = "#e2e8f0"; ctx.lineWidth = 1.5;
      ctx.rect(p[0]-5, p[1]-5, 10, 10); ctx.fill(); ctx.stroke();
    }
    ctx.restore();
  });
  state.projectedHandles = anchors;
}

function renderSvgImage(container, svg, label) {
  if (!svg) {
    container.textContent = "暂无" + label;
    return;
  }
  const image = document.createElement("img");
  image.alt = label;
  image.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
  container.replaceChildren(image);
}

function renderSvgAndDrawing() {
  const views = state.compileData?.views || {};
  for (const k of ["front", "top", "right", "iso"]) {
    renderSvgImage(document.getElementById("svg-" + k), views[k], k.toUpperCase() + " 视图");
  }
  renderSvgImage(document.getElementById("drawing-panel"), state.compileData?.drawingSvg, "2D 工程图");
}

function renderHandles() {
  const handles = state.compileData?.sceneMesh?.edit_handles || [];
  const overlay = document.getElementById("handle-overlay");
  if (!handles.length || state.viewportTab !== "3d") { overlay.style.display = "none"; return; }
  overlay.style.display = "block";
  const active = handles.find(h => h.handle_id === state.activeHandleId) || handles[0];
  state.activeHandleId = active.handle_id;

  const badge = document.getElementById("handle-tier-badge");
  badge.textContent = "Tier " + active.tier;
  badge.className = "tier-badge tier-" + active.tier;
  document.getElementById("handle-name").textContent = active.param_name;

  const slider = document.getElementById("handle-slider");
  const numInput = document.getElementById("handle-num-input");
  const valLabel = document.getElementById("handle-val-label");
  const [minV, maxV] = handleRange(active);
  slider.min = String(minV); slider.max = String(maxV); slider.step = "0.5";
  const displayVal = state.draftValues[active.handle_id] ?? active.current_value;
  slider.value = String(displayVal);
  numInput.value = String(displayVal);
  valLabel.textContent = displayVal + (active.unit === "unitless" ? "" : " " + active.unit);
  document.getElementById("writeback-popover").style.display = "none";

  const chips = document.getElementById("handle-chips");
  chips.innerHTML = handles.map(h =>
    '<button type="button" class="chip' + (h.handle_id === active.handle_id ? " active" : "") + '" data-hid="' + escapeHtml(h.handle_id) + '">T' + h.tier + " · " + escapeHtml(h.param_name) + "</button>"
  ).join("");
  chips.querySelectorAll(".chip").forEach(btn => {
    btn.addEventListener("click", () => {
      state.activeHandleId = btn.getAttribute("data-hid");
      const sel = handles.find(x => x.handle_id === state.activeHandleId);
      if (sel?.ast_span) { state.highlightedSpan = sel.ast_span; renderCodeEditor(); }
      renderHandles();
      renderCanvas3D();
    });
  });
}

function renderFaceChips() {
  const faces = state.compileData?.sceneMesh?.faces || [];
  const box = document.getElementById("face-chips");
  box.innerHTML = faces.slice(0, 12).map(f =>
    '<button type="button" class="chip' + (state.selectedFaceId === f.face_id ? " active" : "") + '" data-fid="' + escapeHtml(f.face_id) + '">' + escapeHtml(f.face_id) + "</button>"
  ).join("");
  box.querySelectorAll(".chip").forEach(btn => {
    btn.addEventListener("click", () => {
      const fid = btn.getAttribute("data-fid");
      const face = faces.find(f => f.face_id === fid);
      if (face) selectFace(face);
    });
  });
}

function selectFace(face) {
  state.selectedFaceId = face ? face.face_id : null;
  const statusEl = document.getElementById("selection-status");
  if (face) {
    state.highlightedSpan = face.ast_span;
    statusEl.innerHTML = '<span class="badge info">FACE</span> <strong>' + escapeHtml(face.face_id) + "</strong> (" + escapeHtml(face.surface_type) + ") · Tags: [" + escapeHtml((face.provenance_tags||[]).join(", ")) + "] · AST L" + face.ast_span.start_line + "..L" + face.ast_span.end_line;
  } else {
    statusEl.textContent = "可在 3D 视口直接拖拽参数锚点 · 点击面/下方标签双向定位源码";
  }
  renderCodeEditor();
  renderCanvas3D();
  renderFaceChips();
}

function renderBomPanel() {
  const mesh = state.compileData?.sceneMesh;
  const comps = state.assemblyComponents;
  const isAsm = mesh?.kind === "assembly" || comps.length > 1;
  document.getElementById("kind-badge").textContent = state.activeComponentFilter ? state.activeComponentFilter : isAsm ? "assembly" : "part";
  document.getElementById("explode-wrap").style.display = isAsm && !state.activeComponentFilter ? "inline-flex" : "none";

  const bomCard = document.getElementById("bom-card");
  if (!comps.length) { bomCard.style.display = "none"; return; }
  bomCard.style.display = "block";
  document.getElementById("bom-count").textContent = String(comps.length);
  document.getElementById("bom-reset-btn").style.display = state.activeComponentFilter ? "inline-flex" : "none";

  const list = document.getElementById("bom-list");
  list.innerHTML = comps.map(c =>
    '<li class="bom-item' + (state.activeComponentFilter === c.component_id || state.selectedComponentId === c.component_id ? " active" : "") + '">' +
    '<button type="button" class="btn bom-pick" data-cid="' + escapeHtml(c.component_id) + '" style="flex:1;text-align:left;"><strong>' + escapeHtml(c.part_name || c.component_id) + '</strong> <span style="color:var(--muted);">#' + escapeHtml(c.component_id) + "</span></button>" +
    '<button type="button" class="btn bom-export" data-cid="' + escapeHtml(c.component_id) + '">拆件编译</button></li>'
  ).join("");

  list.querySelectorAll(".bom-pick").forEach(btn => {
    btn.addEventListener("click", () => {
      const cid = btn.getAttribute("data-cid");
      state.selectedComponentId = state.selectedComponentId === cid ? null : cid;
      renderCanvas3D();
      renderBomPanel();
    });
  });
  list.querySelectorAll(".bom-export").forEach(btn => {
    btn.addEventListener("click", () => {
      const componentId = btn.getAttribute("data-cid");
      void compileCurrent("已拆件编译 #" + componentId, componentId);
    });
  });
}

function renderRightDashboard() {
  const d = state.compileData;
  if (!d) return;
  const m = d.metrics || {};
  const volCm3 = (m.volume_mm3 || 0) / 1000;
  const density = Number.parseFloat(document.getElementById("material-select").value || "2.7");
  const massG = volCm3 * density;

  const wt = document.getElementById("watertight-badge");
  wt.textContent = m.watertight ? "L0 水密流形" : "非水密/错误";
  wt.className = "badge " + (m.watertight ? "ok" : "err");

  document.getElementById("kpi-vol").textContent = volCm3.toFixed(2) + " cm³";
  document.getElementById("kpi-vol-mm3").textContent = (m.volume_mm3 || 0).toFixed(1) + " mm³";
  document.getElementById("kpi-mass").textContent = massG.toFixed(1) + " g";
  document.getElementById("kpi-mass-kg").textContent = (massG / 1000).toFixed(3) + " kg";
  document.getElementById("kpi-topo").textContent = (m.face_count || 0) + "F / " + (m.edge_count || 0) + "E";
  document.getElementById("kpi-open-edges").textContent = typeof m.open_edge_count === "number" ? "Open Edges: " + m.open_edge_count : "Provenance Tagged";
  const mn = m.bbox_mm?.min || [0,0,0], mx = m.bbox_mm?.max || [0,0,0];
  const sx = mx[0]-mn[0], sy = mx[1]-mn[1], sz = mx[2]-mn[2];
  document.getElementById("kpi-bbox").textContent = sx.toFixed(1) + "×" + sy.toFixed(1) + "×" + sz.toFixed(1);
  document.getElementById("kpi-time").textContent = (m.compile_time_ms ?? 0).toFixed(1) + " ms · " + (m.kernel_backend || "rust");

  // R20: unclamped envelopeRatio vs clamped bar width
  const ratio = Math.round(Math.max(sx/160, sy/160, sz/120) * 100);
  document.getElementById("env-label").textContent = ratio > 100 ? "Keep-In 包络占用率（已超限）" : "Keep-In 包络占用率";
  document.getElementById("env-pct").textContent = ratio + "%";
  const envBar = document.getElementById("env-bar");
  envBar.className = "progress-fill " + (ratio > 100 ? "over" : "normal");
  envBar.style.width = Math.min(100, ratio) + "%";

  const step = d.stepContent || "";
  document.getElementById("download-step-btn").disabled = !step;
  const dxfButton = document.getElementById("download-dxf-btn");
  dxfButton.disabled = !d.dxfContent;
  dxfButton.textContent = d.dxfIsCutProfile ? "下载 1:1 切割轮廓 DXF (.dxf)" : d.dxfContent ? "下载网格边线 DXF（仅预览）" : "无可用 DXF";
  document.getElementById("download-drawing-btn").disabled = !d.drawingSvg;
  document.getElementById("download-3mf-btn").disabled = !d.threemfBase64;
  document.getElementById("download-mesh-btn").disabled = !d.sceneMesh;
  document.getElementById("step-size-badge").textContent = step.length + " B";
  document.getElementById("step-preview").textContent = step ? step.slice(0, 2200) + (step.length > 2200 ? "\\n... (共 " + step.length + " 字节)" : "") : "无 STEP 输出";
}

function setSyncStatus(text, kind) {
  const badge = document.getElementById("sync-status");
  badge.textContent = text;
  badge.className = "badge " + kind;
}

function showCompileData(data, noticeText) {
  if (data.file !== state.compileData?.file) {
    const materialSelect = document.getElementById("material-select");
    const material = parseContractFromSource(state.source).material;
    const option = Array.from(materialSelect.options).find((item) => item.dataset.material === material);
    materialSelect.value = option?.value || "2.70";
  }
  state.compileData = data;
  state.compiledSource = state.source;
  state.activeComponentFilter = data.componentId;
  state.selectedFaceId = null;
  state.draftValues = {};
  if (!data.componentId) {
    state.assemblyComponents = data.sceneMesh?.components || data.diagnostics?.components || [];
  }
  document.getElementById("cli-badge").textContent = data.cliBinary || "cad-cli";
  setSyncStatus(noticeText, data.ok && data.agentSummary?.passed !== false ? "ok" : "err");

  renderContract();
  renderDiagnostics();
  renderCodeEditor();
  refreshEditorLsp();
  renderSvgAndDrawing();
  renderHandles();
  selectFace(null);
  renderBomPanel();
  renderRightDashboard();
}

async function compileCurrent(noticeText, componentId = state.activeComponentFilter, previewOnly) {
  const request = ++state.modelRequest;
  const { source, currentFile: file } = state;
  setSyncStatus("正在调用 cad-cli 编译...", "info");
  try {
    const data = await postJson("/api/compile", { file, source, componentId, previewOnly });
    if (request !== state.modelRequest || file !== state.currentFile) return;
    if (source !== state.source) { setSyncStatus("源码有新改动，请重新编译", "info"); return; }
    const exportFailed = data.sceneMesh && data.diagnostics?.diagnostics?.some((item) => item.code === "E401_EXPORT_FAILED");
    showCompileData(data, data.ok
      ? noticeText || "cad-cli 编译完成 (" + (data.metrics?.compile_time_ms ?? 0) + " ms)"
      : exportFailed ? "模型已加载，部分导出失败（见诊断）" : "存在编译/门禁诊断");
  } catch (err) {
    if (request === state.modelRequest) setSyncStatus(err.message, "err");
  }
}

async function applyHandleWriteback(tier2Choice, overrideVal) {
  const handles = state.compileData?.sceneMesh?.edit_handles || [];
  const active = handles.find(h => h.handle_id === state.activeHandleId);
  if (!active) return;
  const source = state.source;
  const compileData = state.compileData;
  if (source !== state.compiledSource || state.currentFile !== compileData.file) {
    setSyncStatus("请先编译当前源码，再写回参数", "err");
    return;
  }
  const targetValue = typeof overrideVal === "number" ? overrideVal : Number.parseFloat(document.getElementById("handle-num-input").value);
  const pop = document.getElementById("writeback-popover");

  const resp = await fetch("/api/writeback", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      source: state.source,
      handle: active,
      targetValue,
      tier2Choice: tier2Choice || null,
      extractParamName: document.getElementById("extract-name-input")?.value || undefined,
    }),
  });
  const plan = await resp.json().catch(() => ({ error: "无法读取写回结果" }));
  if (source !== state.source || compileData !== state.compileData || active.handle_id !== state.activeHandleId) return;
  if (plan.error) {
    pop.style.display = "block";
    pop.innerHTML = '<span style="color:var(--red);">' + escapeHtml(plan.error) + "</span>";
    return;
  }

  if (plan.updatedSource) {
    state.source = plan.updatedSource;
    pop.style.display = "none";
    await compileCurrent("参数已更新 (" + active.param_name + ")");
    return;
  }

  if (plan.kind === "tier2" && Array.isArray(plan.options)) {
    pop.style.display = "block";
    pop.innerHTML = "<strong>Tier 2 仿射偏移：选择写回目标</strong>" +
      plan.options.map(opt =>
        '<button type="button" class="choice-btn" data-t2="' + escapeHtml(opt.choice) + '">' + escapeHtml(opt.label) + "</button>"
      ).join("");
    pop.querySelectorAll("[data-t2]").forEach(b => {
      b.addEventListener("click", () => applyHandleWriteback(b.getAttribute("data-t2"), targetValue));
    });
  } else if (plan.kind === "tier3") {
    pop.style.display = "block";
    pop.innerHTML = "<strong>Tier 3 耦合表达式只读保护</strong><div style='color:var(--muted);font-size:11px;margin:4px 0;'>" +
      escapeHtml(plan.reason) + "</div>" +
      '<div style="display:flex;gap:6px;margin-top:6px;"><input id="extract-name-input" class="select" value="' +
      escapeHtml(plan.refactor.extractedParamName) + '" /><button id="do-extract-btn" type="button" class="btn btn-primary">提取独立参数</button></div>';
    document.getElementById("do-extract-btn").addEventListener("click", async () => {
      const r2 = await fetch("/api/writeback", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          source: state.source,
          handle: active,
          targetValue,
          applyTier3Extract: true,
          extractParamName: document.getElementById("extract-name-input").value,
        }),
      });
      const res2 = await r2.json();
      if (source !== state.source || compileData !== state.compileData || active.handle_id !== state.activeHandleId) return;
      if (res2.error) {
        pop.textContent = res2.error;
      } else if (res2.updatedSource) {
        state.source = res2.updatedSource;
        pop.style.display = "none";
        await compileCurrent("Tier 3 已提取参数并重新编译");
      }
    });
  }
}

async function refreshAiConfig() {
  const badge = document.getElementById("ai-backend-badge");
  try {
    const res = await fetch("/api/ai-config");
    if (!res.ok) throw new Error("ai-config unavailable");
    const cfg = await res.json();
    state.aiConfig = cfg;
    if (cfg.default_provider === "template") {
      badge.textContent = "离线模板模式";
      badge.className = "badge info";
    } else if (cfg.has_openai_key && cfg.has_gemini_key) {
      badge.textContent = "已检测到 OpenAI & Gemini Key";
      badge.className = "badge ok";
    } else if (cfg.has_openai_key) {
      badge.textContent = "已检测到 OpenAI Key";
      badge.className = "badge ok";
    } else if (cfg.has_gemini_key) {
      badge.textContent = "已检测到 Gemini Key";
      badge.className = "badge ok";
    } else {
      badge.textContent = "离线模板模式";
      badge.className = "badge info";
    }
    const baseUrlInput = document.getElementById("ai-base-url");
    if (baseUrlInput && cfg.openai_base_url) {
      baseUrlInput.placeholder = cfg.openai_base_url;
    }
    const modelInput = document.getElementById("ai-model");
    if (modelInput) {
      const defModel = cfg.default_provider === "gemini" ? cfg.gemini_model : cfg.openai_model;
      if (defModel) modelInput.placeholder = "默认: " + defModel;
    }
  } catch {
    badge.textContent = "离线模板模式";
    badge.className = "badge info";
  }
}

function initAssets() {
  const card = document.getElementById("assets-card");
  const list = document.getElementById("assets-list");
  const upload = document.getElementById("asset-file");
  const notice = document.getElementById("assets-notice");
  const inspection = document.getElementById("assets-inspection");
  const refreshButton = document.getElementById("assets-refresh");
  let busy = false;
  const run = async (action) => {
    if (busy) return;
    busy = upload.disabled = refreshButton.disabled = true;
    notice.textContent = "";
    try { await action(); }
    catch (err) { notice.textContent = err.message; }
    finally { busy = upload.disabled = refreshButton.disabled = false; }
  };
  const actionButton = (text, click) => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn";
    btn.textContent = text;
    btn.addEventListener("click", click);
    return btn;
  };
  const refresh = async () => {
    const response = await fetch("/api/assets");
    const data = await response.json();
    if (!response.ok) throw new Error(data.error);
    const meshFormats = new Set(["stl", "3mf", "obj", "gltf"]);
    const insertSource = (asset, sourceText) => {
      if (busy) return;
      const current = state.editorMode === "raw" ? document.getElementById("raw-editor").value : state.source;
      if (current.trim() && current !== sourceText && !window.confirm("用导入模型替换当前编辑器源码？")) return;
      state.source = sourceText;
      state.currentFile = asset.name + ".kcl";
      state.activeComponentFilter = state.activeHandleId = state.highlightedSpan = null;
      state.explodeFactor = 0;
      document.getElementById("explode-slider").value = "0";
      document.getElementById("example-select").value = "";
      renderCodeEditor();
      void compileCurrent("已加载外部资产 " + asset.name, null, true);
    };
    list.replaceChildren(...data.assets.map((asset) => {
      const row = document.createElement("li");
      const code = document.createElement("code");
      code.textContent = asset.expression;
      const openShellSlot = document.createElement("span");
      const insert = actionButton("插入并预览", () => insertSource(asset, asset.source));
      const inspect = actionButton("检查", () => void run(async () => {
        inspection.hidden = true;
        const result = await postJson("/api/inspect-asset", { path: asset.path });
        inspection.textContent = JSON.stringify(result, null, 2);
        inspection.hidden = false;
        openShellSlot.replaceChildren();
        if (meshFormats.has(asset.format) && result && result.watertight === false && asset.previewSource) {
          insert.textContent = "插入并预览（实体）";
          const hint = document.createElement("span");
          hint.textContent = " 该网格非水密开放壳，无法直接作为闭合实体参与布尔，可作为参考网格预览 ";
          const previewBtn = actionButton("作为参考网格预览", () => insertSource(asset, asset.previewSource));
          openShellSlot.append(hint, previewBtn);
          notice.textContent = asset.name + " · 该网格非水密开放壳，无法直接作为闭合实体参与布尔，可作为参考网格预览";
        } else {
          insert.textContent = "插入并预览";
          notice.textContent = asset.name + " · 资产检查";
        }
      }));
      row.append(code, inspect, insert, openShellSlot);
      return row;
    }));
  };
  card.addEventListener("toggle", () => { if (card.open) void run(refresh); });
  refreshButton.addEventListener("click", () => void run(refresh));
  upload.addEventListener("change", () => {
    const file = upload.files[0];
    upload.value = "";
    if (!file) return;
    void run(async () => {
      if (!file.size || file.size > 32 * 1024 * 1024) throw new Error("请选择不超过 32 MiB 的非空文件");
      const response = await fetch("/api/assets?name=" + encodeURIComponent(file.name), {
        method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: file,
      });
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      await refresh();
      notice.textContent = "上传完成，可插入并预览";
    });
  });
}

async function init() {
  initAssets();
  void refreshAiConfig();

  const setAiMode = (mode) => {
    state.aiMode = mode === "edit" ? "edit" : "generate";
    document.getElementById("ai-mode-generate").classList.toggle("active", state.aiMode === "generate");
    document.getElementById("ai-mode-edit").classList.toggle("active", state.aiMode === "edit");
    const promptEl = document.getElementById("ai-prompt-input");
    const btnEl = document.getElementById("ai-generate-btn");
    if (state.aiMode === "edit") {
      promptEl.placeholder = "描述要对当前 .kcl 代码做的增量修改，例如：把 4 个孔改为 M5 沉头孔，壁厚加到 4mm";
      btnEl.textContent = "AI 增量修改";
    } else {
      promptEl.placeholder = "描述你想新建的机械零件或装配体...";
      btnEl.textContent = "AI 生成";
    }
  };
  document.getElementById("ai-mode-generate").addEventListener("click", () => setAiMode("generate"));
  document.getElementById("ai-mode-edit").addEventListener("click", () => setAiMode("edit"));

  const res = await fetch("/api/examples");
  const data = await res.json();
  state.examples = data.examples || [];
  const sel = document.getElementById("example-select");
  sel.innerHTML = state.examples.map(ex =>
    '<option value="' + escapeHtml(ex.id) + '">[' + escapeHtml(ex.category) + "] " + escapeHtml(ex.name) + "</option>"
  ).join("");

  if (state.examples.length) {
    const initial = state.examples.find((item) => item.id.endsWith("/01_mounting_plate.kcl")) || state.examples[0];
    sel.value = initial.id;
    state.currentFile = initial.id;
    state.source = initial.source;
    await compileCurrent();
  }

  sel.addEventListener("change", async () => {
    const picked = state.examples.find(e => e.id === sel.value);
    if (picked) {
      state.currentFile = picked.id;
      state.source = picked.source;
      state.selectedFaceId = null;
      state.selectedComponentId = null;
      state.activeComponentFilter = null;
      state.highlightedSpan = null;
      state.activeHandleId = null;
      state.explodeFactor = 0;
      document.getElementById("explode-slider").value = "0";
      await compileCurrent("已加载样本 " + picked.name);
    }
  });

  document.querySelectorAll(".ai-preset").forEach(btn => {
    btn.addEventListener("click", () => {
      setAiMode("generate");
      document.getElementById("ai-prompt-input").value = btn.getAttribute("data-prompt") || "";
    });
  });

  document.getElementById("ai-generate-btn").addEventListener("click", async () => {
    if (state.editorMode === "raw") state.source = document.getElementById("raw-editor").value;
    const prompt = document.getElementById("ai-prompt-input").value.trim();
    if (!prompt) return;
    const mat = document.getElementById("ai-material").value;
    const method = document.getElementById("ai-method").value;
    const mode = state.aiMode;
    const provider = document.getElementById("ai-provider").value;
    const model = document.getElementById("ai-model").value.trim();
    const baseUrl = document.getElementById("ai-base-url").value.trim();
    const apiKey = document.getElementById("ai-api-key").value.trim();
    const enableVlm = Boolean(document.getElementById("ai-enable-vlm")?.checked);
    const fewShotRaw = Number.parseInt(document.getElementById("ai-few-shot-k")?.value ?? "2", 10);
    const fewShotK = Number.isInteger(fewShotRaw) && fewShotRaw >= 0 && fewShotRaw <= 5 ? fewShotRaw : 2;
    const llm = {
      provider,
      ...(model ? { model } : {}),
      ...(baseUrl ? { base_url: baseUrl } : {}),
      ...(apiKey ? { api_key: apiKey } : {}),
      enable_vlm: enableVlm,
      few_shot_k: fewShotK,
    };

    const request = ++state.modelRequest;
    const { source, currentFile } = state;
    const badge = document.getElementById("ai-backend-badge");
    const button = document.getElementById("ai-generate-btn");
    const summaryEl = document.getElementById("ai-run-summary");
    const progressEl = document.getElementById("ai-progress-log");
    const progressSteps = [];
    const renderProgress = () => {
      if (!progressEl) return;
      if (!progressSteps.length) {
        progressEl.style.display = "none";
        progressEl.innerHTML = "";
        return;
      }
      progressEl.style.display = "flex";
      progressEl.innerHTML = progressSteps.map((step) => {
        const dotCls = step.passed === false ? "err" : (step.passed === true || step.stage === "completed") ? "ok" : "";
        const iterBadge = typeof step.iteration === "number"
          ? '<span class="ai-progress-iter">R' + step.iteration + (typeof step.max_iters === "number" ? "/" + step.max_iters : "") + "</span>"
          : "";
        return '<div class="ai-progress-step"><span class="ai-progress-dot ' + dotCls + '"></span>' + iterBadge + "<span>" + escapeHtml(step.message || step.stage || "") + "</span></div>";
      }).join("");
      progressEl.scrollTop = progressEl.scrollHeight;
    };
    badge.textContent = mode === "edit" ? "增量修改中..." : "生成中...";
    badge.className = "badge info";
    button.disabled = true;
    summaryEl.style.display = "none";
    renderProgress();
    try {
      const payload = {
        prompt,
        material: mat,
        manufacturing_method: method,
        mode,
        ...(mode === "edit" ? { existing_source: source } : {}),
        llm,
      };
      const out = await postAgentStream(payload, (step) => {
        if (request !== state.modelRequest) return;
        progressSteps.push(step);
        renderProgress();
        if (step.message) setSyncStatus(step.message, "info");
      });
      if (request !== state.modelRequest || source !== state.source || currentFile !== state.currentFile) return;
      if (
        mode === "edit" &&
        (out.ok === false || out.agentSummary?.passed === false || typeof out.source !== "string" || !out.source.trim())
      ) {
        throw new Error(out.error || "增量修改未通过校验，已保留当前源码");
      }
      state.currentFile = out.file;
      state.source = out.source;
      state.activeHandleId = null;
      state.selectedComponentId = null;
      state.highlightedSpan = null;
      state.explodeFactor = 0;
      document.getElementById("explode-slider").value = "0";
      sel.selectedIndex = -1;

      const usedLlm = Boolean(out.agentSummary?.used_llm);
      const llmProv = out.agentSummary?.llm_provider || (usedLlm ? "llm" : "template");
      const llmModel = out.agentSummary?.llm_model ? " (" + out.agentSummary.llm_model + ")" : "";
      const iters = typeof out.agentSummary?.iterations_run === "number" ? out.agentSummary.iterations_run : 0;
      const vlmEnabled = Boolean(out.agentSummary?.vlm_enabled);
      const repairAttempts = typeof out.agentSummary?.contract_repair_attempts === "number" ? out.agentSummary.contract_repair_attempts : 0;
      const retrievedExamples = Array.isArray(out.agentSummary?.retrieved_examples)
        ? out.agentSummary.retrieved_examples.filter((item) => typeof item === "string" && item.trim())
        : [];
      const examplesLabel = retrievedExamples.length ? retrievedExamples.join(", ") : "无";
      const engineLabel = usedLlm ? "真实 LLM · " + llmProv + llmModel : "离线模板";
      badge.textContent = usedLlm ? "LLM: " + llmProv : "模板引擎";
      badge.className = "badge " + (usedLlm ? "ok" : "info");
      summaryEl.textContent =
        "模式: " + (mode === "edit" ? "增量修改" : "新建生成") +
        " · 引擎: " + engineLabel +
        " · 迭代轮数: " + iters +
        " · 契约修复: " + repairAttempts + " 次" +
        " · VLM: " + (vlmEnabled ? "已启用" : "未启用") +
        " · 参考样本: " + examplesLabel;
      summaryEl.className = "ai-run-summary " + (out.ok && out.agentSummary?.passed !== false ? "ok" : "err");
      summaryEl.style.display = "block";

      const methodLabel = usedLlm ? "LLM" : out.generatorBackend === "cad-agent-cli" ? "Agent" : "模板";
      const status = out.agentSummary?.passed === false
        ? "完成，Agent 验证未通过"
        : out.ok ? "模型编译通过" : "完成（含编译诊断）";
      showCompileData(out, methodLabel + " " + status);
    } catch (err) {
      badge.textContent = mode === "edit" ? "修改失败" : "生成失败";
      badge.className = "badge err";
      summaryEl.textContent = err.message;
      summaryEl.className = "ai-run-summary err";
      summaryEl.style.display = "block";
      if (request === state.modelRequest) setSyncStatus(err.message, "err");
    } finally {
      button.disabled = false;
    }
  });

  document.getElementById("bom-reset-btn").addEventListener("click", () => {
    void compileCurrent("已恢复完整装配体", null);
  });

  document.getElementById("recompile-btn").addEventListener("click", () => {
    if (state.editorMode === "raw") state.source = document.getElementById("raw-editor").value;
    void compileCurrent();
  });
  document.getElementById("lsp-toggle-btn").addEventListener("click", () => {
    state.lspVisible = !state.lspVisible;
    if (state.lspVisible && state.editorMode === "raw") refreshEditorLsp();
    else void queryLspAt(state.highlightedSpan?.start_line ?? 1, state.highlightedSpan?.start_col ?? 1);
  });
  document.getElementById("mode-highlight-btn").addEventListener("click", () => {
    if (state.editorMode === "raw") state.source = document.getElementById("raw-editor").value;
    state.editorMode = "highlight";
    renderCodeEditor();
  });
  document.getElementById("mode-raw-btn").addEventListener("click", () => {
    state.editorMode = "raw";
    renderCodeEditor();
    refreshEditorLsp();
  });
  const rawEditor = document.getElementById("raw-editor");
  rawEditor.addEventListener("input", (e) => {
    state.source = e.target.value;
    refreshEditorLsp();
  });
  rawEditor.addEventListener("click", refreshEditorLsp);
  rawEditor.addEventListener("keyup", refreshEditorLsp);

  const switchTab = (tab) => {
    state.viewportTab = tab;
    document.getElementById("viewport-canvas").style.display = tab === "3d" ? "block" : "none";
    document.getElementById("svg-views-panel").style.display = tab === "svg" ? "grid" : "none";
    document.getElementById("drawing-panel").style.display = tab === "drawing" ? "flex" : "none";
    document.getElementById("handle-overlay").style.display = tab === "3d" && (state.compileData?.sceneMesh?.edit_handles?.length) ? "block" : "none";
    document.getElementById("tab-3d-btn").className = tab === "3d" ? "btn btn-primary" : "btn";
    document.getElementById("tab-svg-btn").className = tab === "svg" ? "btn btn-primary" : "btn";
    document.getElementById("tab-drawing-btn").className = tab === "drawing" ? "btn btn-primary" : "btn";
  };
  document.getElementById("tab-3d-btn").addEventListener("click", () => switchTab("3d"));
  document.getElementById("tab-svg-btn").addEventListener("click", () => switchTab("svg"));
  document.getElementById("tab-drawing-btn").addEventListener("click", () => switchTab("drawing"));

  const presets = { iso: {yaw:0.65,pitch:0.58,zoom:1}, top:{yaw:0,pitch:1.56,zoom:1}, front:{yaw:0,pitch:0.02,zoom:1}, right:{yaw:1.57,pitch:0.02,zoom:1} };
  document.querySelectorAll(".cam-btn").forEach(btn => {
    btn.addEventListener("click", () => {
      switchTab("3d");
      state.camera = { ...presets[btn.getAttribute("data-cam")] };
      renderCanvas3D();
    });
  });

  document.getElementById("section-toggle").addEventListener("change", (e) => {
    state.sectionCut.enabled = e.target.checked;
    document.getElementById("section-axis").style.display = state.sectionCut.enabled ? "inline-block" : "none";
    document.getElementById("section-slider").style.display = state.sectionCut.enabled ? "inline-block" : "none";
    renderCanvas3D();
  });
  document.getElementById("section-axis").addEventListener("change", (e) => {
    state.sectionCut.axis = e.target.value;
    renderCanvas3D();
  });
  document.getElementById("section-slider").addEventListener("input", (e) => {
    state.sectionCut.ratio = Number.parseFloat(e.target.value);
    renderCanvas3D();
  });
  document.getElementById("explode-slider").addEventListener("input", (e) => {
    state.explodeFactor = Number.parseFloat(e.target.value);
    renderCanvas3D();
  });

  const canvas = document.getElementById("viewport-canvas");
  let orbit = { active: false, moved: false, x: 0, y: 0 };
  let hDrag = { handle: null, startX: 0, startVal: 0, latestVal: 0, moved: false };
  canvas.addEventListener("pointerdown", (e) => {
    if (!e.isPrimary || e.button !== 0) return;
    canvas.setPointerCapture(e.pointerId);
    const r = canvas.getBoundingClientRect();
    const px = (e.clientX - r.left) * (canvas.width / r.width);
    const py = (e.clientY - r.top) * (canvas.height / r.height);
    for (const a of state.projectedHandles) {
      if (Math.hypot(px - a.x, py - a.y) <= a.radius + 6) {
        state.activeHandleId = a.handle.handle_id;
        state.highlightedSpan = a.handle.ast_span;
        renderCodeEditor(); renderHandles();
        const cur = state.draftValues[a.handle.handle_id] ?? a.handle.current_value;
        hDrag = { handle: a.handle, compileData: state.compileData, startX: e.clientX, startVal: cur, latestVal: cur, moved: false };
        orbit.moved = true;
        renderCanvas3D();
        return;
      }
    }
    orbit = { active: true, moved: false, x: e.clientX, y: e.clientY };
  });
  function finishDrag(e) {
    if (!e.isPrimary) return;
    if (hDrag.handle) {
      const done = hDrag;
      hDrag = { handle: null };
      document.getElementById("drag-hud").style.display = "none";
      renderHandles();
      if (e.type === "pointerup" && done.moved && done.compileData === state.compileData && done.latestVal !== done.startVal) {
        state.activeHandleId = done.handle.handle_id;
        state.draftValues[done.handle.handle_id] = done.latestVal;
        renderHandles();
        void applyHandleWriteback(null, done.latestVal);
      }
    }
    orbit.active = false;
  }
  canvas.addEventListener("pointerup", finishDrag);
  canvas.addEventListener("pointercancel", finishDrag);
  canvas.addEventListener("lostpointercapture", finishDrag);
  canvas.addEventListener("pointermove", (e) => {
    if (!e.isPrimary) return;
    if (hDrag.handle) {
      const dx = e.clientX - hDrag.startX;
      if (Math.abs(dx) > 2) hDrag.moved = true;
      if (!hDrag.moved) return;
      const [minV, maxV] = handleRange(hDrag.handle);
      const next = Number((hDrag.startVal + dx * 0.25).toFixed(2));
      const nv = Math.max(minV, Math.min(maxV, next));
      hDrag.latestVal = nv;
      document.getElementById("handle-num-input").value = String(nv);
      document.getElementById("handle-slider").value = String(nv);
      document.getElementById("handle-val-label").textContent = String(nv);
      const hud = document.getElementById("drag-hud");
      hud.style.display = "block";
      hud.textContent = "拖拽参数: " + hDrag.handle.param_name + " = " + nv + (hDrag.handle.unit === "unitless" ? "" : hDrag.handle.unit);
      return;
    }
    if (!orbit.active) return;
    const dx = e.clientX - orbit.x, dy = e.clientY - orbit.y;
    if (Math.abs(dx) > 2 || Math.abs(dy) > 2) orbit.moved = true;
    orbit.x = e.clientX; orbit.y = e.clientY;
    state.camera.yaw += dx * 0.01;
    state.camera.pitch = Math.max(-1.45, Math.min(1.45, state.camera.pitch + dy * 0.01));
    renderCanvas3D();
  });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    state.camera.zoom = Math.max(0.4, Math.min(3.0, state.camera.zoom - e.deltaY * 0.001));
    renderCanvas3D();
  });
  canvas.addEventListener("click", (e) => {
    if (orbit.moved) return;
    const r = canvas.getBoundingClientRect();
    const px = (e.clientX - r.left) * (canvas.width / r.width);
    const py = (e.clientY - r.top) * (canvas.height / r.height);
    for (let i = state.projectedTris.length - 1; i >= 0; i--) {
      const t = state.projectedTris[i];
      if (pointInTri(px, py, t.pts[0], t.pts[1], t.pts[2])) { selectFace(t.face); return; }
    }
    selectFace(null);
  });

  const slider = document.getElementById("handle-slider");
  const numInput = document.getElementById("handle-num-input");
  slider.addEventListener("input", () => {
    numInput.value = slider.value;
    document.getElementById("handle-val-label").textContent = slider.value;
  });
  document.getElementById("handle-apply-btn").addEventListener("click", () => void applyHandleWriteback(null));
  document.getElementById("material-select").addEventListener("change", renderRightDashboard);

  const downloadFile = (ext, content, mime) => {
    if (!content || state.source !== state.compiledSource || state.currentFile !== state.compileData?.file) {
      setSyncStatus("请先编译当前源码，确认有可用的导出文件", "err");
      return;
    }
    const blob = new Blob([content], { type: mime });
    const a = document.createElement("a");
    const objectUrl = URL.createObjectURL(blob);
    a.href = objectUrl;
    a.download = (state.compileData.sceneMesh?.part_name || state.compileData.diagnostics?.part_name || "model") + ext;
    a.click();
    setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
  };
  const decodeBase64 = (base64) => {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
    return bytes;
  };
  document.getElementById("download-step-btn").addEventListener("click", () => downloadFile(".step", state.compileData?.stepContent, "application/step"));
  document.getElementById("download-dxf-btn").addEventListener("click", () => downloadFile(".dxf", state.compileData?.dxfContent, "application/dxf"));
  document.getElementById("download-drawing-btn").addEventListener("click", () => downloadFile("_drawing.svg", state.compileData?.drawingSvg, "image/svg+xml"));
  document.getElementById("download-3mf-btn").addEventListener("click", () => {
    const b64 = state.compileData?.threemfBase64;
    downloadFile(".3mf", b64 ? decodeBase64(b64) : null, "model/3mf");
  });
  document.getElementById("download-mesh-btn").addEventListener("click", () => downloadFile(".mesh.json", JSON.stringify(state.compileData?.sceneMesh || {}, null, 2), "application/json"));
}

void init();
</script>
</body>
</html>`;

// Project paths from HTTP clients stay inside workspaceRoot.
// Absolute paths, `..` segments, and null bytes are rejected before cad-cli runs.
function isAbsoluteProjectPath(value) {
  return path.win32.isAbsolute(value)
    || path.posix.isAbsolute(value)
    || /^[A-Za-z]:/.test(value);
}

function isInsideWorkspace(root, candidate) {
  const stripExtended = (input) => path.resolve(input).replace(/^\\\\\?\\/, "");
  const relative = path.relative(stripExtended(root), stripExtended(candidate));
  return relative === ""
    || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

function assertWorkspaceRelative(value, fieldName) {
  if (typeof value !== "string" || value.length === 0) {
    throw httpError(400, `Expected '${fieldName}' to be a workspace-relative path`);
  }
  if (value.includes("\0")) {
    throw httpError(400, `Expected '${fieldName}' to be a workspace-relative path without a null byte`);
  }
  if (isAbsoluteProjectPath(value)) {
    throw httpError(400, `Expected '${fieldName}' to be a workspace-relative path`);
  }
  if (value.split(/[\\/]/).includes("..")) {
    throw httpError(400, `Expected '${fieldName}' to stay inside the workspace`);
  }
}

function lexicalProjectPath(root, relativePath, fieldName) {
  assertWorkspaceRelative(relativePath, fieldName);
  const resolved = path.resolve(root, relativePath);
  if (!isInsideWorkspace(root, resolved)) {
    throw httpError(400, `Expected '${fieldName}' to stay inside the workspace`);
  }
  return resolved;
}

async function canonicalProjectPath(root, relativePath, fieldName) {
  const lexical = lexicalProjectPath(root, relativePath, fieldName);
  const pending = [];
  let cursor = lexical;
  for (;;) {
    try {
      const real = await fs.realpath(cursor);
      if (!isInsideWorkspace(root, real)) {
        throw httpError(400, `Expected '${fieldName}' to stay inside the workspace`);
      }
      let resolved = real;
      for (let index = pending.length - 1; index >= 0; index -= 1) {
        resolved = path.join(resolved, pending[index]);
      }
      if (!isInsideWorkspace(root, resolved)) {
        throw httpError(400, `Expected '${fieldName}' to stay inside the workspace`);
      }
      return resolved;
    } catch (err) {
      if (err?.statusCode) throw err;
      if (err?.code !== "ENOENT") throw err;
      if (path.resolve(cursor) === path.resolve(root)) return lexical;
      pending.push(path.basename(cursor));
      const parent = path.dirname(cursor);
      if (parent === cursor) return lexical;
      cursor = parent;
    }
  }
}

function requireProjectName(name) {
  if (typeof name !== "string" || name.trim() === "" || name.includes("\0")) {
    throw httpError(400, "Expected a non-empty project name");
  }
  return name;
}

function textFromStdio(value) {
  if (typeof value === "string") return value;
  if (Buffer.isBuffer(value)) return value.toString("utf8");
  return "";
}

function tryParseProjectJson(stdout) {
  try {
    const value = JSON.parse(textFromStdio(stdout));
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    return value;
  } catch {
    return null;
  }
}

function parseProjectCliJson(stdout) {
  const parsed = tryParseProjectJson(stdout);
  if (!parsed) throw httpError(422, "cad-cli returned invalid JSON");
  return parsed;
}

async function runProjectCli(args, json) {
  const cliBin = resolveCadCliBinary();
  if (!cliBin) {
    throw httpError(503, "Compiled cad-cli binary not found in target/debug or target/release.");
  }
  try {
    const result = await execFileAsync(cliBin, args, {
      timeout: 30_000,
      maxBuffer: 8 * 1024 * 1024,
    });
    return json ? parseProjectCliJson(result.stdout) : textFromStdio(result.stdout);
  } catch (err) {
    if (err?.statusCode) throw err;
    if (err?.code === "ENOENT") {
      throw httpError(503, "Compiled cad-cli binary not found in target/debug or target/release.");
    }
    if (err?.killed || err?.signal || err?.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
      throw httpError(422, "cad-cli project command exceeded time or output limits");
    }
    const stdout = textFromStdio(err?.stdout);
    const stderr = textFromStdio(err?.stderr);
    if (json) {
      const parsed = tryParseProjectJson(stdout);
      if (parsed) {
        throw Object.assign(httpError(422, "cad-cli project command failed"), { payload: parsed });
      }
    }
    const message = stderr.trim() || stdout.trim() || err?.message || "cad-cli project command failed";
    throw httpError(422, String(message).slice(0, 2000));
  }
}

const PROJECT_INIT_FORMAT_TO_CLI = new Map([
  ["directory", "directory"],
  ["sharded_directory", "sharded-dir"],
  ["sharded-dir", "sharded-dir"],
  ["sqlite", "sqlite"],
  ["archive", "archive"],
]);
const VALID_BRANCH_ACTIONS = new Set([
  "list",
  "create",
  "checkout",
  "rollback",
  "diff",
  "commit_preview",
  "discard_preview",
]);
const VALID_PREVIEW_ACTIONS = new Set(["diff", "commit_preview", "discard_preview"]);
const VALID_BRANCH_PURPOSES = new Set(["user", "ai_candidate"]);
const SAFE_BRANCH_NAME = /^[A-Za-z0-9_][A-Za-z0-9._/-]{0,127}$/;
const SAFE_NODE_ID = /^[A-Za-z0-9_][A-Za-z0-9._:/-]{0,127}$/;

function requireSafeBranchName(value, fieldName) {
  if (
    typeof value !== "string"
    || !SAFE_BRANCH_NAME.test(value)
    || value.includes("..")
    || value.includes("//")
    || value.endsWith("/")
    || value.endsWith(".")
  ) {
    throw httpError(400, `Expected '${fieldName}' to be a valid branch name`);
  }
  return value;
}

function requireSafeNodeIdList(value, fieldName) {
  if (!Array.isArray(value) || value.length === 0 || value.length > 256) {
    throw httpError(400, `Expected '${fieldName}' to contain between 1 and 256 node IDs`);
  }
  const out = [];
  for (const item of value) {
    if (typeof item !== "string" || !SAFE_NODE_ID.test(item) || item.includes("..")) {
      throw httpError(400, `Expected '${fieldName}' items to be valid node IDs`);
    }
    out.push(item);
  }
  return out;
}

function optionalNonNegativeInt(value, fieldName, fallback, max = 1_000_000) {
  if (value === undefined) return fallback;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > max) {
    throw httpError(400, `Expected '${fieldName}' to be an integer between 0 and ${max}`);
  }
  return value;
}

async function runProjectInit(root, body, cliRunner = runProjectCli) {
  const absDir = await canonicalProjectPath(root, body?.relativeDir, "relativeDir");
  const name = requireProjectName(body?.name);
  const args = ["project", "init", absDir, "--name", name];
  if (body?.format !== undefined) {
    if (typeof body.format !== "string" || !PROJECT_INIT_FORMAT_TO_CLI.has(body.format)) {
      throw httpError(
        400,
        "Expected 'format' to be one of directory, sharded_directory, sqlite, or archive",
      );
    }
    args.push("--format", PROJECT_INIT_FORMAT_TO_CLI.get(body.format));
  }
  args.push("--json");
  return cliRunner(args, true);
}

async function runProjectInspect(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  return cliRunner(["project", "inspect", absPath, "--json"], true);
}

async function runProjectApplyPatch(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const patch = body?.patch;
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) {
    throw httpError(400, "Expected a GraphPatch object in 'patch'");
  }
  const branch = body?.branch !== undefined
    ? requireSafeBranchName(body.branch, "branch")
    : undefined;
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-studio-project-patch-"));
  try {
    const patchFile = path.join(tmpDir, "patch.json");
    await fs.writeFile(patchFile, JSON.stringify(patch), "utf8");
    const args = ["project", "apply-patch", absPath, "--patch", patchFile];
    if (branch !== undefined) {
      args.push("--branch", branch);
    }
    args.push("--json");
    return await cliRunner(args, true);
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
  }
}

async function runProjectEmitKcl(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const kcl = await cliRunner(["project", "emit-kcl", absPath], false);
  return { ok: true, kcl };
}

async function runProjectPreviewBranchOp(root, body, action, mcpToolRunner = null) {
  if (!VALID_PREVIEW_ACTIONS.has(action)) {
    throw httpError(400, "Expected preview action to be one of diff, commit_preview, or discard_preview");
  }
  const relPath = body?.relativePath ?? body?.projectPath ?? body?.project_path;
  const absPath = await canonicalProjectPath(root, relPath, "relativePath");
  const rawBranch = body?.branchName ?? body?.branch;
  const branchName = requireSafeBranchName(rawBranch, "branchName");
  if ((action === "commit_preview" || action === "discard_preview") && branchName === "main") {
    throw httpError(400, `Cannot run '${action}' on protected branch 'main'`);
  }
  const [result] = await runMcpToolCalls({
    workspaceRoot: root,
    projectPath: absPath,
    runner: mcpToolRunner,
    calls: [
      {
        name: "cad_branch_control",
        arguments: {
          action,
          branch: branchName,
        },
      },
    ],
  });
  return {
    ok: true,
    action,
    branch: branchName,
    ...(result && typeof result === "object" ? result : { result }),
  };
}

async function runProjectBranch(root, body, cliRunner = runProjectCli, mcpToolRunner = null) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const action = body?.action;
  if (typeof action !== "string" || !VALID_BRANCH_ACTIONS.has(action)) {
    throw httpError(
      400,
      "Expected 'action' to be one of list, create, checkout, rollback, diff, commit_preview, or discard_preview",
    );
  }
  if (VALID_PREVIEW_ACTIONS.has(action)) {
    return runProjectPreviewBranchOp(root, body, action, mcpToolRunner);
  }
  if (action === "list") {
    return cliRunner(["project", "branch", absPath, "list", "--json"], true);
  }
  if (action === "create") {
    const branchName = requireSafeBranchName(body?.branchName, "branchName");
    const args = ["project", "branch", absPath, "create", branchName];
    if (body?.fromBranch !== undefined) {
      args.push("--from", requireSafeBranchName(body.fromBranch, "fromBranch"));
    }
    if (body?.purpose !== undefined) {
      if (typeof body.purpose !== "string" || !VALID_BRANCH_PURPOSES.has(body.purpose)) {
        throw httpError(400, "Expected 'purpose' to be one of user or ai_candidate");
      }
      args.push("--purpose", body.purpose);
    }
    args.push("--json");
    return cliRunner(args, true);
  }
  if (action === "checkout") {
    const branchName = requireSafeBranchName(body?.branchName, "branchName");
    return cliRunner(["project", "branch", absPath, "checkout", branchName, "--json"], true);
  }
  const toSeq = optionalNonNegativeInt(body?.toSeq, "toSeq", undefined, Number.MAX_SAFE_INTEGER);
  if (toSeq === undefined) {
    throw httpError(400, "Expected 'toSeq' to be a non-negative integer for rollback");
  }
  return cliRunner(
    ["project", "branch", absPath, "rollback", "--to-seq", String(toSeq), "--json"],
    true,
  );
}

async function runProjectSliceContext(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const focusNodes = requireSafeNodeIdList(body?.focusNodes, "focusNodes");
  const up = optionalNonNegativeInt(body?.upstreamDepth, "upstreamDepth", 1, 1000);
  const down = optionalNonNegativeInt(body?.downstreamDepth, "downstreamDepth", 1, 1000);
  if (body?.ensureValidClosure !== undefined && typeof body.ensureValidClosure !== "boolean") {
    throw httpError(400, "Expected 'ensureValidClosure' to be a boolean");
  }
  const args = [
    "project",
    "slice-context",
    absPath,
    "--focus",
    focusNodes.join(","),
    "--up",
    String(up),
    "--down",
    String(down),
  ];
  if (body?.ensureValidClosure === false) {
    args.push("--no-closure");
  }
  args.push("--json");
  return cliRunner(args, true);
}

async function runProjectVacuum(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const keepRecent = optionalNonNegativeInt(
    body?.keepRecentOplog,
    "keepRecentOplog",
    undefined,
    1_000_000,
  );
  const args = ["project", "vacuum", absPath];
  if (keepRecent !== undefined) {
    args.push("--keep-recent-oplog", String(keepRecent));
  }
  args.push("--json");
  return cliRunner(args, true);
}

async function runProjectPackAndGo(root, body, cliRunner = runProjectCli) {
  const absPath = await canonicalProjectPath(root, body?.relativePath, "relativePath");
  const absOut = await canonicalProjectPath(root, body?.outputRelativePath, "outputRelativePath");
  if (path.resolve(absPath) === path.resolve(absOut)) {
    throw httpError(400, "Expected 'outputRelativePath' to differ from 'relativePath'");
  }
  return cliRunner(
    ["project", "pack-and-go", absPath, "--workspace-root", root, "--out", absOut, "--json"],
    true,
  );
}

// Session ids become file names under `<workspace>/sessions`. Reject the same
// escapes the project bridge rejects before any path is joined.
function assertSessionId(id) {
  if (typeof id !== "string" || id.length === 0 || id.length > 256) {
    throw httpError(400, "Invalid session id");
  }
  if (id.includes("\0") || id.includes("/") || id.includes("\\") || id.includes("..")) {
    throw httpError(400, "Invalid session id");
  }
  if (id === "." || /^[A-Za-z]:/.test(id) || /[<>:"|?*\u0000-\u001f\u007f]/.test(id)) {
    throw httpError(400, "Invalid session id");
  }
  if (id.endsWith(".") || id.endsWith(" ")) {
    throw httpError(400, "Invalid session id");
  }
}

function decodePathToken(token, label) {
  if (typeof token !== "string" || token.length === 0 || token.includes("%00") || token.includes("\0")) {
    throw httpError(400, `Invalid ${label}`);
  }
  let decoded;
  try {
    decoded = decodeURIComponent(token);
  } catch {
    throw httpError(400, `Invalid ${label}`);
  }
  if (decoded.includes("\0")) throw httpError(400, `Invalid ${label}`);
  return decoded;
}

function matchSessionRoute(reqUrl) {
  const raw = typeof reqUrl === "string" ? reqUrl : "/";
  const queryAt = raw.indexOf("?");
  const pathname = queryAt === -1 ? raw : raw.slice(0, queryAt);
  const match = SESSION_ROUTE.exec(pathname);
  if (!match) return null;
  const resource = match[2];
  if (resource === "tree") return { kind: "tree", sessionToken: match[1] };
  if (resource === "tree/search") return { kind: "search", sessionToken: match[1] };
  if (resource === "interface-digest") return { kind: "digest", sessionToken: match[1] };
  return { kind: "hlod", sessionToken: match[1], chunkToken: match[3] };
}

function readQueryInt(params, name, fallback, { required = false, min = 0, max } = {}) {
  const raw = params.get(name);
  if (raw === null || raw === "") {
    if (required) throw httpError(400, `Expected '${name}' to be an integer`);
    return fallback;
  }
  if (!/^(?:0|[1-9]\d{0,15})$/.test(raw)) {
    throw httpError(400, `Expected '${name}' to be an integer`);
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || (max !== undefined && value > max)) {
    throw httpError(400, `Expected '${name}' to be an integer`);
  }
  return value;
}

function requireUnit(value, fieldName) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw httpError(422, `Expected '${fieldName}' to be a non-negative integer`);
  }
  return value;
}

function requireText(value, fieldName) {
  if (typeof value !== "string") throw httpError(422, `Expected '${fieldName}' to be a string`);
  return value;
}

function requireBool(value, fieldName) {
  if (typeof value !== "boolean") throw httpError(422, `Expected '${fieldName}' to be a boolean`);
  return value;
}

function requireVec3Field(value, fieldName) {
  if (!Array.isArray(value) || value.length !== 3 || !value.every((item) => typeof item === "number" && Number.isFinite(item))) {
    throw httpError(400, `Expected '${fieldName}' to be three finite numbers`);
  }
  return [value[0], value[1], value[2]];
}

function normalizeTreeNode(node, index) {
  if (!node || typeof node !== "object" || Array.isArray(node)) {
    throw httpError(422, `Virtual tree node ${index} is invalid`);
  }
  return {
    instance_id: requireUnit(node.instance_id, `nodes[${index}].instance_id`),
    parent_id: node.parent_id === null ? null : requireUnit(node.parent_id, `nodes[${index}].parent_id`),
    prototype_id: requireUnit(node.prototype_id, `nodes[${index}].prototype_id`),
    name: requireText(node.name, `nodes[${index}].name`),
    part_number: requireText(node.part_number, `nodes[${index}].part_number`),
    bbox_min_mm: requireVec3Field(node.bbox_min_mm, `nodes[${index}].bbox_min_mm`),
    bbox_max_mm: requireVec3Field(node.bbox_max_mm, `nodes[${index}].bbox_max_mm`),
    frozen_lightweight: requireBool(node.frozen_lightweight, `nodes[${index}].frozen_lightweight`),
  };
}

function toByteBuffer(value, fieldName) {
  if (Buffer.isBuffer(value)) return Buffer.from(value);
  if (value instanceof Uint8Array) return Buffer.from(value);
  if (Array.isArray(value) && value.every((item) => Number.isInteger(item) && item >= 0 && item <= 255)) {
    return Buffer.from(value);
  }
  throw httpError(422, `Expected '${fieldName}' to be bytes`);
}

function normalizeChunks(chunks) {
  const map = new Map();
  if (chunks == null) return map;
  if (typeof chunks !== "object" || Array.isArray(chunks)) {
    throw httpError(422, "Session chunks must be an object keyed by chunk id");
  }
  for (const [key, value] of Object.entries(chunks)) {
    if (!/^(?:0|[1-9]\d{0,15})$/.test(key)) {
      throw httpError(422, "Chunk id must be a non-negative integer");
    }
    const bytes = toByteBuffer(value, `chunks.${key}`);
    if (bytes.length > MAX_HLOD_CHUNK_BYTES) {
      throw httpError(413, "HLOD chunk is larger than 16 MiB");
    }
    map.set(Number(key), bytes);
  }
  return map;
}

function isValidBox(min, max) {
  for (let axis = 0; axis < 3; axis += 1) {
    if (!Number.isFinite(min[axis]) || !Number.isFinite(max[axis]) || min[axis] > max[axis]) return false;
  }
  return true;
}

function boxesIntersect(aMin, aMax, bMin, bMax) {
  if (!isValidBox(aMin, aMax) || !isValidBox(bMin, bMax)) return false;
  for (let axis = 0; axis < 3; axis += 1) {
    if (aMax[axis] < bMin[axis] || aMin[axis] > bMax[axis]) return false;
  }
  return true;
}

function pageTree(nodes, offset, limit) {
  const start = offset >= nodes.length ? nodes.length : offset;
  const available = nodes.length - start;
  const count = limit > available ? available : limit;
  return {
    offset,
    limit,
    total_count: nodes.length,
    nodes: nodes.slice(start, start + count),
  };
}

function searchTree(nodes, query, limit) {
  const needle = query.toLowerCase();
  const hits = [];
  for (const node of nodes) {
    if (node.name.toLowerCase().includes(needle) || node.part_number.toLowerCase().includes(needle)) {
      hits.push(node.instance_id);
      if (hits.length === limit) break;
    }
  }
  return hits;
}

function boundLabel(text) {
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length <= MAX_DIGEST_LABEL_BYTES) return { text, truncated: false };
  let end = MAX_DIGEST_LABEL_BYTES;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end -= 1;
  return { text: bytes.subarray(0, end).toString("utf8"), truncated: true };
}

function buildInterfaceSummary(roiMin, roiMax, matches) {
  let labelsTruncated = false;
  const components = [];
  for (const node of matches) {
    if (components.length >= MAX_DIGEST_COMPONENTS) break;
    const name = boundLabel(node.name);
    const partNumber = boundLabel(node.part_number);
    labelsTruncated ||= name.truncated || partNumber.truncated;
    components.push({
      instance_id: node.instance_id,
      prototype_id: node.prototype_id,
      name: name.text,
      part_number: partNumber.text,
      bbox_min_mm: node.bbox_min_mm,
      bbox_max_mm: node.bbox_max_mm,
      frozen_lightweight: node.frozen_lightweight,
    });
  }
  const payload = {
    schema_version: "1.0",
    roi_min_mm: roiMin,
    roi_max_mm: roiMax,
    components,
    total_matches: matches.length,
    labels_truncated: labelsTruncated,
    truncated: labelsTruncated || matches.length > components.length,
  };
  let json = JSON.stringify(payload);
  while (Buffer.byteLength(json, "utf8") >= MAX_DIGEST_JSON_BYTES && payload.components.length > 0) {
    payload.components.pop();
    payload.truncated = true;
    json = JSON.stringify(payload);
  }
  if (Buffer.byteLength(json, "utf8") >= MAX_DIGEST_JSON_BYTES) {
    payload.components = [];
    payload.labels_truncated = false;
    payload.truncated = matches.length > 0;
    json = JSON.stringify(payload);
  }
  return json;
}

function memorySession(id, nodes, chunks) {
  return {
    id,
    nodes,
    async readChunk(chunkId) {
      const bytes = chunks.get(chunkId);
      if (!bytes) throw httpError(404, "HLOD chunk not found");
      return bytes;
    },
  };
}

function readU64LE(buffer, offset) {
  const low = buffer.readUInt32LE(offset);
  const high = buffer.readUInt32LE(offset + 4);
  if (high !== 0) throw httpError(422, "Cadpack field is too large");
  return low;
}

function parseCadpackHeader(header, fileSize) {
  if (header.length < CADPACK_HEADER_SIZE || !header.subarray(0, 4).equals(CADPACK_MAGIC)) {
    throw httpError(422, "Invalid cadpack magic");
  }
  const version = header.readUInt32LE(4);
  if (version !== 1) throw httpError(422, `Unsupported cadpack format version ${version}`);
  const directoryOffset = readU64LE(header, 8);
  const count = header.readUInt32LE(16);
  const entrySize = header.readUInt32LE(20);
  const totalSize = readU64LE(header, 24);
  if (directoryOffset < CADPACK_HEADER_SIZE || directoryOffset % CADPACK_ALIGNMENT !== 0) {
    throw httpError(422, "Cadpack directory is not 64-byte aligned");
  }
  if (entrySize !== CADPACK_ENTRY_SIZE) throw httpError(422, "Unsupported cadpack directory entry size");
  if (totalSize !== fileSize) throw httpError(422, "Cadpack total size does not match the file");
  if (count > MAX_CADPACK_CHUNKS) throw httpError(422, "Cadpack directory is too large");
  const directoryBytes = count * CADPACK_ENTRY_SIZE;
  const directoryEnd = directoryOffset + directoryBytes;
  if (directoryEnd > fileSize) throw httpError(422, "Cadpack directory is outside the file");
  return { directoryOffset, count, directoryEnd };
}

function parseCadpackDirectory(directory, directoryEnd, fileSize) {
  if (directory.length % CADPACK_ENTRY_SIZE !== 0) {
    throw httpError(422, "Cadpack directory is truncated");
  }
  const entries = [];
  const seen = new Set();
  for (let index = 0; index < directory.length; index += CADPACK_ENTRY_SIZE) {
    const chunkId = directory.readUInt32LE(index);
    const kind = directory.readUInt32LE(index + 4);
    const offset = readU64LE(directory, index + 8);
    const length = readU64LE(directory, index + 16);
    if (kind > 5) throw httpError(422, "Cadpack directory has an unknown chunk kind");
    if (seen.has(chunkId)) throw httpError(422, `Cadpack chunk id ${chunkId} is duplicated`);
    seen.add(chunkId);
    if (offset % CADPACK_ALIGNMENT !== 0) throw httpError(422, "Cadpack chunk is not 64-byte aligned");
    const end = offset + length;
    if (offset < directoryEnd || end > fileSize) {
      throw httpError(422, "Cadpack chunk range is outside the file");
    }
    entries.push({
      chunkId,
      kind,
      offset,
      length,
      sha256: Buffer.from(directory.subarray(index + 24, index + 56)),
    });
  }
  const ranges = entries
    .filter((entry) => entry.length > 0)
    .map((entry) => ({ start: entry.offset, end: entry.offset + entry.length }))
    .sort((left, right) => left.start - right.start);
  for (let index = 1; index < ranges.length; index += 1) {
    if (ranges[index].start < ranges[index - 1].end) {
      throw httpError(422, "Cadpack chunk ranges overlap");
    }
  }
  return entries;
}

function verifyChunkHash(entry, bytes) {
  if (bytes.length !== entry.length) throw httpError(422, "Cadpack chunk length does not match the directory");
  const actual = createHash("sha256").update(bytes).digest();
  if (!timingSafeEqual(actual, entry.sha256)) {
    throw httpError(422, `SHA-256 verification failed for chunk ${entry.chunkId}`);
  }
}

function parseTreePayload(bytes) {
  let value;
  try {
    value = JSON.parse(bytes.toString("utf8"));
  } catch {
    throw httpError(422, "Cadpack virtual tree is not JSON");
  }
  const list = Array.isArray(value) ? value : value?.nodes;
  if (!Array.isArray(list)) throw httpError(422, "Cadpack virtual tree is missing nodes");
  return list.map((node, index) => normalizeTreeNode(node, index));
}

function treeNodesFromEntries(entries, readPayload) {
  const nodes = [];
  for (const entry of entries) {
    if (entry.kind !== CHUNK_KIND_VIRTUAL_TREE || entry.length === 0) continue;
    if (entry.length > MAX_TREE_CHUNK_BYTES) throw httpError(422, "Cadpack virtual tree is too large");
    const payload = readPayload(entry);
    verifyChunkHash(entry, payload);
    nodes.push(...parseTreePayload(payload));
  }
  return nodes;
}

function sessionFromCadpackBuffer(id, buffer) {
  const header = parseCadpackHeader(buffer, buffer.length);
  const directory = buffer.subarray(header.directoryOffset, header.directoryEnd);
  const entries = parseCadpackDirectory(directory, header.directoryEnd, buffer.length);
  const nodes = treeNodesFromEntries(entries, (entry) => buffer.subarray(entry.offset, entry.offset + entry.length));
  const chunks = new Map();
  for (const entry of entries) {
    if (entry.kind > 2) continue;
    if (entry.length > MAX_HLOD_CHUNK_BYTES) throw httpError(413, "HLOD chunk is larger than 16 MiB");
    const payload = Buffer.from(buffer.subarray(entry.offset, entry.offset + entry.length));
    verifyChunkHash(entry, payload);
    chunks.set(entry.chunkId, payload);
  }
  return memorySession(id, nodes, chunks);
}

async function readExact(filePath, offset, length) {
  if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || offset < 0 || length < 0) {
    throw httpError(422, "Invalid cadpack range");
  }
  const handle = await fs.open(filePath, "r");
  try {
    const buffer = Buffer.alloc(length);
    let filled = 0;
    while (filled < length) {
      const { bytesRead } = await handle.read(buffer, filled, length - filled, offset + filled);
      if (bytesRead === 0) throw httpError(422, "Cadpack file is truncated");
      filled += bytesRead;
    }
    return buffer;
  } finally {
    await handle.close();
  }
}

function containedJoin(root, relative) {
  if (typeof relative !== "string" || relative.includes("\0") || relative.split(/[\\/]/).includes("..")) {
    return null;
  }
  const resolved = path.resolve(root, relative);
  if (!isInsideWorkspace(root, resolved)) return null;
  return resolved;
}

async function openSessionsDirectory(root) {
  const dir = path.join(root, "sessions");
  let info;
  try {
    info = await fs.lstat(dir);
  } catch (err) {
    if (err?.code === "ENOENT") return null;
    throw err;
  }
  if (!info.isDirectory() || info.isSymbolicLink()) {
    throw httpError(403, "The workspace sessions directory must be a real directory inside the workspace");
  }
  const real = await fs.realpath(dir);
  if (!isInsideWorkspace(root, real)) {
    throw httpError(403, "The workspace sessions directory must be a real directory inside the workspace");
  }
  return real;
}

async function locateSessionCadpack(sessionsDir, id) {
  const relatives = id.toLowerCase().endsWith(".cadpack")
    ? [id]
    : [`${id}.cadpack`, path.join(id, "scene.cadpack")];
  for (const relative of relatives) {
    const candidate = containedJoin(sessionsDir, relative);
    if (!candidate) continue;
    let info;
    try {
      info = await fs.lstat(candidate);
    } catch (err) {
      if (err?.code === "ENOENT") continue;
      throw err;
    }
    if (info.isSymbolicLink()) {
      throw httpError(403, "Session cadpack must be a regular file inside the sessions directory");
    }
    if (!info.isFile()) continue;
    if (info.nlink !== 1) {
      throw httpError(403, "Session cadpack must be a regular file inside the sessions directory");
    }
    const real = await fs.realpath(candidate);
    if (!isInsideWorkspace(sessionsDir, real)) {
      throw httpError(403, "Session cadpack must be a regular file inside the sessions directory");
    }
    if (info.size <= 0 || info.size > MAX_CADPACK_BYTES) {
      throw httpError(413, "Session cadpack size is outside the allowed range");
    }
    return { filePath: real, size: info.size, mtimeMs: info.mtimeMs };
  }
  return null;
}

async function loadFileSession(id, located) {
  const headerBytes = await readExact(located.filePath, 0, CADPACK_HEADER_SIZE);
  const header = parseCadpackHeader(headerBytes, located.size);
  const directory = await readExact(located.filePath, header.directoryOffset, header.directoryEnd - header.directoryOffset);
  const entries = parseCadpackDirectory(directory, header.directoryEnd, located.size);
  const payloads = new Map();
  const readPayload = (entry) => {
    const cached = payloads.get(entry.chunkId);
    if (!cached) throw httpError(422, "Cadpack virtual tree was not loaded");
    return cached;
  };
  for (const entry of entries) {
    if (entry.kind !== CHUNK_KIND_VIRTUAL_TREE || entry.length === 0) continue;
    if (entry.length > MAX_TREE_CHUNK_BYTES) throw httpError(422, "Cadpack virtual tree is too large");
    payloads.set(entry.chunkId, await readExact(located.filePath, entry.offset, entry.length));
  }
  const nodes = treeNodesFromEntries(entries, readPayload);
  const byId = new Map(entries.filter((entry) => entry.kind <= 2).map((entry) => [entry.chunkId, entry]));
  return {
    id,
    nodes,
    async readChunk(chunkId) {
      const entry = byId.get(chunkId);
      if (!entry) throw httpError(404, "HLOD chunk not found");
      if (entry.length > MAX_HLOD_CHUNK_BYTES) throw httpError(413, "HLOD chunk is larger than 16 MiB");
      if (entry.length === 0) return Buffer.alloc(0);
      const bytes = await readExact(located.filePath, entry.offset, entry.length);
      verifyChunkHash(entry, bytes);
      return bytes;
    },
  };
}

function createSessionStore() {
  const memory = new Map();
  const fileCache = new Map();
  return {
    add(seed) {
      if (!seed || typeof seed !== "object" || Array.isArray(seed)) {
        throw httpError(422, "Session seed must be an object");
      }
      assertSessionId(seed.id);
      let session;
      if (seed.cadpackBytes != null) {
        session = sessionFromCadpackBuffer(seed.id, toByteBuffer(seed.cadpackBytes, "cadpackBytes"));
      } else {
        if (seed.nodes != null && !Array.isArray(seed.nodes)) {
          throw httpError(422, "Session nodes must be an array");
        }
        const nodes = Array.isArray(seed.nodes) ? seed.nodes.map((node, index) => normalizeTreeNode(node, index)) : [];
        session = memorySession(seed.id, nodes, normalizeChunks(seed.chunks));
      }
      memory.set(seed.id, session);
      return seed.id;
    },
    async open(id, workspaceRoot) {
      assertSessionId(id);
      const seeded = memory.get(id);
      if (seeded) return seeded;
      const root = await workspaceRoot();
      const sessionsDir = await openSessionsDirectory(root);
      if (!sessionsDir) throw httpError(404, "Session not found");
      const located = await locateSessionCadpack(sessionsDir, id);
      if (!located) throw httpError(404, "Session not found");
      const cacheKey = `${located.filePath}\0${located.mtimeMs}\0${located.size}`;
      const cached = fileCache.get(cacheKey);
      if (cached) return cached;
      const session = await loadFileSession(id, located);
      fileCache.set(cacheKey, session);
      return session;
    },
  };
}

function parseInclusiveByteRange(header, total) {
  if (typeof header !== "string") return null;
  const match = /^bytes=(\d+)-(\d+)$/.exec(header.trim());
  if (!match || match[1].length > 15 || match[2].length > 15) return null;
  const start = Number(match[1]);
  const end = Number(match[2]);
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || end < start) return null;
  if (total <= 0 || start >= total || end >= total) return null;
  if (end - start + 1 > MAX_HLOD_CHUNK_BYTES) return null;
  return { start, end };
}

function sendOctet(res, statusCode, bytes, extraHeaders = {}) {
  const body = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  const payload = body.byteOffset === 0 && body.byteLength === body.buffer.byteLength ? body : Buffer.from(body);
  res.writeHead(statusCode, {
    "Content-Type": "application/octet-stream",
    "Accept-Ranges": "bytes",
    "Cache-Control": "no-store",
    "Content-Length": payload.length,
    ...extraHeaders,
  });
  res.end(payload);
}

function sendUnsatisfiableRange(res, total) {
  const body = JSON.stringify({ error: "Range not satisfiable" });
  res.writeHead(416, {
    "Content-Type": "application/json; charset=utf-8",
    "Accept-Ranges": "bytes",
    "Content-Range": `bytes */${total}`,
    "Cache-Control": "no-store",
    "Content-Length": Buffer.byteLength(body),
  });
  res.end(body);
}

function writeChunkResponse(res, bytes, rangeHeader) {
  if (rangeHeader === undefined) {
    sendOctet(res, 200, bytes);
    return;
  }
  const range = parseInclusiveByteRange(rangeHeader, bytes.length);
  if (!range) {
    sendUnsatisfiableRange(res, bytes.length);
    return;
  }
  sendOctet(res, 206, bytes.subarray(range.start, range.end + 1), {
    "Content-Range": `bytes ${range.start}-${range.end}/${bytes.length}`,
  });
}

async function handleSessionRoute(req, res, url, route, sessionStore, workspaceRoot) {
  const sessionId = decodePathToken(route.sessionToken, "session id");
  const session = await sessionStore.open(sessionId, workspaceRoot);
  if (route.kind === "tree") {
    if (req.method !== "GET") throw httpError(405, "Method not allowed");
    const offset = readQueryInt(url.searchParams, "offset", 0, { min: 0 });
    const limit = readQueryInt(url.searchParams, "limit", undefined, { required: true, min: 1 });
    sendJson(res, 200, pageTree(session.nodes, offset, limit));
    return;
  }
  if (route.kind === "search") {
    if (req.method !== "GET") throw httpError(405, "Method not allowed");
    const query = url.searchParams.get("query");
    if (typeof query !== "string" || query.trim() === "" || query.length > 1024) {
      throw httpError(400, "Expected a non-empty query");
    }
    const limit = readQueryInt(url.searchParams, "limit", DEFAULT_TREE_SEARCH_LIMIT, { min: 1 });
    // WebHttpSessionTransport.searchAssemblyTree reads a JSON array of instance ids.
    sendJson(res, 200, searchTree(session.nodes, query.trim(), limit));
    return;
  }
  if (route.kind === "hlod") {
    if (req.method !== "GET") throw httpError(405, "Method not allowed");
    const chunkToken = decodePathToken(route.chunkToken, "chunk id");
    if (!/^(?:0|[1-9]\d{0,15})$/.test(chunkToken)) throw httpError(400, "Invalid chunk id");
    const chunkId = Number(chunkToken);
    if (!Number.isSafeInteger(chunkId) || chunkId < 0) throw httpError(400, "Invalid chunk id");
    writeChunkResponse(res, await session.readChunk(chunkId), req.headers.range);
    return;
  }
  if (req.method !== "POST") throw httpError(405, "Method not allowed");
  const body = await readJsonBody(req);
  const roiMin = requireVec3Field(body?.roi_min_mm, "roi_min_mm");
  const roiMax = requireVec3Field(body?.roi_max_mm, "roi_max_mm");
  const matches = session.nodes.filter((node) => boxesIntersect(
    node.bbox_min_mm,
    node.bbox_max_mm,
    roiMin,
    roiMax,
  ));
  const summaryJson = buildInterfaceSummary(roiMin, roiMax, matches);
  if (Buffer.byteLength(summaryJson, "utf8") >= MAX_DIGEST_JSON_BYTES) {
    throw httpError(500, "Interface digest exceeded 12 KiB");
  }
  sendJson(res, 200, {
    roi_min_mm: roiMin,
    roi_max_mm: roiMax,
    component_count: matches.length,
    summary_json: summaryJson,
  });
}

/**
 * Attach an in-memory or `.cadpack`-byte session to a server from `createStudioServer`.
 * File-backed sessions can also be dropped in `<workspace>/sessions/<id>.cadpack`.
 */
export function registerStudioSession(server, seed) {
  if (typeof server?.registerStudioSession !== "function") {
    throw new TypeError("registerStudioSession requires a createStudioServer instance");
  }
  return server.registerStudioSession(seed);
}

function encodeDemoHlodBox(node, level) {
  const anchor = node.bbox_min_mm;
  const [x, y, z] = node.bbox_max_mm.map((max, axis) => max - anchor[axis]);
  // Each face has its own outward winding, normals, and vertex slots.
  const faces = [
    { origin: [0, 0, 0], u: [0, 0, z], v: [0, y, 0], normal: [-1, 0, 0] },
    { origin: [x, 0, 0], u: [0, y, 0], v: [0, 0, z], normal: [1, 0, 0] },
    { origin: [0, 0, 0], u: [x, 0, 0], v: [0, 0, z], normal: [0, -1, 0] },
    { origin: [0, y, 0], u: [0, 0, z], v: [x, 0, 0], normal: [0, 1, 0] },
    { origin: [0, 0, 0], u: [0, y, 0], v: [x, 0, 0], normal: [0, 0, -1] },
    { origin: [0, 0, z], u: [x, 0, 0], v: [0, y, 0], normal: [0, 0, 1] },
  ];
  const divisions = 3 - level;
  const stride = divisions + 1;
  const positions = [];
  const normals = [];
  const faceSlots = [];
  const indices = [];
  for (const [slot, { origin, u, v, normal }] of faces.entries()) {
    const base = faceSlots.length;
    for (let row = 0; row <= divisions; row += 1) {
      for (let col = 0; col <= divisions; col += 1) {
        positions.push(...origin.map((value, axis) => value + u[axis] * col / divisions + v[axis] * row / divisions));
        normals.push(...normal);
        faceSlots.push(slot);
      }
    }
    for (let row = 0; row < divisions; row += 1) {
      for (let col = 0; col < divisions; col += 1) {
        const corner = base + row * stride + col;
        indices.push(corner, corner + 1, corner + stride + 1, corner, corner + stride + 1, corner + stride);
      }
    }
  }

  const headerBytes = 56;
  const vertexCount = faceSlots.length;
  const bytes = Buffer.alloc(headerBytes + 4 * (positions.length + normals.length + vertexCount + indices.length));
  bytes.write("HLD1");
  bytes.writeUInt32LE(1, 4);
  bytes.writeUInt32LE(node.prototype_id, 8);
  bytes.writeUInt8(level, 12);
  anchor.forEach((value, axis) => bytes.writeDoubleLE(value, 16 + axis * 8));
  bytes.writeUInt32LE(vertexCount, 40);
  bytes.writeUInt32LE(vertexCount, 44);
  bytes.writeUInt32LE(vertexCount, 48);
  bytes.writeUInt32LE(indices.length, 52);
  let cursor = headerBytes;
  for (const value of [...positions, ...normals]) cursor = bytes.writeFloatLE(value, cursor);
  for (const value of [...faceSlots, ...indices]) cursor = bytes.writeUInt32LE(value, cursor);
  return bytes;
}

/** Build the in-memory assembly used by the Studio HLOD demo. */
export function buildDemoSessionSeed() {
  const nodes = [
    {
      instance_id: 1, parent_id: null, prototype_id: 0,
      name: "Stepper drive assembly", part_number: "DRV-ASM-001",
      bbox_min_mm: [0, 0, 0], bbox_max_mm: [140, 90, 78], frozen_lightweight: true,
    },
    {
      instance_id: 2, parent_id: 1, prototype_id: 1,
      name: "Aluminum mounting plate", part_number: "DRV-PLT-140",
      bbox_min_mm: [0, 0, 0], bbox_max_mm: [140, 90, 12], frozen_lightweight: true,
    },
    {
      instance_id: 3, parent_id: 1, prototype_id: 2,
      name: "NEMA 23 stepper motor", part_number: "DRV-MTR-023",
      bbox_min_mm: [42, 16, 12], bbox_max_mm: [98, 72, 70], frozen_lightweight: true,
    },
    {
      instance_id: 4, parent_id: 1, prototype_id: 3,
      name: "Terminal cover", part_number: "DRV-CVR-080",
      bbox_min_mm: [30, 10, 70], bbox_max_mm: [110, 80, 78], frozen_lightweight: true,
    },
  ];
  const chunks = {};
  for (const node of nodes) {
    for (let level = 0; level < 3; level += 1) {
      chunks[node.prototype_id * 10 + level] = encodeDemoHlodBox(node, level);
    }
  }
  return { id: "demo", nodes, chunks };
}

const KNOWN_EXPORT_KINDS = new Set(["step", "dxf", "drawing", "3mf", "mesh"]);
const KNOWN_ASSET_FORMATS = new Set(["step", "brep", "iges", "stl", "3mf", "obj", "gltf"]);
const MAX_TRACE_FIELD_CHARS = 128;

function normalizeExportKindForLog(raw) {
  if (typeof raw !== "string") return null;
  const normalized = raw.trim().toLowerCase();
  return KNOWN_EXPORT_KINDS.has(normalized) ? normalized : null;
}

function formatFromAssetName(raw) {
  if (typeof raw !== "string") return null;
  const clean = raw.trim().split(/[?#]/)[0];
  const ext = path.posix.extname(clean.replace(/\\/g, "/")).slice(1).toLowerCase();
  if (!ext) return null;
  const mapped = { stp: "step", igs: "iges", glb: "gltf" }[ext] ?? ext;
  return KNOWN_ASSET_FORMATS.has(mapped) ? mapped : "unknown";
}

function normalizeAssetFormatForLog(rawFormat, fallbackPath) {
  if (typeof rawFormat === "string") {
    const normalized = rawFormat.trim().toLowerCase();
    const mapped = { stp: "step", igs: "iges", glb: "gltf" }[normalized] ?? normalized;
    if (KNOWN_ASSET_FORMATS.has(mapped)) return mapped;
  }
  return formatFromAssetName(fallbackPath);
}

function sanitizeTraceField(value, maxLen = MAX_TRACE_FIELD_CHARS) {
  if (typeof value !== "string") return null;
  const sanitized = sanitizeTextSecrets(value).replace(/[\r\n\t]+/g, " ").trim();
  if (!sanitized) return null;
  return sanitized.length > maxLen ? sanitized.slice(0, maxLen) : sanitized;
}

function sanitizeAssetForLog(raw) {
  if (typeof raw !== "string") return null;
  const clean = raw.trim().replace(/\\/g, "/").split(/[?#]/)[0];
  if (!clean) return null;
  if (/^assets\/[A-Za-z0-9][A-Za-z0-9_.-]{0,120}$/.test(clean)) {
    return sanitizeTextSecrets(clean);
  }
  const base = path.posix.basename(clean).slice(0, 120);
  if (!base || base === "." || base === "..") return "[invalid-asset]";
  return sanitizeTextSecrets(base);
}

function extractDiagnosticCode(payload) {
  if (!payload || typeof payload !== "object") return null;
  if (typeof payload.code === "string" && payload.code) return payload.code;
  const list = Array.isArray(payload.diagnostics)
    ? payload.diagnostics
    : Array.isArray(payload.diagnostics?.diagnostics)
      ? payload.diagnostics.diagnostics
      : null;
  if (list) {
    const firstErr = list.find((d) => d && typeof d.code === "string" && d.severity === "error")
      ?? list.find((d) => d && typeof d.code === "string");
    if (firstErr) return firstErr.code;
  }
  return null;
}

export function createStudioServer(options = {}) {
  // This setting belongs to the host, never to an HTTP request.
  const configuredRoot = path.resolve(
    options.workspaceRoot
    || process.env.CAD_STUDIO_WORKSPACE_DIR
    || path.join(REPO_ROOT, ".studio-workspace"),
  );
  let workspace;
  const workspaceRoot = async () => {
    const root = await (workspace ??= openWorkspace(configuredRoot).catch((err) => {
      workspace = undefined;
      throw err;
    }));
    if (await fs.realpath(root).catch(() => null) !== root) {
      throw httpError(503, "The Studio workspace is no longer available");
    }
    return root;
  };

  const projectCliRunner = typeof options.projectCliRunner === "function"
    ? options.projectCliRunner
    : runProjectCli;
  const mcpToolRunner = typeof options.mcpToolRunner === "function"
    ? options.mcpToolRunner
    : null;
  const agentCliRunner = typeof options.agentCliRunner === "function"
    ? options.agentCliRunner
    : null;
  const cadCliRunner = typeof options.cadCliRunner === "function"
    ? options.cadCliRunner
    : null;
  const onTraceLog = typeof options.onTraceLog === "function" ? options.onTraceLog : null;
  const emitTraceLog = (entry) => {
    const record = {
      trace_id: sanitizeTraceField(currentTraceId(), 64) ?? newTraceId(),
      stage: sanitizeTraceField(entry.stage, 32) ?? "unknown",
      asset: sanitizeTraceField(entry.asset),
      format: sanitizeTraceField(entry.format, 32),
      export_kind: sanitizeTraceField(entry.export_kind, 32),
      status: entry.status === "ok" ? "ok" : "error",
      diagnostic_code: sanitizeTraceField(entry.diagnostic_code, 64),
    };
    const line = JSON.stringify(record);
    console.log(line);
    if (onTraceLog) onTraceLog(record, line);
    return record;
  };

  const sessionStore = createSessionStore();
  const sessionSeeds = options.sessions ?? [];
  if (!Array.isArray(sessionSeeds)) throw httpError(422, "sessions must be an array");
  for (const seed of sessionSeeds) sessionStore.add(seed);
  if (!sessionSeeds.some((seed) => seed.id === "demo")) sessionStore.add(buildDemoSessionSeed());

  const server = http.createServer({ requestTimeout: 60_000 }, (req, res) => {
    const headerTrace = req.headers["x-trace-id"];
    const traceId = typeof headerTrace === "string" && TRACE_ID_PATTERN.test(headerTrace)
      ? headerTrace
      : newTraceId();
    res.setHeader("x-trace-id", traceId);
    traceStore.run({ traceId }, () => handleRequest(req, res));
  });
  const handleRequest = async (req, res) => {
    let traceLogged = false;
    let traceContext = null;
    const url = new URL(req.url || "/", "http://localhost");
    try {
      if (url.pathname.startsWith("/api/")) {
        // All CAD endpoints share the local workspace; reject cross-origin browser access.
        const host = req.headers.host;
        const origin = req.headers.origin;
        const isLoopbackOrigin = typeof origin === "string"
          && /^https?:\/\/(?:localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$/i.test(origin);
        if (req.headers["sec-fetch-site"] === "cross-site" ||
            (origin && origin !== `http://${host}` && origin !== `https://${host}` && !isLoopbackOrigin)) {
          throw httpError(403, "Cross-origin asset requests are not supported");
        }
        // Prevent DNS rebinding when using the default loopback listener.
        if (["127.0.0.1", "localhost", "::1"].includes(HOST) &&
            !/^(?:localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$/i.test(req.headers.host ?? "")) {
          throw httpError(403, "Invalid local workbench host");
        }
      }
      const sessionRoute = matchSessionRoute(req.url);
      if (sessionRoute) {
        await handleSessionRoute(req, res, url, sessionRoute, sessionStore, workspaceRoot);
        return;
      }
      if (url.pathname === "/api/assets" || url.pathname === "/api/inspect-asset") {
        try {
          const root = await workspaceRoot();
          const dir = await openAssetDirectory(root);
          if (req.method === "GET" && url.pathname === "/api/assets") {
            sendJson(res, 200, { assets: await listAssets(dir) });
            return;
          }
          if (req.method === "POST" && url.pathname === "/api/assets") {
            const rawName = url.searchParams.get("name");
            traceContext = {
              stage: "upload",
              asset: sanitizeAssetForLog(rawName),
              format: formatFromAssetName(rawName),
              export_kind: null,
            };
            if (req.headers["content-type"] !== "application/octet-stream") {
              throw httpError(415, "Upload raw asset bytes as application/octet-stream");
            }
            const name = normalizeAssetName(rawName);
            const bytes = await readBody(req, MAX_ASSET_BYTES);
            const asset = await storeAsset(dir, name, bytes);
            emitTraceLog({
              stage: "upload",
              asset: asset.path,
              format: asset.format,
              export_kind: null,
              status: "ok",
              diagnostic_code: null,
            });
            traceLogged = true;
            sendJson(res, 201, { asset });
            return;
          }
          if (req.method === "POST" && url.pathname === "/api/inspect-asset") {
            traceContext = {
              stage: "inspect-asset",
              asset: null,
              format: null,
              export_kind: null,
            };
            const body = await readJsonBody(req);
            traceContext = {
              stage: "inspect-asset",
              asset: sanitizeAssetForLog(body?.path),
              format: formatFromAssetName(body?.path),
              export_kind: null,
            };
            const report = await inspectAsset(root, dir, body);
            emitTraceLog({
              stage: "inspect-asset",
              asset: sanitizeAssetForLog(report?.file ?? body?.path),
              format: normalizeAssetFormatForLog(report?.format, body?.path),
              export_kind: null,
              status: report?.status === "error" ? "error" : "ok",
              diagnostic_code: extractDiagnosticCode(report),
            });
            traceLogged = true;
            sendJson(res, 200, report);
            return;
          }
          throw httpError(405, "Method not allowed");
        } catch (err) {
          if (err.statusCode) throw err;
          throw httpError(500, "Could not access the workspace assets directory");
        }
      }

      if (req.method === "GET" && url.pathname === "/") {
        res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
        res.end(INDEX_HTML);
        return;
      }

      if (req.method === "GET" && url.pathname === "/api/examples") {
        const examples = await collectKclExamples();
        sendJson(res, 200, { count: examples.length, examples });
        return;
      }

      if (req.method === "GET" && url.pathname === "/api/ai-config") {
        sendJson(res, 200, getServerAiConfig());
        return;
      }

      if (req.method === "GET" && url.pathname === "/api/ai-profiles") {
        const store = await loadClientAiProfileStore();
        sendJson(res, 200, {
          ...store,
          envStatus: getServerAiConfig(),
        });
        return;
      }

      if (req.method === "PUT" && url.pathname === "/api/ai-profiles") {
        let body;
        try {
          body = await readJsonBody(req);
        } catch (parseErr) {
          sendJson(res, parseErr.statusCode ?? 400, {
            ok: false,
            error: parseErr instanceof Error ? parseErr.message : "Invalid JSON request body",
          });
          return;
        }
        const saved = await saveAiProfileStore(body);
        sendJson(res, 200, {
          ...saved,
          envStatus: getServerAiConfig(),
        });
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/ai-test") {
        let body;
        try {
          body = await readJsonBody(req);
        } catch (parseErr) {
          sendJson(res, parseErr.statusCode ?? 400, {
            ok: false,
            error: parseErr instanceof Error ? parseErr.message : "Invalid JSON request body",
          });
          return;
        }
        const testResult = await testAiProfileConnection(body);
        sendJson(res, 200, testResult);
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/compile") {
        traceContext = {
          stage: "compile",
          asset: null,
          format: "kcl",
          export_kind: null,
        };
        const body = await readJsonBody(req);
        const fileName = typeof body.file === "string" ? body.file : "untitled.kcl";
        const previewOnly = typeof body.previewOnly === "boolean"
          ? body.previewOnly
          : (typeof body.source === "string" && /\bimport\s*::\s*mesh\b/.test(body.source));
        const exportKind = normalizeExportKindForLog(body.exportKind);
        const stage = previewOnly ? "preview" : (exportKind ? "export" : "compile");
        const format = stage === "preview" ? "scene-mesh" : (exportKind ?? "kcl");
        traceContext = {
          stage,
          asset: sanitizeAssetForLog(fileName),
          format,
          export_kind: exportKind,
        };
        if (typeof body.source !== "string" || !body.source.trim()) {
          emitTraceLog({
            ...traceContext,
            status: "error",
            diagnostic_code: "HTTP_400",
          });
          traceLogged = true;
          sendJson(res, 400, { error: "Missing non-empty 'source' field" });
          return;
        }
        // The workbench aborts this fetch to cancel an export. Stop cad-cli and
        // do not publish the artifact; runCadCliBuild already deletes its temp dir.
        const abortController = new AbortController();
        const onClientDisconnect = () => {
          if (!res.writableEnded) abortController.abort();
        };
        res.on("close", onClientDisconnect);
        req.on("aborted", onClientDisconnect);
        let result;
        try {
          result = await runCadCliBuild(
            fileName,
            body.source,
            await workspaceRoot(),
            typeof body.componentId === "string" ? body.componentId : undefined,
            previewOnly,
            cadCliRunner,
            abortController.signal,
          );
        } catch (buildErr) {
          if (isClientAbort(abortController.signal, buildErr)) {
            emitTraceLog({
              ...traceContext,
              status: "error",
              diagnostic_code: "CLIENT_ABORTED",
            });
            traceLogged = true;
            return;
          }
          throw buildErr;
        } finally {
          res.off("close", onClientDisconnect);
          req.off("aborted", onClientDisconnect);
        }
        if (abortController.signal.aborted || res.writableEnded) {
          emitTraceLog({
            ...traceContext,
            status: "error",
            diagnostic_code: "CLIENT_ABORTED",
          });
          traceLogged = true;
          return;
        }
        const requestedReady = exportKind === "step" ? Boolean(result.stepContent)
          : exportKind === "dxf" ? Boolean(result.dxfContent)
          : exportKind === "drawing" ? Boolean(result.drawingSvg)
          : exportKind === "3mf" ? Boolean(result.threemfBase64)
          : result.ok;
        const suppressUnrelatedExportFailure = Boolean(exportKind) && requestedReady;
        emitTraceLog({
          ...traceContext,
          status: requestedReady ? "ok" : "error",
          diagnostic_code: suppressUnrelatedExportFailure
            ? null
            : (extractDiagnosticCode(result.diagnostics) ?? result.code ?? null),
        });
        traceLogged = true;
        sendJson(res, 200, result);
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/lsp") {
        const body = await readJsonBody(req);
        if (typeof body.source !== "string") {
          sendJson(res, 400, { error: "Missing 'source' field" });
          return;
        }
        const line = body.line ?? 1;
        const col = body.col ?? 1;
        if (![line, col].every((value) => Number.isSafeInteger(value) && value > 0 && value <= 0xffffffff)) {
          sendJson(res, 400, { error: "LSP line and col must be positive 1-based integers" });
          return;
        }
        const result = await runLspQuery(
          typeof body.file === "string" ? body.file : "query.kcl", body.source, await workspaceRoot(), line, col,
        );
        sendJson(res, 200, result);
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/agent-generate") {
        let body;
        try {
          body = await readJsonBody(req);
        } catch (parseErr) {
          const envSecrets = [
            process.env.OPENAI_API_KEY,
            process.env.GEMINI_API_KEY,
          ];
          sendJson(res, parseErr.statusCode ?? 400, {
            ok: false,
            error: sanitizeTextSecrets(
              parseErr instanceof Error ? parseErr.message : "Invalid JSON request body",
              envSecrets,
            ),
            error_code: "invalid_json",
          });
          return;
        }
        const secretsToRedact = [
          body?.llm?.api_key,
          process.env.OPENAI_API_KEY,
          process.env.GEMINI_API_KEY,
        ];
        try {
          const validated = validateAgentGenerateBody(body);
          if (!validated.ok) {
            sendJson(
              res,
              validated.status,
              sanitizeValueSecrets(validated.payload, secretsToRedact),
            );
            return;
          }
          const resolvedLlm = await resolveLlmWithSavedProfile(validated.llm);
          if (resolvedLlm.api_key) {
            secretsToRedact.push(resolvedLlm.api_key);
          }
          const result = await runAgentGenerate(
            validated.prompt,
            validated.material,
            validated.manufacturingMethod,
            await workspaceRoot(),
            {
              mode: validated.mode,
              existingSource: validated.existingSource,
              projectPath: validated.projectPath,
              selectedTopoRefs: validated.selectedTopoRefs,
              selectedNodeIds: validated.selectedNodeIds,
              sessionId: validated.sessionId,
              contextAsset: validated.contextAsset,
              previewBranch: validated.previewBranch,
              llm: resolvedLlm,
              agentCliRunner,
              mcpToolRunner,
              cadCliRunner,
              projectCliRunner,
            },
          );
          sendJson(res, 200, sanitizeValueSecrets(result, secretsToRedact));
          return;
        } catch (agentErr) {
          const status = typeof agentErr?.statusCode === "number" ? agentErr.statusCode : 500;
          if (
            agentErr &&
            typeof agentErr === "object" &&
            agentErr.payload &&
            typeof agentErr.payload === "object"
          ) {
            sendJson(res, status, sanitizeValueSecrets(agentErr.payload, secretsToRedact));
            return;
          }
          sendJson(res, status, {
            ok: false,
            error: sanitizeTextSecrets(
              agentErr instanceof Error ? agentErr.message : String(agentErr),
              secretsToRedact,
            ),
            error_code: "agent_error",
          });
          return;
        }
      }

      if (req.method === "POST" && url.pathname === "/api/agent-generate-stream") {
        let body;
        try {
          body = await readJsonBody(req);
        } catch (parseErr) {
          const envSecrets = [
            process.env.OPENAI_API_KEY,
            process.env.GEMINI_API_KEY,
          ];
          res.writeHead(200, {
            "Content-Type": "text/event-stream; charset=utf-8",
            "Cache-Control": "no-cache",
            Connection: "keep-alive",
          });
          writeSseEvent(
            res,
            "error",
            {
              ok: false,
              status: 400,
              error: sanitizeTextSecrets(
                parseErr instanceof Error ? parseErr.message : "Invalid JSON request body",
                envSecrets,
              ),
              error_code: "invalid_json",
            },
            envSecrets,
          );
          res.end();
          return;
        }

        res.writeHead(200, {
          "Content-Type": "text/event-stream; charset=utf-8",
          "Cache-Control": "no-cache",
          Connection: "keep-alive",
        });

        const abortController = new AbortController();
        const onClientDisconnect = () => {
          if (!res.writableEnded) {
            abortController.abort();
          }
        };
        res.on("close", onClientDisconnect);
        req.on("aborted", onClientDisconnect);

        try {
          const validated = validateAgentGenerateBody(body);
          const secretsToRedact = [
            validated.ok ? validated.llm.api_key : body?.llm?.api_key,
            process.env.OPENAI_API_KEY,
            process.env.GEMINI_API_KEY,
          ];
          if (!validated.ok) {
            writeSseEvent(
              res,
              "error",
              {
                ok: false,
                status: validated.status,
                ...validated.payload,
              },
              secretsToRedact,
            );
            res.end();
            return;
          }

          const resolvedLlm = await resolveLlmWithSavedProfile(validated.llm);
          if (resolvedLlm.api_key) {
            secretsToRedact.push(resolvedLlm.api_key);
          }
          const result = await runAgentGenerate(
            validated.prompt,
            validated.material,
            validated.manufacturingMethod,
            await workspaceRoot(),
            {
              mode: validated.mode,
              existingSource: validated.existingSource,
              projectPath: validated.projectPath,
              selectedTopoRefs: validated.selectedTopoRefs,
              selectedNodeIds: validated.selectedNodeIds,
              sessionId: validated.sessionId,
              contextAsset: validated.contextAsset,
              previewBranch: validated.previewBranch,
              llm: resolvedLlm,
              streamProgress: true,
              signal: abortController.signal,
              agentCliRunner,
              mcpToolRunner,
              cadCliRunner,
              projectCliRunner,
              onProgress: (ev) => {
                writeSseEvent(res, "progress", ev, secretsToRedact);
              },
            },
          );
          if (!abortController.signal.aborted && result) {
            writeSseEvent(res, "result", result, secretsToRedact);
          }
        } catch (streamErr) {
          if (!abortController.signal.aborted) {
            const status = typeof streamErr?.statusCode === "number" ? streamErr.statusCode : 500;
            const secretsToRedact = [
              body?.llm?.api_key,
              process.env.OPENAI_API_KEY,
              process.env.GEMINI_API_KEY,
            ];
            const errPayload =
              streamErr && typeof streamErr === "object" && streamErr.payload && typeof streamErr.payload === "object"
                ? { ok: false, status, ...streamErr.payload }
                : {
                    ok: false,
                    status,
                    error: sanitizeTextSecrets(
                      streamErr instanceof Error ? streamErr.message : String(streamErr),
                      secretsToRedact,
                    ),
                    error_code: "agent_error",
                  };
            writeSseEvent(res, "error", errPayload, secretsToRedact);
          }
        } finally {
          res.off("close", onClientDisconnect);
          req.off("aborted", onClientDisconnect);
          if (!res.writableEnded) {
            res.end();
          }
        }
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/writeback") {
        const body = await readJsonBody(req);
        const { source, handle, targetValue } = body;
        const paramName = handle && typeof handle.param_name === "string" ? handle.param_name : "";
        const unit = handle && typeof handle.unit === "string" ? handle.unit : "mm";
        if (typeof source !== "string" || !paramName || typeof targetValue !== "number") {
          sendJson(res, 400, { error: "Invalid writeback payload" });
          return;
        }
        try {
          const updatedSource = projectParameterIntoKcl(
            source,
            parameterFromDimension(paramName, targetValue, unit),
          );
          sendJson(res, 200, { kind: "tier1", updatedSource, paramName });
        } catch (error) {
          const message = error instanceof SessionCommandError
            ? error.message
            : error instanceof Error
              ? error.message
              : String(error);
          sendJson(res, 400, { error: message });
        }
        return;
      }

      if (req.method === "POST" && (
        url.pathname === "/api/project/init"
        || url.pathname === "/api/project/inspect"
        || url.pathname === "/api/project/apply-patch"
        || url.pathname === "/api/project/emit-kcl"
        || url.pathname === "/api/project/branch"
        || url.pathname === "/api/project/preview/diff"
        || url.pathname === "/api/project/preview/commit"
        || url.pathname === "/api/project/preview/discard"
        || url.pathname === "/api/project/slice-context"
        || url.pathname === "/api/project/vacuum"
        || url.pathname === "/api/project/pack-and-go"
      )) {
        const root = await workspaceRoot();
        const body = await readJsonBody(req);
        if (url.pathname === "/api/project/init") {
          sendJson(res, 200, await runProjectInit(root, body, projectCliRunner));
          return;
        }
        if (url.pathname === "/api/project/inspect") {
          sendJson(res, 200, await runProjectInspect(root, body, projectCliRunner));
          return;
        }
        if (url.pathname === "/api/project/apply-patch") {
          sendJson(res, 200, await runProjectApplyPatch(root, body, projectCliRunner));
          return;
        }
        if (url.pathname === "/api/project/emit-kcl") {
          sendJson(res, 200, await runProjectEmitKcl(root, body, projectCliRunner));
          return;
        }
        if (url.pathname === "/api/project/branch") {
          sendJson(res, 200, await runProjectBranch(root, body, projectCliRunner, mcpToolRunner));
          return;
        }
        if (url.pathname === "/api/project/preview/diff") {
          sendJson(res, 200, await runProjectPreviewBranchOp(root, body, "diff", mcpToolRunner));
          return;
        }
        if (url.pathname === "/api/project/preview/commit") {
          sendJson(res, 200, await runProjectPreviewBranchOp(root, body, "commit_preview", mcpToolRunner));
          return;
        }
        if (url.pathname === "/api/project/preview/discard") {
          sendJson(res, 200, await runProjectPreviewBranchOp(root, body, "discard_preview", mcpToolRunner));
          return;
        }
        if (url.pathname === "/api/project/slice-context") {
          sendJson(res, 200, await runProjectSliceContext(root, body, projectCliRunner));
          return;
        }
        if (url.pathname === "/api/project/vacuum") {
          sendJson(res, 200, await runProjectVacuum(root, body, projectCliRunner));
          return;
        }
        sendJson(res, 200, await runProjectPackAndGo(root, body, projectCliRunner));
        return;
      }

      sendJson(res, 404, { error: "Not found" });
    } catch (err) {
      const envSecrets = [
        process.env.OPENAI_API_KEY,
        process.env.GEMINI_API_KEY,
      ];
      const status = typeof err.statusCode === "number" ? err.statusCode : 500;
      if (!traceLogged && req.method === "POST") {
        const fallbackContext = traceContext ?? (
          url.pathname === "/api/assets"
            ? {
                stage: "upload",
                asset: sanitizeAssetForLog(url.searchParams.get("name")),
                format: formatFromAssetName(url.searchParams.get("name")),
                export_kind: null,
              }
            : url.pathname === "/api/inspect-asset"
              ? { stage: "inspect-asset", asset: null, format: null, export_kind: null }
              : url.pathname === "/api/compile"
                ? { stage: "compile", asset: null, format: "kcl", export_kind: null }
                : null
        );
        if (fallbackContext) {
          emitTraceLog({
            ...fallbackContext,
            status: "error",
            diagnostic_code: extractDiagnosticCode(err?.payload)
              ?? (typeof err?.code === "string" ? err.code : null)
              ?? `HTTP_${status}`,
          });
        }
      }
      if (err && typeof err === "object" && err.payload && typeof err.payload === "object") {
        sendJson(res, status, sanitizeValueSecrets(err.payload, envSecrets));
        return;
      }
      sendJson(res, status, {
        error: sanitizeTextSecrets(err instanceof Error ? err.message : String(err), envSecrets),
      });
    }
  };
  server.registerStudioSession = (seed) => sessionStore.add(seed);
  return server;
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  const server = createStudioServer();
  server.listen(PORT, HOST, () => {
    console.log(`AI-Native CAD Studio dev-server listening at http://${HOST}:${PORT}`);
  });
}
