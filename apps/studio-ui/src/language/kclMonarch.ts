/**
 * CAD-DSL (`.kcl`) Monaco Editor Monarch Language Definition & Standalone Tokenizer.
 * Supports `@unit`/`@pmi` annotations, `part`/`sketch`/`let`/`return` keywords,
 * physical dimension literals (`mm`/`deg`/`N`/`kN`/`MPa`), `constraint::*`/`boolean::*`
 * namespaces, axis/plane constants, and `|>` pipe operator.
 */

export const KCL_LANGUAGE_ID = "kcl" as const;

export type KclMonarchRule =
  | readonly [RegExp, string]
  | readonly [RegExp, string, string]
  | readonly [RegExp, { readonly cases: Readonly<Record<string, string>> }]
  | { readonly include: string };

export interface KclMonarchLanguage {
  readonly defaultToken: string;
  readonly tokenPostfix: string;
  readonly keywords: readonly string[];
  readonly typeKeywords: readonly string[];
  readonly builtinConstants: readonly string[];
  readonly dimensionUnits: readonly string[];
  readonly namespaces: readonly string[];
  readonly operators: readonly string[];
  readonly symbols: RegExp;
  readonly escapes: RegExp;
  readonly tokenizer: {
    readonly root: readonly KclMonarchRule[];
    readonly comment: readonly KclMonarchRule[];
    readonly string: readonly KclMonarchRule[];
    readonly whitespace: readonly KclMonarchRule[];
  };
}

export interface KclLanguageConfiguration {
  readonly comments: {
    readonly lineComment: string;
    readonly blockComment: readonly [string, string];
  };
  readonly brackets: readonly (readonly [string, string])[];
  readonly autoClosingPairs: readonly { readonly open: string; readonly close: string }[];
  readonly surroundingPairs: readonly { readonly open: string; readonly close: string }[];
}

export const kclLanguageConfiguration: KclLanguageConfiguration = {
  comments: {
    lineComment: "//",
    blockComment: ["/*", "*/"],
  },
  brackets: [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"],
  ],
  autoClosingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"' },
  ],
  surroundingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"' },
  ],
};

export const KCL_KEYWORDS = [
  "part",
  "assembly",
  "component",
  "sketch",
  "let",
  "return",
  "import",
  "const",
  "for",
  "in",
  "true",
  "false",
  "ellipse",
  "profile::ellipse",
  "spline",
  "profile::spline",
  "path::helix",
  "path::spline3d",
] as const;

export const KCL_BUILTIN_FUNCTIONS = [
  "box",
  "cylinder",
  "extrude",
  "extrude_symmetric",
  "revolve",
  "sweep",
  "loft",
  "shell",
  "mirror",
  "fillet",
  "chamfer",
  "point",
  "line",
  "arc",
  "circle",
  "rectangle",
  "ellipse",
  "profile::ellipse",
  "spline",
  "profile::spline",
  "path::helix",
  "path::spline3d",
] as const;

export const KCL_TYPE_KEYWORDS = [
  "Length",
  "Angle",
  "Force",
  "Pressure",
  "Solid",
  "Assembly",
  "Component",
  "Sketch",
  "Face",
  "Edge",
  "Point",
  "u32",
  "i32",
  "f64",
  "bool",
  "String",
] as const;

export const KCL_BUILTIN_CONSTANTS = [
  "X_AXIS",
  "Y_AXIS",
  "Z_AXIS",
  "XY_PLANE",
  "XZ_PLANE",
  "YZ_PLANE",
] as const;

export const KCL_DIMENSION_UNITS = [
  "mm",
  "m",
  "in",
  "deg",
  "rad",
  "N",
  "kN",
  "MPa",
] as const;

export const KCL_NAMESPACES = [
  "constraint",
  "boolean",
  "region",
  "array",
  "hole",
  "mate",
  "tag",
  "pmi",
  "assert",
  "implicit",
  "field",
  "force",
  "profile",
  "path",
  "import",
] as const;

