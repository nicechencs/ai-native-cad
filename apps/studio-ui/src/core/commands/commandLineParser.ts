export type ParsedCommandLine =
  | { readonly kind: "command"; readonly commandId: string; readonly args?: unknown }
  | { readonly kind: "ai"; readonly mode: "generate" | "edit"; readonly prompt: string }
  | { readonly kind: "error"; readonly message: string }
  | { readonly kind: "empty" };

function parsePositiveNumber(raw?: string): number | undefined {
  if (!raw) return undefined;
  const cleaned = raw.replace(/(?:mm|deg)$/i, "");
  const value = Number.parseFloat(cleaned);
  return Number.isFinite(value) && value > 0 ? value : undefined;
}

export function parseCommandLine(input: string): ParsedCommandLine {
  const trimmed = input.trim();
  if (!trimmed) {
    return { kind: "empty" };
  }

  if (trimmed.startsWith("/ai") || trimmed.startsWith("ai ")) {
    const rest = trimmed.slice(3).trim();
    if (!rest) {
      return { kind: "command", commandId: "ai.openEdit" };
    }
    if (rest.startsWith("new ") || rest.startsWith("新建 ")) {
      const prompt = rest.replace(/^(?:new|新建)\s+/i, "").trim();
      if (!prompt) {
        return { kind: "command", commandId: "ai.openGenerate" };
      }
      return { kind: "ai", mode: "generate", prompt };
    }
    return { kind: "ai", mode: "edit", prompt: rest };
  }

  const parts = trimmed.split(/\s+/);
  const head = parts[0].toLowerCase();
  const arg1 = parts[1];
  const arg2 = parts[2];

  switch (head) {
    case "h":
    case "hole":
    case "通孔":
      return {
        kind: "command",
        commandId: "model.throughHole",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "ch":
    case "cbore":
    case "沉头孔":
      return {
        kind: "command",
        commandId: "model.counterboreHole",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "f":
    case "fillet":
    case "圆角":
      return {
        kind: "command",
        commandId: "model.filletEdges",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "c":
    case "chamfer":
    case "倒角":
      return {
        kind: "command",
        commandId: "model.chamferEdges",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "s":
    case "sketch":
    case "草图": {
      const plane = arg1?.toLowerCase();
      if (plane === "xy") return { kind: "command", commandId: "model.sketch.xy" };
      if (plane === "xz") return { kind: "command", commandId: "model.sketch.xz" };
      return { kind: "command", commandId: "model.sketch" };
    }

    case "e":
    case "ext":
    case "extrude":
    case "拉伸":
      return {
        kind: "command",
        commandId: "model.extrude",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "r":
    case "rev":
    case "revolve":
    case "旋转":
      return { kind: "command", commandId: "model.revolve" };

    case "shell":
    case "抽壳":
      return {
        kind: "command",
        commandId: "model.shell",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "draft":
    case "拔模":
      return { kind: "command", commandId: "model.draft" };

    case "pattern":
    case "阵列":
      return { kind: "command", commandId: "model.pattern" };

    case "mirror":
    case "镜像":
      return { kind: "command", commandId: "model.mirror" };

    case "bool":
    case "boolean":
    case "布尔":
      return { kind: "command", commandId: "model.boolean" };

    case "box":
    case "长方体":
      return { kind: "command", commandId: "model.primitiveBox" };

    case "cyl":
    case "cylinder":
    case "圆柱":
      return { kind: "command", commandId: "model.primitiveCylinder" };

    case "direct":
    case "offset":
    case "偏移面":
      return {
        kind: "command",
        commandId: "model.directEdit",
        args: { sizeMm: parsePositiveNumber(arg1) },
      };

    case "mate":
    case "配合":
      return { kind: "command", commandId: "assembly.addMate" };

    case "tag":
    case "标注":
      return { kind: "command", commandId: "annotate.tagTopology" };

    case "regen":
    case "rebuild":
    case "再生":
      return { kind: "command", commandId: "model.regenerate" };

    case "nav":
    case "导航": {
      const mode = arg1?.toLowerCase();
      if (mode === "sw" || mode === "solidworks") return { kind: "command", commandId: "nav.preset.solidworks" };
      if (mode === "web" || mode === "trackpad") return { kind: "command", commandId: "nav.preset.web" };
      return { kind: "command", commandId: "nav.preset.nxCreo" };
    }

    case "iso":
    case "等轴测":
      return { kind: "command", commandId: "view.preset.iso" };
    case "top":
    case "俯视":
      return { kind: "command", commandId: "view.preset.top" };
    case "front":
    case "主视":
      return { kind: "command", commandId: "view.preset.front" };
    case "right":
    case "右视":
      return { kind: "command", commandId: "view.preset.right" };
    case "left":
    case "左视":
      return { kind: "command", commandId: "view.preset.left" };
    case "back":
    case "后视":
      return { kind: "command", commandId: "view.preset.back" };
    case "bottom":
    case "仰视":
      return { kind: "command", commandId: "view.preset.bottom" };
    case "fit":
    case "zoomfit":
    case "适配":
      return { kind: "command", commandId: "view.fit" };
    case "ortho":
    case "正交":
      return { kind: "command", commandId: "display.projection.ortho" };
    case "persp":
    case "perspective":
    case "透视":
      return { kind: "command", commandId: "display.projection.perspective" };
    case "wireframe":
    case "线框":
      return { kind: "command", commandId: "display.style.wireframe" };
    case "shaded":
    case "着色":
      return { kind: "command", commandId: "display.style.shaded" };
    case "translucent":
    case "xray":
    case "半透明":
      return { kind: "command", commandId: "display.style.translucent" };
    case "hlr":
    case "线稿":
      return { kind: "command", commandId: "display.style.hlr" };
    case "showall":
    case "显示全部":
      return { kind: "command", commandId: "display.showAll" };

    case "3d":
      return { kind: "command", commandId: "view.mode3d" };
    case "2d":
    case "drawing":
    case "工程图":
      return { kind: "command", commandId: "view.modeDrawing" };

    case "bbox":
    case "boxenv":
    case "包络":
      return { kind: "command", commandId: "view.toggleKeepInBox" };

    case "section":
    case "sec":
    case "剖切": {
      if (!arg1) {
        return { kind: "command", commandId: "view.toggleSectionCut" };
      }
      const lowerArg = arg1.toLowerCase();
      if (lowerArg === "off" || lowerArg === "关闭") {
        return { kind: "command", commandId: "view.setSectionCut", args: { enabled: false } };
      }
      if (lowerArg === "flip" || lowerArg === "翻转") {
        return { kind: "command", commandId: "view.flipSectionCut" };
      }
      const axis = lowerArg === "x" ? "X" : lowerArg === "y" ? "Y" : lowerArg === "z" ? "Z" : undefined;
      const ratioRaw = axis ? arg2 : arg1;
      const ratio = ratioRaw ? Number.parseFloat(ratioRaw) : undefined;
      return {
        kind: "command",
        commandId: "view.setSectionCut",
        args: {
          enabled: true,
          ...(axis ? { axis } : {}),
          ...(ratio !== undefined && Number.isFinite(ratio)
            ? { ratio: Math.min(0.95, Math.max(0.05, ratio)) }
            : {}),
        },
      };
    }

    case "measure":
    case "m":
    case "测量": {
      const sub = arg1?.toLowerCase();
      if (sub === "face" || sub === "f" || sub === "面") {
        return { kind: "command", commandId: "inspect.measureFace" };
      }
      if (sub === "clear" || sub === "off" || sub === "清除") {
        return { kind: "command", commandId: "inspect.clearMeasurement" };
      }
      return { kind: "command", commandId: "inspect.measureCircle" };
    }

    case "export":
    case "导出": {
      const fmt = arg1?.toLowerCase();
      if (fmt === "step" || fmt === "stp") return { kind: "command", commandId: "export.step" };
      if (fmt === "dxf") return { kind: "command", commandId: "export.dxf" };
      if (fmt === "svg" || fmt === "drawing") return { kind: "command", commandId: "export.drawing" };
      if (fmt === "3mf") return { kind: "command", commandId: "export.3mf" };
      if (fmt === "mesh" || fmt === "json") return { kind: "command", commandId: "export.mesh" };
      return { kind: "command", commandId: "panel.openExport" };
    }

    case "undo":
    case "撤销":
      return { kind: "command", commandId: "edit.undo" };
    case "redo":
    case "重做":
      return { kind: "command", commandId: "edit.redo" };
    case "theme":
    case "主题":
      return { kind: "command", commandId: "theme.toggle" };
    case "zen":
      return { kind: "command", commandId: "layout.toggleZen" };
    case "clear":
    case "esc":
      return { kind: "command", commandId: "selection.clear" };
    case "help":
    case "?":
      return { kind: "command", commandId: "help.shortcuts" };

    default:
      return {
        kind: "error",
        message: `未识别命令 "${head}"。试试 ext 20、hole 8、fillet 2、shell 2.5、sketch、iso、section z 0.5、nav nx 或 /ai <描述>`,
      };
  }
}

export const COMMAND_LINE_SUGGESTIONS: readonly {
  readonly syntax: string;
  readonly description: string;
}[] = [
  { syntax: "sketch", description: "新建 2D 参数化草图并进入草图上下文（简写 s）" },
  { syntax: "ext 20", description: "打开拉伸仪表板并预设 20mm（简写 e）" },
  { syntax: "hole 8", description: "在选中面打开 Ø8mm 通孔仪表板（简写 h）" },
  { syntax: "cbore 10", description: "打开沉头孔/ISO 标准孔仪表板（简写 ch）" },
  { syntax: "fillet 2", description: "打开边缘圆角仪表板 R2mm（简写 f）" },
  { syntax: "chamfer 1.5", description: "打开边缘倒角仪表板 1.5mm（简写 c）" },
  { syntax: "shell 2.5", description: "打开等壁厚抽壳仪表板 2.5mm" },
  { syntax: "iso / top / front / right", description: "切换 3D 标准视角" },
  { syntax: "section z 0.5", description: "沿 Z/Y/X 轴开启剖切（section off 关闭）" },
  { syntax: "nav nx / sw / web", description: "切换鼠标导航预设（NX/Creo · SW · Web）" },
  { syntax: "regen", description: "重新编译并运行 L0-L4 工程验证（Ctrl+G）" },
  { syntax: "export step / dxf / svg", description: "导出当前模型工程文件" },
  { syntax: "/ai 加 4 个 M4 螺纹孔", description: "直接向 AI 助手发送增量修改指令" },
  { syntax: "/ai new 铝合金传感器支架", description: "使用 AI 从零生成新零件" },
];
