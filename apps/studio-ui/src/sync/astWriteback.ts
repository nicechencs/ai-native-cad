import type {
  EditHandle,
  SourceSpan,
  Tier2AffineBinding,
  Tier3CoupledBinding,
} from "../types/scene.ts";

export interface TextEdit {
  readonly startOffset: number;
  readonly endOffset: number;
  readonly newText: string;
  readonly span: SourceSpan;
}

export interface Tier1WritebackResult {
  readonly kind: "tier1";
  readonly applied: true;
  readonly handleId: string;
  readonly paramName: string;
  readonly previousValue: number;
  readonly nextValue: number;
  readonly unit: string;
  readonly previousText: string;
  readonly replacementText: string;
  readonly edit: TextEdit;
  readonly updatedSource: string;
}

export type Tier2ChoiceKind = "offset" | "upstream";

export interface Tier2PatchOption {
  readonly choice: Tier2ChoiceKind;
  readonly label: string;
  readonly description: string;
  readonly targetParam: string;
  readonly previousValue: number;
  readonly nextValue: number;
  readonly unit: string;
  readonly edit: TextEdit;
  readonly previewSource: string;
}

export interface Tier2WritebackResult {
  readonly kind: "tier2";
  readonly requiresChoice: true;
  readonly handleId: string;
  readonly paramName: string;
  readonly targetValue: number;
  readonly unit: string;
  readonly options: readonly [Tier2PatchOption, Tier2PatchOption];
}

export interface Tier3ExtractRefactoring {
  readonly extractedParamName: string;
  readonly extractedValue: number;
  readonly unit: string;
  readonly originalExpression: string;
  readonly dependencies: readonly string[];
  readonly edits: readonly TextEdit[];
  readonly previewSource: string;
}

export interface Tier3WritebackResult {
  readonly kind: "tier3";
  readonly blocked: true;
  readonly handleId: string;
  readonly paramName: string;
  readonly requestedValue: number;
  readonly reason: string;
  readonly highlightedSpan: SourceSpan;
  readonly refactor: Tier3ExtractRefactoring;
}

export type WritebackPlan =
  | Tier1WritebackResult
  | Tier2WritebackResult
  | Tier3WritebackResult;

export class AstWritebackError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "AstWritebackError";
    this.code = code;
  }
}

interface LineIndex {
  readonly lineStarts: readonly number[];
  readonly lineEnding: "\r\n" | "\n";
}

function buildLineIndex(source: string): LineIndex {
  const lineStarts: number[] = [0];
  let crlfCount = 0;
  let lfCount = 0;

  for (let i = 0; i < source.length; i += 1) {
    if (source[i] === "\n") {
      if (i > 0 && source[i - 1] === "\r") {
        crlfCount += 1;
      } else {
        lfCount += 1;
      }
      lineStarts.push(i + 1);
    }
  }

  return {
    lineStarts,
    lineEnding: crlfCount > lfCount ? "\r\n" : "\n",
  };
}

function lineColToOffset(index: LineIndex, sourceLength: number, line: number, col: number): number {
  if (line < 1 || line > index.lineStarts.length) {
    throw new AstWritebackError(
      "SPAN_OUT_OF_RANGE",
      `Line ${line} is out of range (1..${index.lineStarts.length}).`,
    );
  }
  if (col < 1) {
    throw new AstWritebackError("SPAN_OUT_OF_RANGE", `Column ${col} must be >= 1.`);
  }

  const lineStart = index.lineStarts[line - 1];
  const nextLineStart =
    line < index.lineStarts.length ? index.lineStarts[line] : sourceLength;
  const offset = lineStart + (col - 1);

  if (offset > nextLineStart) {
    throw new AstWritebackError(
      "SPAN_OUT_OF_RANGE",
      `Column ${col} at line ${line} exceeds line length.`,
    );
  }

  return offset;
}

export function spanToOffsets(
  source: string,
  span: SourceSpan,
): { readonly startOffset: number; readonly endOffset: number } {
  const index = buildLineIndex(source);
  const startOffset = lineColToOffset(index, source.length, span.start_line, span.start_col);
  const endOffset = lineColToOffset(index, source.length, span.end_line, span.end_col);

  if (endOffset < startOffset) {
    throw new AstWritebackError(
      "INVALID_SPAN",
      `Span end (${span.end_line}:${span.end_col}) precedes start (${span.start_line}:${span.start_col}).`,
    );
  }

  return { startOffset, endOffset };
}

