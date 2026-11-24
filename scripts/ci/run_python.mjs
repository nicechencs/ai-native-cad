#!/usr/bin/env node
/**
 * Cross-platform Python 3 launcher for npm/pnpm scripts.
 *
 * Probes in order: CAD_AGENT_PYTHON, python, python3, py (-3 on Windows).
 * Passes all CLI arguments via argv without shell interpolation.
 */

import { spawnSync } from "node:child_process";

const args = process.argv.slice(2);
if (args.length === 0) {
  console.error("Usage: node scripts/ci/run_python.mjs <script-or-args...>");
  process.exit(2);
}

/** @type {Array<{ cmd: string, prefix: string[] }>} */
const candidates = [];
const envOverride = process.env.CAD_AGENT_PYTHON?.trim();
if (envOverride) {
  candidates.push({ cmd: envOverride, prefix: [] });
}
candidates.push({ cmd: "python", prefix: [] });
candidates.push({ cmd: "python3", prefix: [] });
if (process.platform === "win32") {
  candidates.push({ cmd: "py", prefix: ["-3"] });
}

function probePython3(candidate) {
  const probe = spawnSync(candidate.cmd, [...candidate.prefix, "--version"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (probe.error || probe.status !== 0) {
    return false;
  }
  const output = `${probe.stdout ?? ""} ${probe.stderr ?? ""}`;
  return /Python\s+3\./i.test(output);
}

const selected = candidates.find(probePython3);
if (!selected) {
  console.error(
    "Could not locate a Python 3 interpreter (tried CAD_AGENT_PYTHON, python, python3" +
      (process.platform === "win32" ? ", py -3" : "") +
      ")."
  );
  process.exit(127);
}

const result = spawnSync(selected.cmd, [...selected.prefix, ...args], {
  stdio: "inherit",
});
if (result.error) {
  console.error(`Failed to launch ${selected.cmd}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
