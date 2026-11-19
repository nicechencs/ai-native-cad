import { execFile } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import fs from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  applyTier2Choice,
  applyTier3ExtractParameter,
  computeWritebackPlan,
} from "./src/sync/astWriteback.ts";
import {
  analyzeKclLspFallback,
  applyKclCompletion,
  clipToSection,
  generateFallbackDrawingSvg,
  generateFallbackDxfFromMesh,
  generateFallbackKclFromPrompt,
  handleRange,
} from "./src/utils/studioHelpers.ts";

const execFileAsync = promisify(execFile);

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const REPO_ROOT = path.resolve(__dirname, "..", "..");

const HOST = process.env.HOST || "127.0.0.1";
const PORT = Number.parseInt(process.env.PORT || "5173", 10);

const cliCapabilityCache = new Map();

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

async function detectCliCapabilities(cliBin) {
  const mtime = statSync(cliBin).mtimeMs;
  const cacheKey = `${cliBin}:${mtime}`;
  const cached = cliCapabilityCache.get(cacheKey);
  if (cached) {
    return cached;
  }

  let rootHelp = "";
  let buildHelp = "";
  try {
    const r1 = await execFileAsync(cliBin, ["--help"]);
    rootHelp = `${r1.stdout ?? ""}\n${r1.stderr ?? ""}`;
  } catch {
    // Ignore help probe failure
  }
  try {
    const r2 = await execFileAsync(cliBin, ["build", "--help"]);
    buildHelp = `${r2.stdout ?? ""}\n${r2.stderr ?? ""}`;
  } catch {
    // Ignore build help probe failure
  }

  const caps = {
    hasOutDxf: buildHelp.includes("--out-dxf"),
    hasOutDrawing: buildHelp.includes("--out-drawing"),
    hasComponent: buildHelp.includes("--component"),
    hasLsp: /\blsp\b/.test(rootHelp),
  };
  cliCapabilityCache.set(cacheKey, caps);
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

async function runCadCliBuild(fileName, source, componentId) {
  const cliBin = resolveCadCliBinary();
  if (!cliBin) {
    const err = new Error(
      "Compiled cad-cli binary not found in target/debug or target/release.",
    );
    err.statusCode = 503;
    throw err;
  }

  const caps = await detectCliCapabilities(cliBin);
  if (componentId && !caps.hasComponent) {
    const err = new Error("This cad-cli build does not support component export (--component).");
    err.statusCode = 501;
    throw err;
  }
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-studio-"));
  const safeBase = path
    .basename(fileName || "model.kcl")
    .replace(/[^a-zA-Z0-9_.-]/g, "_");
  const kclName = safeBase.endsWith(".kcl") ? safeBase : `${safeBase}.kcl`;
  const kclPath = path.join(tmpDir, kclName);
  const stepPath = path.join(tmpDir, "output.step");
  const meshPath = path.join(tmpDir, "output.mesh.json");
  const viewsDir = path.join(tmpDir, "views");
  const dxfPath = path.join(tmpDir, "output.dxf");
  const drawingPath = path.join(tmpDir, "drawing.svg");

  try {
    await fs.writeFile(kclPath, source, "utf8");
    await fs.mkdir(viewsDir, { recursive: true });

    const args = [
      "build",
      kclPath,
      "--out-step",
      stepPath,
      "--out-mesh",
      meshPath,
      "--out-views",
      viewsDir,
    ];
    if (caps.hasOutDxf) {
      args.push("--out-dxf", dxfPath);
    }
    if (caps.hasOutDrawing) {
      args.push("--out-drawing", drawingPath);
    }
    if (caps.hasComponent && componentId) {
      args.push("--component", componentId);
    }
    args.push("--json");

    let stdout = "";
    let stderr = "";
    let exitCode = 0;

    try {
      const res = await execFileAsync(cliBin, args, {
        maxBuffer: 25 * 1024 * 1024,
      });
      stdout = res.stdout ?? "";
      stderr = res.stderr ?? "";
    } catch (execErr) {
      exitCode = typeof execErr.code === "number" ? execErr.code : 1;
      stdout = execErr.stdout ?? "";
      stderr = execErr.stderr ?? execErr.message ?? "";
    }

    let diagnosticsReport;
    try {
      diagnosticsReport = JSON.parse(stdout);
    } catch {
      diagnosticsReport = {
        status: "error",
        file: fileName || kclName,
        diagnostics: [
          {
            code: "E_CLI_EXEC",
            severity: "error",
            message: stderr || stdout || "cad-cli failed without JSON output",
            span: { start_line: 1, start_col: 1, end_line: 1, end_col: 1 },
          },
        ],
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

    let dxfContent = "";
    if (existsSync(dxfPath)) {
      dxfContent = await fs.readFile(dxfPath, "utf8");
    } else if (sceneMesh && !caps.hasOutDxf) {
      dxfContent = generateFallbackDxfFromMesh(sceneMesh);
    }

    let drawingSvg = "";
    if (existsSync(drawingPath)) {
      drawingSvg = await fs.readFile(drawingPath, "utf8");
    } else if (sceneMesh && diagnosticsReport?.metrics) {
      drawingSvg = generateFallbackDrawingSvg(
        sceneMesh,
        diagnosticsReport.metrics,
        views,
      );
    }

    return {
      ok: diagnosticsReport.status === "ok" && exitCode === 0,
      exitCode,
      cliBinary: path.relative(REPO_ROOT, cliBin).replace(/\\/g, "/"),
      file: fileName || kclName,
      componentId: componentId || null,
      diagnostics: diagnosticsReport,
      metrics: diagnosticsReport.metrics,
      sceneMesh,
      views,
      stepContent,
      dxfContent,
      dxfIsCutProfile: existsSync(dxfPath),
      drawingSvg,
    };
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
  }
}

async function runLspQuery(fileName, source, line, col) {
  const cliBin = resolveCadCliBinary();
  if (cliBin) {
    const caps = await detectCliCapabilities(cliBin);
    if (caps.hasLsp) {
      const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-lsp-"));
      const kclPath = path.join(tmpDir, "query.kcl");
      try {
        await fs.writeFile(kclPath, source, "utf8");
        const args = ["lsp", kclPath];
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

async function runAgentGenerate(prompt, material, manufacturingMethod) {
  const pythonCandidates = ["python", "python3", "py"];
  const agentSrc = path.join(REPO_ROOT, "services", "cad-agent", "src");
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "cad-agent-ui-"));

  try {
    let generatedSource = "";
    let generatorBackend = "studio-fallback";
    let agentSummary = null;

    const cliBin = resolveCadCliBinary();
    if (cliBin && existsSync(path.join(agentSrc, "cad_agent", "cli.py"))) {
      const caps = await detectCliCapabilities(cliBin);
      const env = { ...process.env, PYTHONPATH: agentSrc, PYTHONUTF8: "1" };
      const args = [
        "-m", "cad_agent.cli", `--prompt=${prompt}`,
        `--material=${material}`, `--manufacturing-method=${manufacturingMethod}`,
        "--work-dir", tmpDir, "--cli-bin", cliBin, "--json",
      ];
      if (!caps.hasOutDxf || !caps.hasOutDrawing) args.push("--no-drawings");
      for (const pyBin of pythonCandidates) {
        let stdout;
        try {
          ({ stdout } = await execFileAsync(pyBin, args, {
            env, cwd: REPO_ROOT, maxBuffer: 10 * 1024 * 1024,
          }));
        } catch (err) {
          // A refinement run can return a candidate even if its gates failed.
          stdout = err.stdout;
        }
        try {
          const payload = JSON.parse(stdout || "{}");
          if (typeof payload.kcl_code !== "string" || !payload.kcl_code.trim()) continue;
          generatedSource = payload.kcl_code;
          generatorBackend = "cad-agent-cli";
          agentSummary = {
            passed: payload.passed,
            archetype: payload.archetype,
            iterations_run: payload.iterations_run,
            contract: payload.contract,
            verdict: payload.verdict,
          };
          break;
        } catch {
          // Try the next interpreter when no structured result was returned.
        }
      }
    }

    if (!generatedSource) {
      generatedSource = generateFallbackKclFromPrompt(
        prompt,
        material || "Al-6061-T6",
        manufacturingMethod || "CNC_3AXIS_MILLING",
      );
    }

    const compileResult = await runCadCliBuild("ai_generated.kcl", generatedSource);
    return {
      ok: compileResult.ok,
      prompt,
      generatorBackend,
      agentSummary,
      source: generatedSource,
      ...compileResult,
    };
  } finally {
    await fs.rm(tmpDir, { recursive: true, force: true });
  }
}

async function readJsonBody(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  const raw = Buffer.concat(chunks).toString("utf8");
  return raw ? JSON.parse(raw) : {};
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
          <span class="card-title">AI 自然语言生成模型 (L4 Agent)</span>
          <span id="ai-backend-badge" class="badge info">Ready</span>
        </div>
        <textarea id="ai-prompt-input" class="ai-prompt" rows="2">设计一个 96x68x36mm 壁厚 2.5mm 的铝合金抽壳控制盒，带镜像安装耳和 M4x0.7 螺纹孔</textarea>
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

const KW = new Set(["part","assembly","component","sketch","let","return","import","const","for","in","true","false"]);
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
    '<span style="margin-left:auto;color:var(--muted);font-family:monospace;">L' + item.span.start_line + ":" + item.span.start_col + "</span></div>" +
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

async function compileCurrent(noticeText, componentId = state.activeComponentFilter) {
  const request = ++state.modelRequest;
  const { source, currentFile: file } = state;
  setSyncStatus("正在调用 cad-cli 编译...", "info");
  try {
    const data = await postJson("/api/compile", { file, source, componentId });
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
    await compileCurrent("AST 双向同步完成 (" + active.param_name + ")");
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

async function init() {
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
      document.getElementById("ai-prompt-input").value = btn.getAttribute("data-prompt") || "";
    });
  });

  document.getElementById("ai-generate-btn").addEventListener("click", async () => {
    const prompt = document.getElementById("ai-prompt-input").value.trim();
    if (!prompt) return;
    const mat = document.getElementById("ai-material").value;
    const method = document.getElementById("ai-method").value;
    const request = ++state.modelRequest;
    const { source, currentFile } = state;
    const badge = document.getElementById("ai-backend-badge");
    const button = document.getElementById("ai-generate-btn");
    badge.textContent = "生成中...";
    button.disabled = true;
    try {
      const out = await postJson("/api/agent-generate", { prompt, material: mat, manufacturing_method: method });
      badge.textContent = out.generatorBackend;
      if (request !== state.modelRequest || source !== state.source || currentFile !== state.currentFile) return;
      state.currentFile = out.file;
      state.source = out.source;
      state.activeHandleId = null;
      state.selectedComponentId = null;
      state.highlightedSpan = null;
      state.explodeFactor = 0;
      document.getElementById("explode-slider").value = "0";
      sel.selectedIndex = -1;
      const methodLabel = out.generatorBackend === "cad-agent-cli" ? "Agent" : "模板";
      const status = out.agentSummary?.passed === false
        ? "生成完成，Agent 验证未通过"
        : out.ok ? "生成并编译通过" : "生成完成（含编译诊断）";
      showCompileData(out, methodLabel + " " + status);
    } catch (err) {
      badge.textContent = "生成失败";
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
  document.getElementById("download-step-btn").addEventListener("click", () => downloadFile(".step", state.compileData?.stepContent, "application/step"));
  document.getElementById("download-dxf-btn").addEventListener("click", () => downloadFile(".dxf", state.compileData?.dxfContent, "application/dxf"));
  document.getElementById("download-drawing-btn").addEventListener("click", () => downloadFile("_drawing.svg", state.compileData?.drawingSvg, "image/svg+xml"));
  document.getElementById("download-mesh-btn").addEventListener("click", () => downloadFile(".mesh.json", JSON.stringify(state.compileData?.sceneMesh || {}, null, 2), "application/json"));
}

void init();
</script>
</body>
</html>`;

export function createStudioServer() {
  return http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url || "/", `http://${req.headers.host || "127.0.0.1"}`);

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

      if (req.method === "POST" && url.pathname === "/api/compile") {
        const body = await readJsonBody(req);
        if (typeof body.source !== "string" || !body.source.trim()) {
          sendJson(res, 400, { error: "Missing non-empty 'source' field" });
          return;
        }
        const result = await runCadCliBuild(
          body.file || "untitled.kcl",
          body.source,
          typeof body.componentId === "string" ? body.componentId : undefined,
        );
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
        const result = await runLspQuery(body.file || "query.kcl", body.source, line, col);
        sendJson(res, 200, result);
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/agent-generate") {
        const body = await readJsonBody(req);
        if (typeof body.prompt !== "string" || !body.prompt.trim()) {
          sendJson(res, 400, { error: "Missing non-empty 'prompt' field" });
          return;
        }
        const result = await runAgentGenerate(
          body.prompt,
          typeof body.material === "string" ? body.material : "Al-6061-T6",
          typeof body.manufacturing_method === "string"
            ? body.manufacturing_method
            : "CNC_3AXIS_MILLING",
        );
        sendJson(res, 200, result);
        return;
      }

      if (req.method === "POST" && url.pathname === "/api/writeback") {
        const body = await readJsonBody(req);
        const { source, handle, targetValue, tier2Choice, applyTier3Extract, extractParamName } = body;
        if (typeof source !== "string" || !handle || typeof targetValue !== "number") {
          sendJson(res, 400, { error: "Invalid writeback payload" });
          return;
        }
        if (handle.tier === 2 && (tier2Choice === "offset" || tier2Choice === "upstream")) {
          const applied = applyTier2Choice(source, handle, targetValue, tier2Choice);
          sendJson(res, 200, { kind: "tier2-applied", ...applied });
          return;
        }
        if (handle.tier === 3 && applyTier3Extract) {
          const applied = applyTier3ExtractParameter(source, handle, targetValue, extractParamName);
          sendJson(res, 200, { kind: "tier3-applied", ...applied });
          return;
        }
        const plan = computeWritebackPlan(source, handle, targetValue, extractParamName);
        sendJson(res, 200, plan);
        return;
      }

      sendJson(res, 404, { error: "Not found" });
    } catch (err) {
      const status = typeof err.statusCode === "number" ? err.statusCode : 500;
      sendJson(res, status, { error: err instanceof Error ? err.message : String(err) });
    }
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  const server = createStudioServer();
  server.listen(PORT, HOST, () => {
    console.log(`AI-Native CAD Studio dev-server listening at http://${HOST}:${PORT}`);
  });
}