export function extractSpanText(source: string, span: SourceSpan): string {
  const { startOffset, endOffset } = spanToOffsets(source, span);
  return source.slice(startOffset, endOffset);
}

export function applyTextEdits(source: string, edits: readonly TextEdit[]): string {
  const sorted = [...edits].sort((a, b) => b.startOffset - a.startOffset);

  for (let i = 1; i < sorted.length; i += 1) {
    const newer = sorted[i - 1];
    const older = sorted[i];
    if (older.endOffset > newer.startOffset) {
      throw new AstWritebackError(
        "OVERLAPPING_EDITS",
        `Overlapping edits detected at offsets [${older.startOffset}, ${older.endOffset}] and [${newer.startOffset}, ${newer.endOffset}].`,
      );
    }
  }

  let output = source;
  for (const edit of sorted) {
    output =
      output.slice(0, edit.startOffset) +
      edit.newText +
      output.slice(edit.endOffset);
  }
  return output;
}

function assertFiniteValue(value: number, paramName: string): void {
  if (!Number.isFinite(value)) {
    throw new AstWritebackError(
      "NON_FINITE_VALUE",
      `Parameter "${paramName}" requires a finite numeric value, got ${String(value)}.`,
    );
  }
}

/**
 * Formats a replacement number preserving decimal style of the original literal:
 * - If original had a decimal point (e.g. `42.0` or `12.50`), keep at least one decimal place
 *   while avoiding IEEE-754 float noise (`42.0000000001`).
 * - If original was an integer (e.g. `6`), keep integer when value is integral.
 */
export function formatPreservingPrecision(
  nextValue: number,
  originalNumericToken: string,
): string {
  const normalizedVal = Number(nextValue.toPrecision(12));
  const normalized = Object.is(normalizedVal, -0) ? 0 : normalizedVal;
  const hadDecimal =
    originalNumericToken.includes(".") && !/[eE]/.test(originalNumericToken);

  if (!hadDecimal && Number.isInteger(normalized)) {
    return String(normalized);
  }

  const raw = String(normalized);
  if (hadDecimal && !raw.includes(".") && !/[eE]/.test(raw)) {
    return `${raw}.0`;
  }
  return raw;
}

interface MatchedLiteral {
  readonly numberStartInSlice: number;
  readonly numberEndInSlice: number;
  readonly numericText: string;
  readonly unitSuffix: string;
}

const NUMERIC_LITERAL_PATTERN =
  /([+-]?\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?)([a-zA-Z_]+)?/;

/**
 * Finds the target numeric literal inside a span slice while ignoring
 * inline `// ...` comments and `@pmi::...` annotations.
 */
function locateNumericLiteralInSlice(
  slice: string,
  expectedUnit: string,
): MatchedLiteral {
  const commentIndex = slice.indexOf("//");
  const annotationIndex = slice.indexOf("@");
  let codeCutoff = slice.length;
  if (commentIndex !== -1) {
    codeCutoff = Math.min(codeCutoff, commentIndex);
  }
  if (annotationIndex !== -1) {
    codeCutoff = Math.min(codeCutoff, annotationIndex);
  }

  const codePart = slice.slice(0, codeCutoff);
  const equalsIndex = codePart.lastIndexOf("=");
  const searchStart = equalsIndex !== -1 ? equalsIndex + 1 : 0;
  const searchRegion = codePart.slice(searchStart);

  const match = NUMERIC_LITERAL_PATTERN.exec(searchRegion);
  if (!match || match.index === undefined) {
    throw new AstWritebackError(
      "LITERAL_NOT_FOUND",
      `Could not locate a numeric literal inside span text "${slice}".`,
    );
  }

  const numericText = match[1];
  const unitSuffix = match[2] ?? "";

  if (expectedUnit && unitSuffix && unitSuffix !== expectedUnit) {
    throw new AstWritebackError(
      "UNIT_MISMATCH",
      `Expected unit "${expectedUnit}" in source, but found "${unitSuffix}".`,
    );
  }

  const numberStartInSlice = searchStart + match.index;
  const numberEndInSlice = numberStartInSlice + numericText.length;

  return {
    numberStartInSlice,
    numberEndInSlice,
    numericText,
    unitSuffix,
  };
}