export const kclMonarchTokensProvider: KclMonarchLanguage = {
  defaultToken: "identifier",
  tokenPostfix: ".kcl",
  keywords: KCL_KEYWORDS,
  typeKeywords: KCL_TYPE_KEYWORDS,
  builtinConstants: KCL_BUILTIN_CONSTANTS,
  dimensionUnits: KCL_DIMENSION_UNITS,
  namespaces: KCL_NAMESPACES,
  operators: ["|>", "->", "::", "=", "+", "-", "*", "/", ".."],
  symbols: /[=><!~?:&|+\-*\/\^%]+/,
  escapes: /\\(?:[abfnrtv\\"']|x[0-9A-Fa-f]{1,4}|u[0-9A-Fa-f]{4})/,
  tokenizer: {
    root: [
      { include: "@whitespace" },

      // Annotations: @unit, @material, @manufacturing, @pmi::tolerance
      [/@[a-zA-Z_][\w]*(?:::[a-zA-Z_][\w]*)*/, "annotation"],

      // Signed axis constants: +X, -X, +Y, -Y, +Z, -Z
      [/[+-][XYZ]\b/, "constant.axis"],

      // Pipe and return arrow operators
      [/\|>/, "operator.pipe"],
      [/->/, "operator.arrow"],

      // Namespace calls: constraint::coincident, boolean::subtract, mate::coaxial, hole::tapped, profile::ellipse, profile::spline, path::helix, path::spline3d, etc.
      [
        /\b(?:constraint|boolean|region|array|hole|mate|tag|pmi|assert|implicit|field|force|profile|path)::[a-zA-Z_][\w]*/,
        "namespace.call",
      ],
      [/[a-zA-Z_][\w]*::[a-zA-Z_][\w]*/, "namespace.call"],

      // Physical dimension literals: e.g., 42.0mm, 1e-3mm, 1_000N, 1.5MPa
      [
        /\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?(?:mm|kN|MPa|deg|rad|in|m|N)\b/,
        "number.dimension",
      ],

      // Plain float & integer literals
      [/\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?/, "number.float"],
      [/\d[\d_]*/, "number"],

      // Identifiers, keywords, types, constants, and method calls
      [
        /[a-zA-Z_][\w]*/,
        {
          cases: {
            "@keywords": "keyword",
            "@typeKeywords": "type",
            "@builtinConstants": "constant.plane",
            "@dimensionUnits": "unit.dimension",
            "@default": "identifier",
          },
        },
      ],

      // Delimiters & brackets
      [/[{}()\[\]]/, "@brackets"],
      [/[,.]/, "delimiter"],
      [/:/, "delimiter.colon"],

      // Operators
      [/@symbols/, "operator"],

      // Strings
      [/"([^"\\]|\\.)*$/, "string.invalid"],
      [/"/, "string", "@string"],
    ],

    comment: [
      [/[^\/*]+/, "comment"],
      [/\/\*/, "comment", "@push"],
      [/\*\//, "comment", "@pop"],
      [/[\/*]/, "comment"],
    ],

    string: [
      [/[^\\"]+/, "string"],
      [/@escapes/, "string.escape"],
      [/\\./, "string.escape.invalid"],
      [/"/, "string", "@pop"],
    ],

    whitespace: [
      [/[ \t\r\n]+/, "white"],
      [/\/\*/, "comment", "@comment"],
      [/\/\/.*$/, "comment"],
    ],
  },
};

export interface KclThemeRule {
  readonly token: string;
  readonly foreground: string;
  readonly fontStyle?: string;
}

export const kclThemeRules: readonly KclThemeRule[] = [
  { token: "keyword.kcl", foreground: "C586C0", fontStyle: "bold" },
  { token: "type.kcl", foreground: "4EC9B0" },
  { token: "annotation.kcl", foreground: "DCDCAA" },
  { token: "namespace.call.kcl", foreground: "79C0FF" },
  { token: "number.dimension.kcl", foreground: "B5CEA8", fontStyle: "bold" },
  { token: "unit.dimension.kcl", foreground: "9CDCFE" },
  { token: "number.float.kcl", foreground: "B5CEA8" },
  { token: "number.kcl", foreground: "B5CEA8" },
  { token: "constant.axis.kcl", foreground: "FFA657", fontStyle: "bold" },
  { token: "constant.plane.kcl", foreground: "FFA657" },
  { token: "operator.pipe.kcl", foreground: "FF7B72", fontStyle: "bold" },
  { token: "operator.arrow.kcl", foreground: "FF7B72" },
  { token: "string.kcl", foreground: "CE9178" },
  { token: "comment.kcl", foreground: "6A9955", fontStyle: "italic" },
];

/** Minimal structural interface matching `monaco-editor` to avoid hard runtime coupling. */
export interface MonacoLike {
  readonly languages: {
    register(language: { readonly id: string; readonly extensions?: readonly string[]; readonly aliases?: readonly string[] }): void;
    setLanguageConfiguration(languageId: string, configuration: unknown): void;
    setMonarchTokensProvider(languageId: string, languageDef: unknown): void;
  };
  readonly editor?: {
    defineTheme(themeName: string, themeData: {
      readonly base: "vs" | "vs-dark" | "hc-black";
      readonly inherit: boolean;
      readonly rules: readonly KclThemeRule[];
      readonly colors: Readonly<Record<string, string>>;
    }): void;
  };
}

export function registerKclLanguage(monaco: MonacoLike): void {
  monaco.languages.register({
    id: KCL_LANGUAGE_ID,
    extensions: [".kcl"],
    aliases: ["CAD-DSL", "KCL", "kcl"],
  });
  monaco.languages.setLanguageConfiguration(KCL_LANGUAGE_ID, kclLanguageConfiguration);
  monaco.languages.setMonarchTokensProvider(KCL_LANGUAGE_ID, kclMonarchTokensProvider);

  monaco.editor?.defineTheme("kcl-dark", {
    base: "vs-dark",
    inherit: true,
    rules: kclThemeRules,
    colors: {
      "editor.background": "#0D1117",
      "editor.lineHighlightBackground": "#161B22",
      "editorLineNumber.foreground": "#6E7681",
      "editorCursor.foreground": "#58A6FF",
    },
  });
}

export type KclTokenCategory =
  | "comment"
  | "annotation"
  | "keyword"
  | "type"
  | "namespace"
  | "dimension"
  | "number"
  | "constant"
  | "pipe"
  | "string"
  | "operator"
  | "plain";

export interface KclHighlightToken {
  readonly text: string;
  readonly category: KclTokenCategory;
}

const KEYWORD_SET = new Set<string>(KCL_KEYWORDS);
const TYPE_SET = new Set<string>(KCL_TYPE_KEYWORDS);
const CONSTANT_SET = new Set<string>(KCL_BUILTIN_CONSTANTS);

const LINE_TOKEN_REGEX =
  /(\/\/.*$|@[a-zA-Z_][\w]*(?:::[a-zA-Z_][\w]*)*|"(?:[^"\\]|\\.)*"|\|>|->|[+-][XYZ]\b|\b[a-zA-Z_][\w]*::[a-zA-Z_][\w]*|\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?(?:mm|kN|MPa|deg|rad|in|m|N)\b|\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?\b|\b[a-zA-Z_][\w]*\b)/g;

/** Lightweight single-line tokenizer mirroring `kclMonarchTokensProvider` for standalone preview shell. */
export function tokenizeKclLine(line: string): readonly KclHighlightToken[] {
  const tokens: KclHighlightToken[] = [];
  let cursor = 0;

  for (const match of line.matchAll(LINE_TOKEN_REGEX)) {
    const start = match.index ?? 0;
    const text = match[0];

    if (start > cursor) {
      tokens.push({ text: line.slice(cursor, start), category: "plain" });
    }

    let category: KclTokenCategory = "plain";
    if (text.startsWith("//")) {
      category = "comment";
    } else if (text.startsWith("@")) {
      category = "annotation";
    } else if (text.startsWith('"')) {
      category = "string";
    } else if (text === "|>") {
      category = "pipe";
    } else if (text === "->") {
      category = "operator";
    } else if (/^[+-][XYZ]$/.test(text) || CONSTANT_SET.has(text)) {
      category = "constant";
    } else if (text.includes("::")) {
      category = "namespace";
    } else if (/^\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d[\d_]*)?(?:mm|kN|MPa|deg|rad|in|m|N)$/.test(text)) {
      category = "dimension";
    } else if (/^\d/.test(text)) {
      category = "number";
    } else if (KEYWORD_SET.has(text)) {
      category = "keyword";
    } else if (TYPE_SET.has(text)) {
      category = "type";
    }

    tokens.push({ text, category });
    cursor = start + text.length;
  }

  if (cursor < line.length) {
    tokens.push({ text: line.slice(cursor), category: "plain" });
  }

  return tokens;
}
