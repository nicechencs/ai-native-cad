import {
  estimateSketchDof,
  FEATURE_DASHBOARD_META,
  ISO_METRIC_CLEARANCE_MM,
  ISO_METRIC_DESIGNATIONS,
  type FeatureDashboardDraft,
} from "../../core/modeling/featureDashboardModel.ts";
import type {
  BooleanOpKind,
  DatumPlane,
  FastenerClearance,
  FastenerHoleStandard,
  FeatureBodyOp,
  MateKindSpec,
} from "../../types/apiCommands.ts";
import { CadIcon, type CadIconName } from "../../ui/icons/CadIcons.tsx";

export interface FeatureDashboardProps {
  readonly draft: FeatureDashboardDraft | null;
  readonly onChange: (next: FeatureDashboardDraft) => void;
  readonly onApply: () => void;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
  readonly onRemoveReference: (refId: string) => void;
  readonly onClearReferences: () => void;
}

function dashboardIcon(kind: FeatureDashboardDraft["kind"]): CadIconName {
  switch (kind) {
    case "hole":
      return "throughHole";
    case "fillet":
      return "fillet";
    case "chamfer":
      return "chamfer";
    case "primitive_box":
      return "boxPrimitive";
    case "primitive_cylinder":
      return "cylinderPrimitive";
    case "extrude":
      return "extrude";
    case "revolve":
      return "revolve";
    case "shell":
      return "shell";
    case "draft":
      return "draft";
    case "pattern":
      return "pattern";
    case "mirror":
      return "mirror";
    case "boolean":
      return "booleanOp";
    case "direct_edit":
      return "directEdit";
    case "mate":
      return "mate";
    case "tag_topology":
      return "tag";
    case "design_assertion":
      return "assertGate";
    case "sketch":
      return "sketch";
  }
}

/**
 * Creo Dashboard + NX Dialog (Preview / Apply / OK) hybrid horizontal feature strip.
 * Anchored neatly inside the top of the 3D viewport without obstructing the central model.
 */