function buildLiteralReplacementEdit(
  source: string,
  span: SourceSpan,
  nextValue: number,
  expectedUnit: string,
): {
  readonly edit: TextEdit;
  readonly previousText: string;
  readonly replacementText: string;
  readonly preservedUnit: string;
} {
  const { startOffset, endOffset } = spanToOffsets(source, span);
  const slice = source.slice(startOffset, endOffset);
  const matched = locateNumericLiteralInSlice(slice, expectedUnit);

  const formattedNumber = formatPreservingPrecision(nextValue, matched.numericText);
  const numStart = startOffset + matched.numberStartInSlice;
  const numEnd = startOffset + matched.numberEndInSlice;

  // Replace ONLY the numeric token bytes so trailing unit suffix (`mm`, `deg`, `N`),
  // `@pmi::...` annotations, whitespace, and `// ...` comments remain 100% untouched.
  const edit: TextEdit = {
    startOffset: numStart,
    endOffset: numEnd,
    newText: formattedNumber,
    span,
  };

  return {
    edit,
    previousText: matched.numericText + matched.unitSuffix,
    replacementText: formattedNumber + matched.unitSuffix,
    preservedUnit: matched.unitSuffix || expectedUnit,
  };
}

/**
 * Tier 1: Direct literal / top-level parameter lossless in-place rewrite.
 */
export function writebackTier1(
  source: string,
  handle: EditHandle,
  nextValue: number,
): Tier1WritebackResult {
  if (handle.tier !== 1) {
    throw new AstWritebackError(
      "TIER_MISMATCH",
      `Handle "${handle.handle_id}" is Tier ${handle.tier}, expected Tier 1.`,
    );
  }
  assertFiniteValue(nextValue, handle.param_name);

  const { edit, previousText, replacementText, preservedUnit } =
    buildLiteralReplacementEdit(
      source,
      handle.ast_span,
      nextValue,
      normalizeUnit(handle.unit),
    );

  const updatedSource = applyTextEdits(source, [edit]);

  return {
    kind: "tier1",
    applied: true,
    handleId: handle.handle_id,
    paramName: handle.param_name,
    previousValue: handle.current_value,
    nextValue,
    unit: preservedUnit,
    previousText,
    replacementText,
    edit,
    updatedSource,
  };
}

function computeTier2Values(
  binding: Tier2AffineBinding,
  targetValue: number,
): { readonly nextOffset: number; readonly nextUpstream: number } {
  if (binding.operator === "+") {
    return {
      nextOffset: targetValue - binding.upstream_value,
      nextUpstream: targetValue - binding.offset_value,
    };
  }
  return {
    nextOffset: binding.upstream_value - targetValue,
    nextUpstream: targetValue + binding.offset_value,
  };
}

/**
 * Tier 2: Single-variable affine offset (`target = upstream +/- offset`).
 * Generates a dual-option patch set allowing the engineer to choose whether
 * to mutate the offset constant or the upstream parameter.
 */
