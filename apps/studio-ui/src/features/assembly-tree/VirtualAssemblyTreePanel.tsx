/**
 * One page of a host-owned assembly tree.
 * Search, paging, and the ROI digest all go through `virtualTreeModel`, which calls `CadSessionTransport`.
 */
import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  HLOD_LEVELS,
  type CadSessionTransport,
  type HlodLevel,
  type RoiInterfaceDigest,
  type VirtualTreeNode,
} from "../../sync/cadSessionTransport.ts";
import {
  computePageBounds,
  createInitialVirtualTreeState,
  formatAssemblyBreadcrumb,
  inspectSelectionRoiDigest,
  loadVirtualTreePage,
  searchVirtualTree,
  type VirtualTreeState,
} from "./virtualTreeModel.ts";

const SEARCH_DEBOUNCE_MS = 300;

export interface VirtualAssemblyTreePanelProps {
  readonly transport: CadSessionTransport;
  readonly sessionId: string;
  readonly pageSize?: number;
  readonly onSelectInstance?: (node: VirtualTreeNode) => void;
  readonly onDigestReady?: (digest: RoiInterfaceDigest) => void;
}

export function VirtualAssemblyTreePanel({
  transport,
  sessionId,
  pageSize,
  onSelectInstance,
  onDigestReady,
}: VirtualAssemblyTreePanelProps) {
  const searchId = useId();
  const [state, setState] = useState(() => createInitialVirtualTreeState(sessionId, pageSize));
  const [draft, setDraft] = useState("");
  const stateRef = useRef(state);
  const transportRef = useRef(transport);
  const requestGen = useRef(0);
  const draftGen = useRef(0);
  const searchTimer = useRef<number | null>(null);
  stateRef.current = state;
  transportRef.current = transport;

  const bounds = useMemo(
    () => computePageBounds(state.offset, state.limit, state.totalCount),
    [state.offset, state.limit, state.totalCount],
  );
  const selected = state.nodes.find((node) => node.instance_id === state.selectedInstanceId) ?? null;

  const commit = (next: VirtualTreeState) => {
    stateRef.current = next;
    setState(next);
  };

  // Page loads must not wipe a tier click or a row click that happened while the request was in flight.
  const commitTree = (next: VirtualTreeState) => {
    const latest = stateRef.current;
    commit({
      ...next,
      hlodLevel: latest.hlodLevel,
      selectedInstanceId: latest.selectedInstanceId,
      latestDigest: latest.latestDigest,
    });
  };

  useEffect(() => {
    const keptTier = stateRef.current.hlodLevel;
    draftGen.current += 1;
    if (searchTimer.current !== null) {
      window.clearTimeout(searchTimer.current);
      searchTimer.current = null;
    }
    const initial = createInitialVirtualTreeState(sessionId, pageSize);
    const gen = requestGen.current + 1;
    requestGen.current = gen;
    commit({ ...initial, hlodLevel: keptTier, loading: true });
    setDraft("");
    void loadVirtualTreePage(transport, initial, 0).then((next) => {
      if (requestGen.current !== gen) return;
      commitTree({ ...next, hlodLevel: keptTier });
    });
    return () => {
      requestGen.current += 1;
    };
  }, [transport, sessionId, pageSize]);

  useEffect(() => {
    const myDraft = draftGen.current + 1;
    draftGen.current = myDraft;
    if (draft.trim() === stateRef.current.searchQuery) return;
    searchTimer.current = window.setTimeout(() => {
      searchTimer.current = null;
      if (draftGen.current !== myDraft) return;
      const current = stateRef.current;
      if (draft.trim() === current.searchQuery) return;
      const gen = requestGen.current + 1;
      requestGen.current = gen;
      commit({ ...current, loading: true, error: null });
      void searchVirtualTree(transportRef.current, current, draft).then((next) => {
        if (requestGen.current !== gen || draftGen.current !== myDraft) return;
        commitTree(next);
      });
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      if (searchTimer.current !== null) {
        window.clearTimeout(searchTimer.current);
        searchTimer.current = null;
      }
    };
  }, [draft]);

  const showOffset = (offset: number) => {
    const current = stateRef.current;
    const gen = requestGen.current + 1;
    requestGen.current = gen;
    commit({ ...current, loading: true, error: null });
    void loadVirtualTreePage(transportRef.current, current, offset).then((next) => {
      if (requestGen.current !== gen) return;
      commitTree(next);
    });
  };

  const selectInstance = (node: VirtualTreeNode) => {
    const current = stateRef.current;
    commit({
      ...current,
      selectedInstanceId: node.instance_id,
      latestDigest: current.selectedInstanceId === node.instance_id ? current.latestDigest : null,
    });
    onSelectInstance?.(node);
  };

  const inspectSelection = () => {
    const current = stateRef.current;
    if (current.selectedInstanceId === null) return;
    const gen = requestGen.current + 1;
    requestGen.current = gen;
    commit({ ...current, loading: true, error: null });
    void inspectSelectionRoiDigest(transportRef.current, current, current.selectedInstanceId).then((next) => {
      if (requestGen.current !== gen) return;
      commit({ ...next, hlodLevel: stateRef.current.hlodLevel });
      if (next.error === null && next.latestDigest) onDigestReady?.(next.latestDigest);
    });
  };

  const pageLabel = bounds.pageCount === 0
    ? "第 0 / 0 页"
    : `第 ${bounds.pageIndex + 1} / ${bounds.pageCount} 页`;
  const countLabel = state.searchQuery
    ? `${formatCount(state.totalCount)} 项匹配`
    : `${formatCount(state.totalCount)} 个实例`;

  return (
    <section className="panel-stack virtual-assembly-tree" aria-label="虚拟装配树" aria-busy={state.loading}>
      <div className="panel-card">
        <header className="panel-card-header">
          <h2 className="panel-card-title">装配层级 (HLOD)</h2>
          <div role="group" aria-label="HLOD 精度层级" style={tierStyle}>
            {HLOD_LEVELS.map((level) => (
              <button
                key={level}
                type="button"
                className={`mini-badge${state.hlodLevel === level ? " is-ok" : ""}`}
                aria-pressed={state.hlodLevel === level}
                onClick={() => commit({ ...stateRef.current, hlodLevel: level })}
                style={tierButtonStyle}
              >
                {formatHlodTier(level)}
              </button>
            ))}
          </div>
          <span className="mini-badge" data-total-count={state.totalCount}>{countLabel}</span>
        </header>

        <form
          style={searchFormStyle}
          onSubmit={(event) => {
            event.preventDefault();
            if (searchTimer.current !== null) {
              window.clearTimeout(searchTimer.current);
              searchTimer.current = null;
            }
            const myDraft = draftGen.current + 1;
            draftGen.current = myDraft;
            const current = stateRef.current;
            const gen = requestGen.current + 1;
            requestGen.current = gen;
            commit({ ...current, loading: true, error: null });
            void searchVirtualTree(transportRef.current, current, draft).then((next) => {
              if (requestGen.current !== gen || draftGen.current !== myDraft) return;
              commitTree(next);
            });
          }}
        >
          <label htmlFor={searchId} style={labelStyle}>搜索装配实例</label>
          <input
            id={searchId}
            className="text-input"
            type="search"
            value={draft}
            placeholder="输入主机装配树中的实例名或件号…"
            onChange={(event) => setDraft(event.target.value)}
          />
        </form>

        {state.error ? <p role="alert" style={errorStyle}>{state.error}</p> : null}
        {state.loading ? <p style={hintStyle}>正在加载分页…</p> : null}

        {state.nodes.length === 0 && !state.loading ? (
          <p style={emptyStyle}>{state.searchQuery ? "没有匹配的装配实例。" : "暂无装配实例。"}</p>
        ) : (
          <ol aria-label="装配实例列表" style={listStyle}>
            {state.nodes.map((node, index) => {
              const isSelected = node.instance_id === state.selectedInstanceId;
              return (
                <li key={`${node.instance_id}-${index}`} data-instance-id={node.instance_id}>
                  <button
                    type="button"
                    aria-current={isSelected ? "true" : undefined}
                    data-lightweight={node.frozen_lightweight ? "true" : "false"}
                    onClick={() => selectInstance(node)}
                    style={{
                      ...rowStyle,
                      background: isSelected ? "var(--bg-active)" : "transparent",
                      borderColor: isSelected ? "var(--accent-primary)" : "var(--border-subtle)",
                    }}
                  >
                    <span style={nameStyle}>{node.name.trim() || formatAssemblyBreadcrumb(node)}</span>
                    <span style={opStyle}>{node.part_number || "—"}</span>
                    {node.frozen_lightweight ? <span className="mini-badge is-ok">轻量化</span> : null}
                    <span style={opStyle}>{formatBboxSize(node)}</span>
                    {node.parent_id !== null ? <span style={crumbStyle}>{formatAssemblyBreadcrumb(node)}</span> : null}
                  </button>
                </li>
              );
            })}
          </ol>
        )}

        <div style={pagerStyle}>
          <button
            type="button"
            className="tool-btn is-compact"
            disabled={!bounds.hasPrev || state.loading}
            onClick={() => showOffset(Math.max(0, state.offset - state.limit))}
          >
            上一页
          </button>
          <span style={hintStyle}>{pageLabel}</span>
          <button
            type="button"
            className="tool-btn is-compact"
            disabled={!bounds.hasNext || state.loading}
            onClick={() => showOffset(state.offset + state.limit)}
          >
            下一页
          </button>
          <span style={spacerStyle} />
          <button
            type="button"
            className="tool-btn is-compact"
            disabled={selected === null || state.loading}
            onClick={inspectSelection}
          >
            检查 ROI 接口摘要
          </button>
        </div>

        {state.latestDigest ? (
          <div aria-label="ROI 接口摘要" style={digestStyle}>
            <span className="status-pill is-ok">
              {formatCount(state.latestDigest.component_count)} 个组件
            </span>
            <span style={opStyle}>{compactSummary(state.latestDigest.summary_json)}</span>
          </div>
        ) : null}
      </div>
    </section>
  );
}

function formatHlodTier(level: HlodLevel): string {
  switch (level) {
    case "hlod0":
      return "HLOD0";
    case "hlod1":
      return "HLOD1";
    case "hlod2":
      return "HLOD2";
  }
}

function formatBboxSize(node: VirtualTreeNode): string {
  const size = [0, 1, 2].map((axis) => formatMillimetres(node.bbox_max_mm[axis] - node.bbox_min_mm[axis]));
  return `${size[0]} × ${size[1]} × ${size[2]} mm`;
}

function formatMillimetres(value: number): string {
  if (!Number.isFinite(value)) return "—";
  const rounded = Math.round(value * 10) / 10;
  if (Object.is(rounded, -0)) return "0";
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

function formatCount(count: number): string {
  if (!Number.isFinite(count)) return "0";
  return new Intl.NumberFormat("en-US").format(count);
}

function compactSummary(summary: string): string {
  const line = summary.replace(/\s+/g, " ").trim();
  if (!line) return "No digest summary";
  if (line.length <= 96) return line;
  return `${line.slice(0, 93)}…`;
}

const tierStyle = { display: "inline-flex", gap: 4 } as const;
const tierButtonStyle = { border: 0, cursor: "pointer", font: "inherit" } as const;
const searchFormStyle = { display: "flex", flexDirection: "column", gap: 4 } as const;
const labelStyle = { fontSize: 11, color: "var(--text-muted)" } as const;
const listStyle = {
  listStyle: "none",
  margin: 0,
  padding: 0,
  display: "flex",
  flexDirection: "column",
  gap: 6,
  maxHeight: 360,
  overflow: "auto",
} as const;
const rowStyle = {
  width: "100%",
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: 6,
  textAlign: "left",
  border: "1px solid var(--border-subtle)",
  borderRadius: "var(--radius-sm)",
  padding: "6px 8px",
  color: "var(--text-primary)",
  cursor: "pointer",
} as const;
const nameStyle = { fontWeight: 600, color: "var(--text-primary)" } as const;
const opStyle = { fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)" } as const;
const crumbStyle = { flexBasis: "100%", fontSize: 11, color: "var(--text-secondary)" } as const;
const pagerStyle = { display: "flex", flexWrap: "wrap", alignItems: "center", gap: 6 } as const;
const spacerStyle = { flex: "1 1 8px" } as const;
const hintStyle = { margin: 0, color: "var(--text-secondary)", fontSize: 12 } as const;
const emptyStyle = { margin: 0, color: "var(--text-muted)", fontSize: 12 } as const;
const errorStyle = { margin: 0, color: "var(--status-error)", fontSize: 12 } as const;
const digestStyle = { display: "flex", flexWrap: "wrap", alignItems: "center", gap: 8 } as const;
