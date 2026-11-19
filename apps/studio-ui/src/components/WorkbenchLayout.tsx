import { useEffect, useMemo, useState } from "react";
import { createDesktopIpcClient, type EngineStatus } from "../../../desktop/src/ipcBridge.ts";
import {
  DEMO_DESIGN_CONTRACT,
  INITIAL_DEMO_KCL,
  MATERIAL_PRESETS,
  evaluateDemoKclModel,
} from "../data/demoWorkbenchState.ts";
import type {
  MaterialPreset,
  SelectionTarget,
  SourceSpan,
} from "../types/scene.ts";
import {
  generateFallbackDrawingSvg,
  generateFallbackDxfFromMesh,
  generateFallbackKclFromPrompt,
} from "../utils/studioHelpers.ts";
import { CenterViewport } from "./CenterViewport.tsx";
import { LeftPanel } from "./LeftPanel.tsx";
import { RightPanel } from "./RightPanel.tsx";

export function WorkbenchLayout() {
  const [kclSource, setKclSource] = useState<string>(INITIAL_DEMO_KCL);
  const [selectedMaterial, setSelectedMaterial] = useState<MaterialPreset>(
    MATERIAL_PRESETS[0],
  );
  const [selectedTarget, setSelectedTarget] = useState<SelectionTarget>(null);
  const [selectedComponentId, setSelectedComponentId] = useState<string | null>(null);
  const [highlightedSpan, setHighlightedSpan] = useState<SourceSpan | null>(null);
  const [highlightLabel, setHighlightLabel] = useState<string | null>(null);
  const [syncNotice, setSyncNotice] = useState<string>(
    "Code-as-IR 双向同步就绪 (< 16ms 视口预览 / < 50ms 增量 AST 写回)",
  );
  const [engineStatus, setEngineStatus] = useState<EngineStatus | null>(null);

  useEffect(() => {
    let mounted = true;
    void createDesktopIpcClient()
      .getEngineStatus()
      .then((status) => {
        if (mounted) {
          setEngineStatus(status);
        }
      });
    return () => {
      mounted = false;
    };
  }, []);

  const evaluated = useMemo(() => evaluateDemoKclModel(kclSource), [kclSource]);
  const dxfContent = useMemo(
    () => generateFallbackDxfFromMesh(evaluated.sceneMesh),
    [evaluated.sceneMesh],
  );
  const drawingSvg = useMemo(
    () =>
      generateFallbackDrawingSvg(
        evaluated.sceneMesh,
        evaluated.diagnostics.metrics,
        {},
      ),
    [evaluated],
  );

  const handleCommitKclSource = (nextSource: string, summary: string) => {
    setKclSource(nextSource);
    setSyncNotice(summary);
  };

  const handleFocusAstSpan = (span: SourceSpan, label: string) => {
    setHighlightedSpan(span);
    setHighlightLabel(label);
  };

  const handleAiGenerate = (
    prompt: string,
    material: string,
    manufacturingMethod: string,
  ) => {
    const generated = generateFallbackKclFromPrompt(
      prompt,
      material,
      manufacturingMethod,
    );
    setKclSource(generated);
    setSelectedTarget(null);
    setSelectedComponentId(null);
    setHighlightedSpan(null);
    setHighlightLabel(null);
    const preset = MATERIAL_PRESETS.find((item) => item.id === material);
    if (preset) setSelectedMaterial(preset);
    setSyncNotice(`已生成模板源码 (${material} / ${manufacturingMethod})；真实编译请使用完整工作台`);
  };

  const handleResetDemo = () => {
    setKclSource(INITIAL_DEMO_KCL);
    setSelectedTarget(null);
    setSelectedComponentId(null);
    setHighlightedSpan(null);
    setHighlightLabel(null);
    setSyncNotice("已重置为 LightweightBearingMount 演示基准状态");
  };

  return (
    <div className="workbench-shell">
      <header className="topbar">
        <div className="topbar-left">
          <span className="brand-dot" />
          <strong>AI-Native CAD Studio</strong>
          <span className="topbar-sep">/</span>
          <span className="topbar-part">{evaluated.sceneMesh.part_name}.kcl</span>
        </div>
        <div className="topbar-center">
          <span className="sync-banner">{syncNotice}</span>
        </div>
        <div className="topbar-right">
          <span className="badge badge-neutral">
            {engineStatus?.runtime === "tauri-v2" ? "Tauri v2 Native" : "Web Preview"}
          </span>
          <button type="button" className="pill-btn" onClick={handleResetDemo}>
            重置演示模型
          </button>
        </div>
      </header>

      <div className="workbench-columns">
        <LeftPanel
          contract={DEMO_DESIGN_CONTRACT}
          diagnosticsReport={evaluated.diagnostics}
          kclSource={kclSource}
          highlightedSpan={highlightedSpan}
          highlightLabel={highlightLabel}
          onChangeSource={setKclSource}
          onSelectSpan={handleFocusAstSpan}
          onAiGenerate={handleAiGenerate}
        />

        <CenterViewport
          kclSource={kclSource}
          sceneMesh={evaluated.sceneMesh}
          keepInBbox={DEMO_DESIGN_CONTRACT.keep_in_bbox_mm}
          drawingSvg={drawingSvg}
          selectedComponentId={selectedComponentId}
          selectedTarget={selectedTarget}
          onSelectTarget={setSelectedTarget}
          onCommitKclSource={handleCommitKclSource}
          onFocusAstSpan={handleFocusAstSpan}
        />

        <RightPanel
          kclSource={kclSource}
          contract={DEMO_DESIGN_CONTRACT}
          diagnosticsReport={evaluated.diagnostics}
          sceneMesh={evaluated.sceneMesh}
          selectedMaterial={selectedMaterial}
          onSelectMaterial={setSelectedMaterial}
          dxfContent={dxfContent}
          drawingSvg={drawingSvg}
          selectedComponentId={selectedComponentId}
          onSelectComponent={setSelectedComponentId}
        />
      </div>
    </div>
  );
}