export function planTier2Writeback(
  source: string,
  handle: EditHandle,
  targetValue: number,
): Tier2WritebackResult {
  if (handle.tier !== 2) {
    throw new AstWritebackError(
      "TIER_MISMATCH",
      `Handle "${handle.handle_id}" is Tier ${handle.tier}, expected Tier 2.`,
    );
  }
  if (!handle.tier2_binding) {
    throw new AstWritebackError(
      "MISSING_TIER2_BINDING",
      `Tier 2 handle "${handle.handle_id}" is missing tier2_binding metadata.`,
    );
  }
  assertFiniteValue(targetValue, handle.param_name);

  const binding = handle.tier2_binding;
  const { nextOffset, nextUpstream } = computeTier2Values(binding, targetValue);

  const offsetReplacement = buildLiteralReplacementEdit(
    source,
    binding.offset_span,
    nextOffset,
    binding.offset_unit,
  );
  const offsetOption: Tier2PatchOption = {
    choice: "offset",
    label: `修改偏移常量 (${formatPreservingPrecision(binding.offset_value, String(binding.offset_value))}${binding.offset_unit} -> ${offsetReplacement.replacementText})`,
    description: `保持上游参数 ${binding.upstream_param} (${binding.upstream_value}${binding.upstream_unit}) 不变，仅更新偏移常量。`,
    targetParam: `${handle.param_name} [offset]`,
    previousValue: binding.offset_value,
    nextValue: nextOffset,
    unit: offsetReplacement.preservedUnit,
    edit: offsetReplacement.edit,
    previewSource: applyTextEdits(source, [offsetReplacement.edit]),
  };

  const upstreamReplacement = buildLiteralReplacementEdit(
    source,
    binding.upstream_span,
    nextUpstream,
    binding.upstream_unit,
  );
  const upstreamOption: Tier2PatchOption = {
    choice: "upstream",
    label: `修改上游参数 ${binding.upstream_param} (${binding.upstream_value}${binding.upstream_unit} -> ${upstreamReplacement.replacementText})`,
    description: `保持偏移常量 ${binding.offset_value}${binding.offset_unit} 不变，联动更新上游参数 ${binding.upstream_param}。`,
    targetParam: binding.upstream_param,
    previousValue: binding.upstream_value,
    nextValue: nextUpstream,
    unit: upstreamReplacement.preservedUnit,
    edit: upstreamReplacement.edit,
    previewSource: applyTextEdits(source, [upstreamReplacement.edit]),
  };

  return {
    kind: "tier2",
    requiresChoice: true,
    handleId: handle.handle_id,
    paramName: handle.param_name,
    targetValue,
    unit: handle.unit,
    options: [offsetOption, upstreamOption],
  };
}

export function applyTier2Choice(
  source: string,
  handle: EditHandle,
  targetValue: number,
  choice: Tier2ChoiceKind,
): { readonly option: Tier2PatchOption; readonly updatedSource: string } {
  const plan = planTier2Writeback(source, handle, targetValue);
  const selected = plan.options.find((opt) => opt.choice === choice);
  if (!selected) {
    throw new AstWritebackError(
      "INVALID_TIER2_CHOICE",
      `Choice "${choice}" not available on handle "${handle.handle_id}".`,
    );
  }
  return {
    option: selected,
    updatedSource: selected.previewSource,
  };
}

function normalizeUnit(unit: string): string {
  return unit === "unitless" ? "" : unit;
}

function buildCoupledBindingFallback(
  source: string,
  handle: EditHandle,
): Tier3CoupledBinding {
  let exprText = `${handle.param_name} (coupled)`;
  try {
    const raw = extractSpanText(source, handle.ast_span).trim();
    if (raw) {
      exprText = raw;
    }
  } catch {
    // Keep fallback label if span cannot be sliced.
  }
  return {
    expression_text: exprText,
    dependencies: [],
    suggested_param_name: `${handle.param_name}_override`,
    statement_span: handle.ast_span,
    rhs_span: handle.ast_span,
  };
}

function getLineIndentationAndLineNumber(
  source: string,
  offset: number,
): { readonly lineStartOffset: number; readonly indent: string; readonly lineNumber: number; readonly lineEnding: "\r\n" | "\n" } {
  const index = buildLineIndex(source);
  let lineIdx = 0;
  for (let i = 0; i < index.lineStarts.length; i += 1) {
    if (index.lineStarts[i] <= offset) {
      lineIdx = i;
    } else {
      break;
    }
  }
  const lineStartOffset = index.lineStarts[lineIdx];
  let cursor = lineStartOffset;
  while (cursor < source.length && (source[cursor] === " " || source[cursor] === "\t")) {
    cursor += 1;
  }
  return {
    lineStartOffset,
    indent: source.slice(lineStartOffset, cursor),
    lineNumber: lineIdx + 1,
    lineEnding: index.lineEnding,
  };
}

interface Tier3InsertionContext {
  readonly declKeyword: "let" | "const";
  readonly lineStartOffset: number;
  readonly indent: string;
  readonly lineNumber: number;
  readonly lineEnding: "\r\n" | "\n";
}

