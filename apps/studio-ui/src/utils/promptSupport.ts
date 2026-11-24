import rules from "../../../../services/cad-agent/src/cad_agent/unsupported_features.json" with { type: "json" };

const patterns = rules.map(({ id, label, pattern }) => ({ id, label, pattern: new RegExp(pattern) }));

/** Known template gaps only; passing this check does not prove the prompt is covered. */
export function checkTemplatePrompt(prompt: string) {
  const normalized = prompt.normalize("NFKC").toLowerCase();
  const matches = patterns.filter(({ pattern }) => pattern.test(normalized));
  if (matches.length === 0) return null;
  return {
    error_code: "unsupported_features",
    unsupported_features: matches.map(({ id }) => id),
    error: `自然语言建模暂不支持：${matches.map(({ label }) => label).join("、")}。未生成模型。`,
  };
}
