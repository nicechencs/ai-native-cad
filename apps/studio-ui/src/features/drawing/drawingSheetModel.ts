/**
 * Camera, filters, and health for one associative drawing sheet.
 *
 * The host owns the sheet SVG and the dimension rows. This module only
 * decides what the preview shows. It imports neither React nor the DOM,
 * so Node can test every transition directly.
 *
 * Status names follow `DimensionStatus` in `cad-render`:
 * `resolved` is `Resolved`, `dangling` is `Dangling`.
 * `ambiguous` is the dangling case whose anchor matched more than one face or edge.
 */

/** Projected view on a drawing sheet. `section_aa` is the A-A section. */
export type DrawingViewKind = "front" | "top" | "right" | "iso" | "section_aa";

/** Whether a callout is still tied to the current 3D model. */
export type DimensionBindingStatus = "resolved" | "dangling" | "ambiguous";

/** One dimension or GD&T callout after the host has measured it. */
export interface DrawingDimensionItem {
  readonly id: string;
  readonly label: string;
  readonly nominalMm: number;
  /** Tolerance or GD&T frame text, for example `±0.05` or `⌖ ⌀0.02`. */
  readonly toleranceText: string;
  readonly status: DimensionBindingStatus;
  readonly topoRefId: string;
  readonly view: DrawingViewKind;
}

/** Sheet payload the preview can render without calling the kernel. */
export interface AssociativeDrawingSheetData {
  readonly sheetId: string;
  readonly title: string;
  /** Human scale label, for example `1:2`. */
  readonly scaleLabel: string;
  /** Vector sheet markup from the associative drawing renderer. */
  readonly svgMarkup: string;
  readonly views: readonly DrawingViewKind[];
  readonly dimensions: readonly DrawingDimensionItem[];
}

/** Camera and inspector filters for one open sheet. */
export interface DrawingViewerState {
  readonly activeView: DrawingViewKind | "all";
  readonly zoom: number;
  readonly panX: number;
  readonly panY: number;
  readonly selectedDimensionId: string | null;
  readonly showOnlyDangling: boolean;
}

/** Smallest scale the sheet camera will apply. */
export const MIN_DRAWING_ZOOM = 0.25;

/** Largest scale the sheet camera will apply. */
export const MAX_DRAWING_ZOOM = 8;

const SCRIPT_NAME = "script";

/**
 * Open the whole sheet at 1:1.
 * Dangling callouts stay in the list until the user turns the filter on.
 * `sheet` is part of the call so a new sheet can rebuild this state.
 */
export function createInitialDrawingViewerState(sheet: AssociativeDrawingSheetData): DrawingViewerState {
  // An empty sheet and a sheet with views both start on All.
  // View tabs narrow the dimension list afterwards. They do not crop the SVG.
  const activeView: DrawingViewerState["activeView"] = sheet.views.length === 0 ? "all" : "all";
  return {
    activeView,
    zoom: 1,
    panX: 0,
    panY: 0,
    selectedDimensionId: null,
    showOnlyDangling: false,
  };
}

/**
 * Multiply the current zoom by `factor` and keep the result inside `[0.25, 8]`.
 * A factor that is not a positive finite number leaves the camera unchanged.
 */
export function zoomDrawingViewer(state: DrawingViewerState, factor: number): DrawingViewerState {
  if (!Number.isFinite(factor) || factor <= 0) return state;
  const zoom = clampDrawingZoom(state.zoom * factor);
  if (zoom === state.zoom) return state;
  return { ...state, zoom };
}

/** Move the sheet by a screen-pixel delta. A non-finite axis is ignored. */
export function panDrawingViewer(state: DrawingViewerState, dx: number, dy: number): DrawingViewerState {
  const panX = Number.isFinite(dx) ? state.panX + dx : state.panX;
  const panY = Number.isFinite(dy) ? state.panY + dy : state.panY;
  if (panX === state.panX && panY === state.panY) return state;
  return { ...state, panX, panY };
}

/** Return the camera to 1:1 with no pan. View filters and the selection stay. */
export function resetDrawingView(state: DrawingViewerState): DrawingViewerState {
  if (state.zoom === 1 && state.panX === 0 && state.panY === 0) return state;
  return { ...state, zoom: 1, panX: 0, panY: 0 };
}

/** Count resolved, dangling, and ambiguous callouts. */
export function summarizeDimensionHealth(dimensions: readonly DrawingDimensionItem[]): {
  total: number;
  resolved: number;
  dangling: number;
  ambiguous: number;
  allHealthy: boolean;
} {
  let resolved = 0;
  let dangling = 0;
  let ambiguous = 0;
  for (const dimension of dimensions) {
    switch (dimension.status) {
      case "resolved":
        resolved += 1;
        break;
      case "dangling":
        dangling += 1;
        break;
      case "ambiguous":
        ambiguous += 1;
        break;
      default: {
        const unexpected: never = dimension.status;
        void unexpected;
        break;
      }
    }
  }
  return {
    total: dimensions.length,
    resolved,
    dangling,
    ambiguous,
    allHealthy: dangling === 0 && ambiguous === 0 && resolved === dimensions.length,
  };
}

