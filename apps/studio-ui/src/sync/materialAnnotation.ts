/** First `@material("id")` annotation. The editor and the property dropdown share this text. */
const MATERIAL_ANNOTATION = /@material\(\s*(["'])([^"']+)\1\s*\)/;

export function materialIdInSource(source: string): string | null {
  const match = MATERIAL_ANNOTATION.exec(source);
  return match?.[2] ?? null;
}

export function sourceWithMaterial(source: string, materialId: string): string {
  const next = `@material("${materialId}")`;
  if (MATERIAL_ANNOTATION.test(source)) return source.replace(MATERIAL_ANNOTATION, next);
  return `${next}\n${source}`;
}

/** Volume is mm³. Density is g/cm³. One cm³ is 1000 mm³. */
export function massGramsFromVolume(volumeMm3: number, densityGPerCm3: number): number {
  return (volumeMm3 / 1000) * densityGPerCm3;
}
