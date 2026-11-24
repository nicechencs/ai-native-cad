import { useEffect, useRef, useState } from "react";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";

export interface InlinePointPromptPopoverProps {
  readonly open: boolean;
  readonly selectedTopoRefs: readonly string[];
  readonly selectedNodeIds: readonly string[];
  readonly busy?: boolean;
  readonly onSubmit: (prompt: string) => void;
  readonly onOpenFullCopilot: () => void;
  readonly onClose: () => void;
}

/**
 * Viewport-native Inline Point-and-Prompt bubble (Alt+A / MiniToolbar / Radial SW).
 * Attaches structured TopoRef + NodeId context without forcing the full right AI drawer open.
 */
export function InlinePointPromptPopover({
  open,
  selectedTopoRefs,
  selectedNodeIds,
  busy = false,
  onSubmit,
  onOpenFullCopilot,
  onClose,
}: InlinePointPromptPopoverProps) {
  const [prompt, setPrompt] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus());
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open, onClose]);

  if (!open) return null;

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    const trimmed = prompt.trim();
    if (!trimmed || busy) return;
    onSubmit(trimmed);
    setPrompt("");
    onClose();
  };

  return (
    <div
      className="inline-ai-popover"
      role="dialog"
      aria-label="视口内联 AI Point-and-Prompt"
      onPointerDown={(event) => event.stopPropagation()}
    >
      <form className="inline-ai-form" onSubmit={handleSubmit}>
        <span className="inline-ai-icon">
          <CadIcon name="sparkles" size={14} />
        </span>
        <div className="inline-ai-body">
          <div className="inline-ai-context">
            {selectedTopoRefs.length > 0 ? (
              <span className="mini-badge">已挂载 TopoRef: {selectedTopoRefs.slice(0, 3).join(", ")}{selectedTopoRefs.length > 3 ? ` +${selectedTopoRefs.length - 3}` : ""}</span>
            ) : selectedNodeIds.length > 0 ? (
              <span className="mini-badge">已挂载特征: {selectedNodeIds.join(", ")}</span>
            ) : (
              <span className="muted-text">当前整个零件上下文（点选面/边可自动挂载局部 TopoRef）</span>
            )}
          </div>
          <input
            ref={inputRef}
            type="text"
            className="inline-ai-input"
            placeholder="描述工程修改意图，例如：在此面阵列 4 个 M5 沉头孔 / 外缘倒圆角 R3…"
            value={prompt}
            disabled={busy}
            onChange={(event) => setPrompt(event.target.value)}
          />
        </div>
        <button type="submit" className="fd-btn is-confirm" disabled={busy || !prompt.trim()}>
          {busy ? "执行中…" : "发送"}
        </button>
        <button
          type="button"
          className="icon-ghost-btn"
          title="展开完整 AI 副驾面板"
          aria-label="展开完整 AI 副驾面板"
          onClick={() => {
            onClose();
            onOpenFullCopilot();
          }}
        >
          <CadIcon name="sidebarRight" size={14} />
        </button>
        <button
          type="button"
          className="icon-ghost-btn"
          title="关闭 (Esc)"
          aria-label="关闭内联 AI"
          onClick={onClose}
        >
          <CadIcon name="close" size={13} />
        </button>
      </form>
    </div>
  );
}