/**
 * Keep callouts for the active view.
 * `showOnlyDangling` keeps rows whose status is `dangling` and hides resolved and ambiguous rows.
 * Input order is preserved.
 */
export function filterDrawingDimensions(
  dimensions: readonly DrawingDimensionItem[],
  activeView: DrawingViewKind | "all",
  showOnlyDangling: boolean,
): readonly DrawingDimensionItem[] {
  return dimensions.filter((dimension) => {
    if (activeView !== "all" && dimension.view !== activeView) return false;
    if (showOnlyDangling && dimension.status !== "dangling") return false;
    return true;
  });
}

/**
 * Remove active content from a drawing SVG.
 * `<script>` elements and `on…` event attributes are dropped.
 * `javascript:` links are cleared. `orient` on markers is kept, and text nodes are not rewritten.
 */
export function sanitizeDrawingSvg(rawSvg: string): string {
  if (rawSvg === "") return "";
  return stripDangerousAttributes(stripScriptElements(rawSvg));
}

function clampDrawingZoom(value: number): number {
  if (!Number.isFinite(value)) return 1;
  if (value < MIN_DRAWING_ZOOM) return MIN_DRAWING_ZOOM;
  if (value > MAX_DRAWING_ZOOM) return MAX_DRAWING_ZOOM;
  return value;
}

function stripScriptElements(source: string): string {
  const lower = source.toLowerCase();
  let cursor = 0;
  let out = "";
  while (cursor < source.length) {
    const open = lower.indexOf("<script", cursor);
    if (open === -1) {
      out += source.slice(cursor);
      break;
    }
    const boundary = lower[open + 7];
    const isScriptTag = boundary === undefined
      || boundary === ">"
      || boundary === "/"
      || /\s/.test(boundary);
    if (!isScriptTag) {
      out += source.slice(cursor, open + 7);
      cursor = open + 7;
      continue;
    }
    out += source.slice(cursor, open);
    const tagEnd = findTagEnd(source, open);
    if (tagEnd === -1) break;
    if (source[tagEnd - 1] === "/") {
      cursor = tagEnd + 1;
      continue;
    }
    const close = findClosingTag(lower, SCRIPT_NAME, tagEnd + 1);
    if (close === -1) break;
    const closeEnd = findTagEnd(source, close);
    cursor = closeEnd === -1 ? source.length : closeEnd + 1;
  }
  return out;
}

function stripDangerousAttributes(source: string): string {
  let cursor = 0;
  let out = "";
  while (cursor < source.length) {
    const open = source.indexOf("<", cursor);
    if (open === -1) {
      out += source.slice(cursor);
      break;
    }
    out += source.slice(cursor, open);
    const end = findTagEnd(source, open);
    if (end === -1) {
      out += source.slice(open);
      break;
    }
    const tag = source.slice(open, end + 1);
    const keepWholeTag = tag.startsWith("</") || tag.startsWith("<!") || tag.startsWith("<?");
    out += keepWholeTag ? tag : cleanStartTag(tag);
    cursor = end + 1;
  }
  return out;
}

function cleanStartTag(tag: string): string {
  const withoutEvents = tag.replace(
    /\s+on(?!ient\b)[a-z][\w:-]*\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+)/gi,
    "",
  );
  return withoutEvents.replace(
    /(\s(?:xlink:href|href)\s*=\s*)(?:"\s*javascript:[^"]*"|'\s*javascript:[^']*'|javascript:[^\s>]*)/gi,
    '$1""',
  );
}

/** Index of the closing tag whose name matches, or -1. The source must already be lower-case. */
function findClosingTag(lowerSource: string, name: string, from: number): number {
  const needle = `</${name}`;
  let cursor = from;
  while (cursor < lowerSource.length) {
    const close = lowerSource.indexOf(needle, cursor);
    if (close === -1) return -1;
    const boundary = lowerSource[close + needle.length];
    if (boundary === undefined || boundary === ">" || boundary === "/" || /\s/.test(boundary)) {
      return close;
    }
    cursor = close + needle.length;
  }
  return -1;
}

/** Index of the `>` that ends the tag starting at `open`, respecting quotes. */
function findTagEnd(source: string, open: number): number {
  let quote: "\"" | "'" | null = null;
  for (let index = open + 1; index < source.length; index += 1) {
    const char = source[index];
    if (quote !== null) {
      if (char === quote) quote = null;
      continue;
    }
    if (char === "\"" || char === "'") {
      quote = char;
      continue;
    }
    if (char === ">") return index;
  }
  return -1;
}