export function FeatureDashboard({
  draft,
  onChange,
  onApply,
  onConfirm,
  onCancel,
  onRemoveReference,
  onClearReferences,
}: FeatureDashboardProps) {
  if (!draft) return null;

  const meta = FEATURE_DASHBOARD_META[draft.kind];
  const patch = (partial: Partial<FeatureDashboardDraft>) => onChange({ ...draft, ...partial });
  const sketchDof = draft.kind === "sketch"
    ? estimateSketchDof(draft.sketchEntities, draft.sketchConstraints)
    : null;

  const numInput = (
    label: string,
    value: number,
    unit: string,
    onSet: (n: number) => void,
    options?: { readonly min?: number; readonly max?: number; readonly step?: number; readonly width?: number },
  ) => (
    <label className="fd-field">
      <span className="fd-field-label">{label}</span>
      <span className="fd-num-wrap">
        <input
          type="number"
          className="fd-num-input"
          style={options?.width ? { width: options.width } : undefined}
          min={options?.min}
          max={options?.max}
          step={options?.step ?? 0.5}
          value={value}
          onChange={(event) => {
            const parsed = Number.parseFloat(event.target.value);
            if (Number.isFinite(parsed)) onSet(parsed);
          }}
        />
        {unit && <span className="fd-unit">{unit}</span>}
      </span>
    </label>
  );

  return (
    <section
      className={`feature-dashboard${draft.kind === "sketch" ? " is-sketch" : ""}`}
      role="region"
      aria-label={`${meta.title} 特征仪表板`}
    >
      <div className="fd-main-row">
        <div className="fd-identity">
          <span className="fd-icon-badge">
            <CadIcon name={dashboardIcon(draft.kind)} size={15} />
          </span>
          <input
            type="text"
            className="fd-name-input"
            aria-label="特征名称"
            value={draft.featureName}
            onChange={(event) => patch({ featureName: event.target.value })}
          />
        </div>

        {/* Creo-style Reference Collector (yellow/amber active highlight when awaiting pick) */}
        {meta.collectorTarget !== "none" && (
          <div
            className={`fd-collector${draft.references.length === 0 ? " is-collecting" : " is-filled"}`}
            title={`${meta.collectorLabel}：在 3D 视口点击几何体即可收集`}
          >
            <span className="fd-collector-label">{meta.collectorLabel}</span>
            {draft.references.length === 0 ? (
              <span className="fd-collector-placeholder">点击视口拾取…</span>
            ) : (
              <div className="fd-collector-chips">
                {draft.references.slice(0, 3).map((refId) => (
                  <span key={refId} className="fd-ref-chip">
                    <span>{refId}</span>
                    <button
                      type="button"
                      aria-label={`移除参考 ${refId}`}
                      onClick={() => onRemoveReference(refId)}
                    >
                      ×
                    </button>
                  </span>
                ))}
                {draft.references.length > 3 && (
                  <span className="mini-badge">+{draft.references.length - 3}</span>
                )}
                <button
                  type="button"
                  className="fd-ref-clear"
                  title="清空收集器"
                  onClick={onClearReferences}
                >
                  清空
                </button>
              </div>
            )}
          </div>
        )}

        {/* Primary contextual parameters */}
        <div className="fd-params-strip">
          {draft.kind === "hole" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">孔型</span>
                <select
                  className="fd-select"
                  value={draft.holeMode}
                  onChange={(event) => patch({ holeMode: event.target.value as FeatureDashboardDraft["holeMode"] })}
                >
                  <option value="simple">通孔 / 简单孔</option>
                  <option value="counterbore">沉头孔</option>
                  <option value="countersink">沉锥孔</option>
                  <option value="standard">ISO 标准孔</option>
                </select>
              </label>

              {draft.holeMode === "standard" ? (
                <>
                  <label className="fd-field">
                    <span className="fd-field-label">规格</span>
                    <select
                      className="fd-select"
                      value={draft.holeDesignation}
                      onChange={(event) => {
                        const des = event.target.value;
                        const spec = ISO_METRIC_CLEARANCE_MM[des];
                        patch({
                          holeDesignation: des,
                          ...(spec
                            ? {
                                diameterMm: spec.through,
                                cboreDiameterMm: spec.cbore,
                                cboreDepthMm: spec.cboreDepth,
                              }
                            : {}),
                        });
                      }}
                    >
                      {ISO_METRIC_DESIGNATIONS.map((m) => (
                        <option key={m} value={m}>{m}</option>
                      ))}
                    </select>
                  </label>
                  <label className="fd-field">
                    <span className="fd-field-label">标准</span>
                    <select
                      className="fd-select"
                      value={draft.holeStandard}
                      onChange={(event) => patch({ holeStandard: event.target.value as FastenerHoleStandard })}
                    >
                      <option value="iso4762_counterbore">ISO 4762 沉头</option>
                      <option value="iso10642_countersink">ISO 10642 沉锥</option>
                      <option value="metric_clearance">公制通孔</option>
                      <option value="metric_tap_drill">螺纹底孔</option>
                    </select>
                  </label>
                </>
              ) : (
                numInput("孔径 Ø", draft.diameterMm, "mm", (v) => patch({ diameterMm: Math.max(0.5, v) }), { min: 0.5 })
              )}

              <label className="fd-field">
                <span className="fd-field-label">深度</span>
                <select
                  className="fd-select"
                  value={draft.holeDepthKind}
                  onChange={(event) => patch({ holeDepthKind: event.target.value as "through_all" | "blind" })}
                >
                  <option value="through_all">贯穿</option>
                  <option value="blind">盲孔</option>
                </select>
              </label>
              {draft.holeDepthKind === "blind" &&
                numInput("深", draft.depthMm, "mm", (v) => patch({ depthMm: Math.max(0.5, v) }), { min: 0.5 })}
            </>
          )}

          {draft.kind === "fillet" &&
            numInput("半径 R", draft.radiusMm, "mm", (v) => patch({ radiusMm: Math.max(0.2, v) }), { min: 0.2 })}

          {draft.kind === "chamfer" && (
            <>
              {numInput("距离 C", draft.chamferDistanceMm, "mm", (v) => patch({ chamferDistanceMm: Math.max(0.2, v) }), { min: 0.2 })}
              {!draft.chamferSymmetric &&
                numInput("距离 C2", draft.chamferDistance2Mm, "mm", (v) => patch({ chamferDistance2Mm: Math.max(0.2, v) }), { min: 0.2 })}
            </>
          )}

          {draft.kind === "primitive_box" && (
            <>
              {numInput("长 X", draft.widthMm, "mm", (v) => patch({ widthMm: Math.max(1, v) }), { min: 1 })}
              {numInput("宽 Y", draft.lengthMm, "mm", (v) => patch({ lengthMm: Math.max(1, v) }), { min: 1 })}
              {numInput("高 Z", draft.heightMm, "mm", (v) => patch({ heightMm: Math.max(1, v) }), { min: 1 })}
            </>
          )}

          {draft.kind === "primitive_cylinder" && (
            <>
              {numInput("直径 Ø", draft.diameterMm, "mm", (v) => patch({ diameterMm: Math.max(1, v) }), { min: 1 })}
              {numInput("高度 H", draft.heightMm, "mm", (v) => patch({ heightMm: Math.max(1, v) }), { min: 1 })}
            </>
          )}

          {draft.kind === "extrude" && (
            <>
              {numInput("拉伸距离", draft.heightMm, "mm", (v) => patch({ heightMm: Math.max(0.5, v) }), { min: 0.5 })}
              <label className="fd-field">
                <span className="fd-field-label">布尔</span>
                <select
                  className="fd-select"
                  value={draft.bodyOp}
                  onChange={(event) => patch({ bodyOp: event.target.value as FeatureBodyOp })}
                >
                  <option value="join">合并</option>
                  <option value="cut">切除</option>
                  <option value="new_body">新建实体</option>
                  <option value="intersect">求交</option>
                </select>
              </label>
              <label className="fd-check-inline">
                <input
                  type="checkbox"
                  checked={draft.symmetric}
                  onChange={(event) => patch({ symmetric: event.target.checked })}
                />
                <span>对称</span>
              </label>
            </>
          )}

          {draft.kind === "revolve" && (
            <>
              {numInput("旋转角", draft.angleDeg, "°", (v) => patch({ angleDeg: Math.max(1, Math.min(360, v)) }), { min: 1, max: 360, step: 15 })}
              <label className="fd-field">
                <span className="fd-field-label">布尔</span>
                <select
                  className="fd-select"
                  value={draft.bodyOp}
                  onChange={(event) => patch({ bodyOp: event.target.value as FeatureBodyOp })}
                >
                  <option value="join">合并</option>
                  <option value="cut">切除</option>
                  <option value="new_body">新建实体</option>
                </select>
              </label>
            </>
          )}

          {draft.kind === "shell" && (
            <>
              {numInput("壁厚", draft.thicknessMm, "mm", (v) => patch({ thicknessMm: Math.max(0.4, v) }), { min: 0.4 })}
              <label className="fd-check-inline">
                <input
                  type="checkbox"
                  checked={draft.inward}
                  onChange={(event) => patch({ inward: event.target.checked })}
                />
                <span>向内抽壳</span>
              </label>
            </>
          )}

          {draft.kind === "draft" && (
            <>
              {numInput("拔模角", draft.angleDeg, "°", (v) => patch({ angleDeg: Math.max(0.1, Math.min(45, v)) }), { min: 0.1, max: 45, step: 0.5 })}
              <label className="fd-field">
                <span className="fd-field-label">中性面</span>
                <select
                  className="fd-select"
                  value={draft.datumPlane}
                  onChange={(event) => patch({ datumPlane: event.target.value as DatumPlane })}
                >
                  <option value="xy">XY 平面</option>
                  <option value="xz">XZ 平面</option>
                  <option value="yz">YZ 平面</option>
                </select>
              </label>
            </>
          )}

          {draft.kind === "pattern" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">模式</span>
                <select
                  className="fd-select"
                  value={draft.patternType}
                  onChange={(event) => patch({ patternType: event.target.value as "linear" | "polar" })}
                >
                  <option value="polar">环形阵列</option>
                  <option value="linear">线性阵列</option>
                </select>
              </label>
              {numInput("数量", draft.patternCount, "个", (v) => patch({ patternCount: Math.max(2, Math.round(v)) }), { min: 2, step: 1, width: 48 })}
              {draft.patternType === "polar"
                ? numInput("总角度", draft.angleDeg, "°", (v) => patch({ angleDeg: Math.max(10, Math.min(360, v)) }), { min: 10, max: 360, step: 15 })
                : numInput("间距", draft.patternSpacingMm, "mm", (v) => patch({ patternSpacingMm: Math.max(1, v) }), { min: 1 })}
            </>
          )}

          {draft.kind === "mirror" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">对称平面</span>
                <select
                  className="fd-select"
                  value={draft.datumPlane}
                  onChange={(event) => patch({ datumPlane: event.target.value as DatumPlane })}
                >
                  <option value="yz">YZ 平面</option>
                  <option value="xz">XZ 平面</option>
                  <option value="xy">XY 平面</option>
                </select>
              </label>
              <label className="fd-check-inline">
                <input
                  type="checkbox"
                  checked={draft.keepOriginal}
                  onChange={(event) => patch({ keepOriginal: event.target.checked })}
                />
                <span>保留原体</span>
              </label>
            </>
          )}

          {draft.kind === "boolean" && (
            <label className="fd-field">
              <span className="fd-field-label">运算类型</span>
              <select
                className="fd-select"
                value={draft.booleanOp}
                onChange={(event) => patch({ booleanOp: event.target.value as BooleanOpKind })}
              >
                <option value="union">布尔合并 (Union)</option>
                <option value="subtract">布尔切除 (Subtract)</option>
                <option value="intersect">布尔求交 (Intersect)</option>
              </select>
            </label>
          )}

          {draft.kind === "direct_edit" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">操作</span>
                <select
                  className="fd-select"
                  value={draft.directEditOp}
                  onChange={(event) => patch({ directEditOp: event.target.value as FeatureDashboardDraft["directEditOp"] })}
                >
                  <option value="offset">偏移面 (Offset)</option>
                  <option value="move">移动面 (Move)</option>
                  <option value="delete_and_heal">删面修补</option>
                </select>
              </label>
              {draft.directEditOp !== "delete_and_heal" &&
                numInput("偏移量", draft.offsetMm, "mm", (v) => patch({ offsetMm: v }), { step: 0.5 })}
            </>
          )}

          {draft.kind === "mate" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">配合类型</span>
                <select
                  className="fd-select"
                  value={draft.mateKind}
                  onChange={(event) => patch({ mateKind: event.target.value as MateKindSpec["kind"] })}
                >
                  <option value="coincident">重合 (Coincident)</option>
                  <option value="concentric">同心 (Concentric)</option>
                  <option value="distance">距离 (Distance)</option>
                  <option value="angle">角度 (Angle)</option>
                  <option value="parallel">平行 (Parallel)</option>
                  <option value="perpendicular">垂直 (Perpendicular)</option>
                  <option value="gear">齿轮比 (Gear)</option>
                </select>
              </label>
              {(draft.mateKind === "distance" || draft.mateKind === "angle" || draft.mateKind === "gear") &&
                numInput(
                  draft.mateKind === "distance" ? "距离" : draft.mateKind === "angle" ? "角度" : "传动比",
                  draft.mateValue,
                  draft.mateKind === "distance" ? "mm" : draft.mateKind === "angle" ? "°" : ":1",
                  (v) => patch({ mateValue: v }),
                )}
            </>
          )}

          {draft.kind === "tag_topology" && (
            <label className="fd-field">
              <span className="fd-field-label">语义标签</span>
              <input
                type="text"
                className="fd-text-input"
                placeholder="如 bearing_seat_wall"
                value={draft.semanticTag}
                onChange={(event) => patch({ semanticTag: event.target.value })}
              />
            </label>
          )}

          {draft.kind === "design_assertion" && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">断言规则</span>
                <select
                  className="fd-select"
                  value={draft.assertionRule}
                  onChange={(event) => patch({ assertionRule: event.target.value as FeatureDashboardDraft["assertionRule"] })}
                >
                  <option value="min_wall_thickness">最小壁厚</option>
                  <option value="volume_bounds">体积上下限</option>
                  <option value="boolean_condition">表达式条件</option>
                </select>
              </label>
              {draft.assertionRule === "min_wall_thickness" &&
                numInput("最小壁厚", draft.assertionMin, "mm", (v) => patch({ assertionMin: Math.max(0.2, v) }), { min: 0.2 })}
              {draft.assertionRule === "boolean_condition" && (
                <input
                  type="text"
                  className="fd-text-input"
                  value={draft.assertionExpr}
                  onChange={(event) => patch({ assertionExpr: event.target.value })}
                />
              )}
            </>
          )}

          {draft.kind === "sketch" && sketchDof && (
            <>
              <label className="fd-field">
                <span className="fd-field-label">基准面</span>
                <select
                  className="fd-select"
                  value={draft.datumPlane}
                  onChange={(event) => patch({ datumPlane: event.target.value as DatumPlane })}
                >
                  <option value="xy">XY 平面</option>
                  <option value="xz">XZ 平面</option>
                  <option value="yz">YZ 平面</option>
                </select>
              </label>
              <span className={`status-pill${sketchDof.status === "fully" ? " is-ok" : sketchDof.status === "over" ? " is-error" : " is-warn"}`}>
                {sketchDof.label}
              </span>
              <span className="mini-badge">
                {draft.sketchEntities.length} 图元 · {draft.sketchConstraints.length} 约束
              </span>
            </>
          )}
        </div>

        {/* Right Controls: Slide-up Advanced + Preview + Apply + Confirm + Cancel */}
        <div className="fd-actions">
          {(draft.kind === "hole" || draft.kind === "chamfer" || draft.kind === "sketch") && (
            <button
              type="button"
              className={`fd-btn${draft.advancedOpen ? " is-active" : ""}`}
              onClick={() => patch({ advancedOpen: !draft.advancedOpen })}
              title="展开 / 收起高级参数副面板 (Creo Slide-up Panel)"
            >
              <span>选项</span>
              <CadIcon name="chevronDown" size={11} />
            </button>
          )}

          <button
            type="button"
            className={`fd-icon-btn${draft.previewEnabled ? " is-active" : ""}`}
            aria-pressed={draft.previewEnabled}
            title={draft.previewEnabled ? "实时预览已开启" : "实时预览已关闭"}
            onClick={() => patch({ previewEnabled: !draft.previewEnabled })}
          >
            <CadIcon name={draft.previewEnabled ? "eye" : "eyeOff"} size={13} />
          </button>

          {meta.supportsApplyContinuous && (
            <button
              type="button"
              className="fd-btn"
              title="应用当前特征并保持仪表板打开以连续创建 (NX Apply)"
              onClick={onApply}
            >
              应用
            </button>
          )}

          <button
            type="button"
            className="fd-btn is-confirm"
            title="确认创建并关闭仪表板（或在视口空白处单击中键）"
            onClick={onConfirm}
          >
            <CadIcon name="check" size={13} />
            <span>{draft.kind === "sketch" ? "完成草图" : "确定"}</span>
          </button>

          <button
            type="button"
            className="fd-btn is-cancel"
            title="取消特征创建 (Esc)"
            onClick={onCancel}
          >
            <CadIcon name="close" size={13} />
          </button>
        </div>
      </div>

      {/* Creo Slide-up / Slide-down Advanced Options Panel */}
      {draft.advancedOpen && (
        <div className="fd-advanced-drawer">
          {draft.kind === "hole" && (
            <div className="fd-advanced-row">
              {numInput("沉头孔径", draft.cboreDiameterMm, "mm", (v) => patch({ cboreDiameterMm: Math.max(1, v) }), { min: 1 })}
              {numInput("沉头深度", draft.cboreDepthMm, "mm", (v) => patch({ cboreDepthMm: Math.max(0.5, v) }), { min: 0.5 })}
              <label className="fd-field">
                <span className="fd-field-label">配合等级</span>
                <select
                  className="fd-select"
                  value={draft.holeClearance}
                  onChange={(event) => patch({ holeClearance: event.target.value as FastenerClearance })}
                >
                  <option value="close">精密 (Close)</option>
                  <option value="normal">中等 (Normal)</option>
                  <option value="loose">宽松 (Loose)</option>
                </select>
              </label>
            </div>
          )}

          {draft.kind === "chamfer" && (
            <div className="fd-advanced-row">
              <label className="fd-check-inline">
                <input
                  type="checkbox"
                  checked={draft.chamferSymmetric}
                  onChange={(event) => patch({ chamferSymmetric: event.target.checked })}
                />
                <span>等距对称倒角 (45°)</span>
              </label>
            </div>
          )}

          {draft.kind === "sketch" && (
            <div className="fd-advanced-row">
              <span className="fd-field-label">当前草图图元：</span>
              <div className="fd-collector-chips">
                {draft.sketchEntities.map((ent) => (
                  <span key={ent.id} className="fd-ref-chip">
                    <span>{ent.kind}:{ent.id}</span>
                  </span>
                ))}
                {draft.sketchEntities.length === 0 && <span className="muted-text">使用上方「草图」功能区添加矩形/圆/直线</span>}
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