function resolveTier3InsertionContext(
  source: string,
  targetOffset: number,
): Tier3InsertionContext {
  let parenDepth = 0;
  let bracketDepth = 0;
  let braceDepth = 0;
  let inString = false;
  let inLineComment = false;
  let inBlockComment = false;

  // Track active `part Name(` header if targetOffset lands inside its parameter list
  let activePartHeaderOffset: number | null = null;
  let activePartParenDepth: number | null = null;

  // Track the start offset of the current top-level statement inside the innermost `{ ... }` block
  const blockStmtStarts: number[] = [0];
  let atStmtBoundary = true;

  for (let i = 0; i < targetOffset && i < source.length; i += 1) {
    const ch = source[i];
    const next = i + 1 < source.length ? source[i + 1] : "";

    if (inLineComment) {
      if (ch === "\n") {
        inLineComment = false;
        if (parenDepth === 0 && bracketDepth === 0) {
          atStmtBoundary = true;
        }
      }
      continue;
    }

    if (inBlockComment) {
      if (ch === "*" && next === "/") {
        inBlockComment = false;
        i += 1;
      }
      continue;
    }

    if (inString) {
      if (ch === "\\") {
        i += 1;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }

    if (ch === "/" && next === "/") {
      inLineComment = true;
      i += 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      inBlockComment = true;
      i += 1;
      continue;
    }
    if (ch === '"') {
      inString = true;
      atStmtBoundary = false;
      continue;
    }

    if (ch === "\n") {
      if (parenDepth === 0 && bracketDepth === 0) {
        atStmtBoundary = true;
      }
      continue;
    }

    if (ch === " " || ch === "\t" || ch === "\r") {
      continue;
    }

    // Detect `part <Ident>(` at top-level (braceDepth === 0 && parenDepth === 0)
    if (
      braceDepth === 0 &&
      parenDepth === 0 &&
      ch === "p" &&
      source.slice(i, i + 4) === "part" &&
      (i === 0 || !/[a-zA-Z0-9_]/.test(source[i - 1])) &&
      /^\s+[A-Za-z_]\w*\s*\(/.test(source.slice(i + 4))
    ) {
      activePartHeaderOffset = i;
      activePartParenDepth = null;
    }

    if (atStmtBoundary && parenDepth === 0 && bracketDepth === 0 && ch !== "}") {
      blockStmtStarts[blockStmtStarts.length - 1] = i;
      atStmtBoundary = false;
    }

    if (ch === "(") {
      parenDepth += 1;
      if (activePartHeaderOffset !== null && activePartParenDepth === null && braceDepth === 0) {
        activePartParenDepth = parenDepth;
      }
    } else if (ch === ")") {
      if (activePartParenDepth !== null && parenDepth === activePartParenDepth) {
        activePartHeaderOffset = null;
        activePartParenDepth = null;
      }
      parenDepth = Math.max(0, parenDepth - 1);
    } else if (ch === "[") {
      bracketDepth += 1;
    } else if (ch === "]") {
      bracketDepth = Math.max(0, bracketDepth - 1);
    } else if (ch === "{") {
      braceDepth += 1;
      blockStmtStarts.push(i + 1);
      atStmtBoundary = true;
    } else if (ch === "}") {
      braceDepth = Math.max(0, braceDepth - 1);
      if (blockStmtStarts.length > 1) {
        blockStmtStarts.pop();
      }
      atStmtBoundary = true;
    }
  }

  // Case A: target is inside `part Name(...)` parameter list -> hoist top-level `const` before `part`
  if (activePartHeaderOffset !== null && activePartParenDepth !== null) {
    const info = getLineIndentationAndLineNumber(source, activePartHeaderOffset);
    return {
      declKeyword: "const",
      ...info,
    };
  }

  // Case B: inside `{ ... }` block (or top-level) -> insert `let` before the enclosing statement line
  const stmtAnchor = atStmtBoundary
    ? targetOffset
    : (blockStmtStarts[blockStmtStarts.length - 1] ?? targetOffset);
  const info = getLineIndentationAndLineNumber(source, Math.min(stmtAnchor, targetOffset));
  return {
    declKeyword: braceDepth === 0 ? "const" : "let",
    ...info,
  };
}

/**
 * Tier 3: Coupled expression guard + "Extract Parameter" refactoring generator.
 * Refuses blind overwrite of multi-variable / non-invertible expressions and emits
 * a lossless refactoring patch extracting an independent Tier 1 `let` or `const` parameter.
 */
export function planTier3ExtractRefactor(
  source: string,
  handle: EditHandle,
  requestedValue: number,
  customParamName?: string,
): Tier3WritebackResult {
  if (handle.tier !== 3) {
    throw new AstWritebackError(
      "TIER_MISMATCH",
      `Handle "${handle.handle_id}" is Tier ${handle.tier}, expected Tier 3.`,
    );
  }
  assertFiniteValue(requestedValue, handle.param_name);

  const binding =
    handle.tier3_binding ?? buildCoupledBindingFallback(source, handle);
  const extractedParamName = (customParamName?.trim() || binding.suggested_param_name).replace(
    /[^a-zA-Z0-9_]/g,
    "_",
  );

  const stmtOffsets = spanToOffsets(source, binding.statement_span);
  const rhsOffsets = spanToOffsets(source, binding.rhs_span);
  const { declKeyword, lineStartOffset, indent, lineNumber, lineEnding } =
    resolveTier3InsertionContext(source, stmtOffsets.startOffset);

  const effectiveUnit = normalizeUnit(handle.unit);
  const formattedVal = formatPreservingPrecision(
    requestedValue,
    String(handle.current_value).includes(".") ? String(handle.current_value) : `${handle.current_value}.0`,
  );
  const literalWithUnit = `${formattedVal}${effectiveUnit}`;

  // The audit text is a line comment; embedded newlines must not become KCL code.
  const auditExpression = binding.expression_text.replace(/\s+/g, " ").trim();
  const declarationLine = `${indent}${declKeyword} ${extractedParamName} = ${literalWithUnit} // extracted from: ${auditExpression}${lineEnding}`;

  const insertEdit: TextEdit = {
    startOffset: lineStartOffset,
    endOffset: lineStartOffset,
    newText: declarationLine,
    span: {
      start_line: lineNumber,
      start_col: 1,
      end_line: lineNumber,
      end_col: 1,
    },
  };

  const replaceRhsEdit: TextEdit = {
    startOffset: rhsOffsets.startOffset,
    endOffset: rhsOffsets.endOffset,
    newText: extractedParamName,
    span: binding.rhs_span,
  };

  const edits: readonly TextEdit[] = [insertEdit, replaceRhsEdit];
  const previewSource = applyTextEdits(source, edits);

  return {
    kind: "tier3",
    blocked: true,
    handleId: handle.handle_id,
    paramName: handle.param_name,
    requestedValue,
    reason: `参数 "${handle.param_name}" 绑定于多变量耦合表达式 (${binding.expression_text})，禁止盲目覆写；请先执行“提取独立顶层参数”重构。`,
    highlightedSpan: binding.statement_span,
    refactor: {
      extractedParamName,
      extractedValue: requestedValue,
      unit: handle.unit,
      originalExpression: binding.expression_text,
      dependencies: binding.dependencies,
      edits,
      previewSource,
    },
  };
}

export function applyTier3ExtractParameter(
  source: string,
  handle: EditHandle,
  requestedValue: number,
  customParamName?: string,
): { readonly refactor: Tier3ExtractRefactoring; readonly updatedSource: string } {
  const result = planTier3ExtractRefactor(source, handle, requestedValue, customParamName);
  return {
    refactor: result.refactor,
    updatedSource: result.refactor.previewSource,
  };
}

/**
 * Unified View-Update Controller dispatcher for Tier 1 / Tier 2 / Tier 3 handles.
 */
export function computeWritebackPlan(
  source: string,
  handle: EditHandle,
  targetValue: number,
  customExtractParamName?: string,
): WritebackPlan {
  switch (handle.tier) {
    case 1:
      return writebackTier1(source, handle, targetValue);
    case 2:
      return planTier2Writeback(source, handle, targetValue);
    case 3:
      return planTier3ExtractRefactor(source, handle, targetValue, customExtractParamName);
  }
}
