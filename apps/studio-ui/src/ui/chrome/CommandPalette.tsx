import { useEffect, useMemo, useRef, useState } from "react";
import {
  isCommandEnabled,
  searchCommands,
  type CommandContext,
  type CommandDefinition,
} from "../../core/commands/commandRegistry.ts";
import type { CanonicalGraph } from "../../types/graphIr.ts";
import { CadIcon, type CadIconName } from "../icons/CadIcons.tsx";

export interface CommandPaletteProps {
  readonly open: boolean;
  readonly commands: readonly CommandDefinition[];
  readonly context: CommandContext;
  readonly graphIr?: CanonicalGraph | null;
  readonly onSelectNode?: (nodeId: string) => void;
  readonly onClose: () => void;
  readonly onExecute: (commandId: string) => void;
}

type FinderEntry =
  | { readonly kind: "command"; readonly cmd: CommandDefinition }
  | { readonly kind: "node"; readonly nodeId: string; readonly name: string; readonly nodeKind: string }
  | { readonly kind: "param"; readonly name: string; readonly detail: string };

/**
 * NX-inspired Command Finder: searches commands (with Ribbon breadcrumb path),
 * parametric feature nodes, and model parameters from a single Ctrl+K popup.
 */
export function CommandPalette({
  open,
  commands,
  context,
  graphIr,
  onSelectNode,
  onClose,
  onExecute,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const entries = useMemo<readonly FinderEntry[]>(() => {
    const cmdEntries: FinderEntry[] = searchCommands(commands, query, context).map((cmd) => ({
      kind: "command",
      cmd,
    }));

    const trimmed = query.trim().toLowerCase();
    if (!trimmed || !graphIr) return cmdEntries;

    const nodeEntries: FinderEntry[] = Object.values(graphIr.nodes)
      .filter((node) => `${node.name} ${node.id} ${node.kind.kind}`.toLowerCase().includes(trimmed))
      .slice(0, 6)
      .map((node) => ({
        kind: "node",
        nodeId: node.id,
        name: node.name || node.id,
        nodeKind: node.kind.kind,
      }));

    const paramEntries: FinderEntry[] = Object.values(graphIr.parameters)
      .filter((param) => `${param.name} ${param.formula ?? ""}`.toLowerCase().includes(trimmed))
      .slice(0, 5)
      .map((param) => ({
        kind: "param",
        name: param.name,
        detail: param.formula ? `= ${param.formula}` : param.ir_type,
      }));

    return [...cmdEntries, ...nodeEntries, ...paramEntries];
  }, [commands, query, context, graphIr]);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActiveIndex(0);
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, [open]);

  useEffect(() => {
    setActiveIndex(0);
  }, [query]);

  if (!open) return null;

  const runEntry = (entry: FinderEntry | undefined) => {
    if (!entry) return;
    if (entry.kind === "command") {
      if (!isCommandEnabled(entry.cmd, context)) return;
      onClose();
      onExecute(entry.cmd.id);
      return;
    }
    if (entry.kind === "node") {
      onClose();
      onSelectNode?.(entry.nodeId);
      onExecute("panel.openModelTree");
      return;
    }
    onClose();
    onExecute("panel.openProperties");
  };

  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="全局命令面板 (Command Finder)"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="command-palette">
        <div className="palette-input-row">
          <CadIcon name="search" size={16} />
          <input
            ref={inputRef}
            type="text"
            className="palette-input"
            placeholder="Command Finder：搜索命令、特征节点或参数（如 拉伸 / 孔 / fillet / iso / flange_od）…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                onClose();
              } else if (event.key === "ArrowDown") {
                event.preventDefault();
                setActiveIndex((prev) => (entries.length ? (prev + 1) % entries.length : 0));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActiveIndex((prev) =>
                  entries.length ? (prev - 1 + entries.length) % entries.length : 0,
                );
              } else if (event.key === "Enter") {
                event.preventDefault();
                runEntry(entries[activeIndex]);
              }
            }}
          />
          <kbd className="shortcut-kbd">Esc</kbd>
        </div>

        <div className="palette-list" role="listbox" aria-label="匹配命令列表">
          {entries.length === 0 ? (
            <div className="palette-empty">未找到匹配命令或特征，可按 / 在底部命令行输入 /ai 指令。</div>
          ) : (
            entries.map((entry, index) => {
              const selected = index === activeIndex;
              if (entry.kind === "node") {
                return (
                  <button
                    key={`node:${entry.nodeId}`}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    className={`palette-item${selected ? " is-selected" : ""}`}
                    onMouseEnter={() => setActiveIndex(index)}
                    onClick={() => runEntry(entry)}
                  >
                    <span className="palette-item-icon">
                      <CadIcon name="cube" size={15} />
                    </span>
                    <span className="palette-item-main">
                      <span className="palette-item-title">{entry.name}</span>
                      <span className="palette-item-desc">模型树特征节点 · {entry.nodeKind} ({entry.nodeId})</span>
                    </span>
                    <span className="palette-item-meta">
                      <span className="palette-category-tag">特征</span>
                    </span>
                  </button>
                );
              }
              if (entry.kind === "param") {
                return (
                  <button
                    key={`param:${entry.name}`}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    className={`palette-item${selected ? " is-selected" : ""}`}
                    onMouseEnter={() => setActiveIndex(index)}
                    onClick={() => runEntry(entry)}
                  >
                    <span className="palette-item-icon">
                      <CadIcon name="properties" size={15} />
                    </span>
                    <span className="palette-item-main">
                      <span className="palette-item-title">{entry.name}</span>
                      <span className="palette-item-desc">工程参数 · {entry.detail}</span>
                    </span>
                    <span className="palette-item-meta">
                      <span className="palette-category-tag">参数 fx</span>
                    </span>
                  </button>
                );
              }
              const { cmd } = entry;
              const enabled = isCommandEnabled(cmd, context);
              return (
                <button
                  key={cmd.id}
                  type="button"
                  role="option"
                  aria-selected={selected}
                  disabled={!enabled}
                  className={`palette-item${selected ? " is-selected" : ""}`}
                  onMouseEnter={() => setActiveIndex(index)}
                  onClick={() => runEntry(entry)}
                >
                  <span className="palette-item-icon">
                    <CadIcon name={(cmd.icon as CadIconName) ?? "command"} size={15} />
                  </span>
                  <span className="palette-item-main">
                    <span className="palette-item-title">{cmd.title}</span>
                    {(cmd.ribbonPath || cmd.description) && (
                      <span className="palette-item-desc">
                        {cmd.ribbonPath ? `${cmd.ribbonPath}${cmd.description ? ` · ${cmd.description}` : ""}` : cmd.description}
                      </span>
                    )}
                  </span>
                  <span className="palette-item-meta">
                    <span className="palette-category-tag">{cmd.category}</span>
                    {cmd.shortcut && <kbd className="shortcut-kbd">{cmd.shortcut}</kbd>}
                  </span>
                </button>
              );
            })
          )}
        </div>

        <div className="palette-footer">
          <span>↑↓ 选择</span>
          <span>Enter 执行 / 定位</span>
          <span>支持搜索 Ribbon 路径、特征节点与参数</span>
          <span>Esc 关闭</span>
        </div>
      </div>
    </div>
  );
}
