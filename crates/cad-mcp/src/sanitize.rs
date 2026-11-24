//! Labels copied from external STEP, IGES, or mesh files.

const MAX_LABEL_CHARS: usize = 240;

/// Drop Unicode control (`Cc`), format (`Cf`), and line/paragraph separator
/// (`Zl`/`Zp`) characters from an untrusted asset label, trim whitespace, and
/// cap length at 240 Unicode scalar values.
///
/// Aligned with `services/cad-agent/src/cad_agent/v2_context_adapter.py`.
/// The remaining text is safe to show as data. It is not a trusted name,
/// a tool description, or a system instruction.
pub fn sanitize_untrusted_label(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|&ch| !is_disallowed_label_char(ch))
        .collect();
    filtered.trim().chars().take(MAX_LABEL_CHARS).collect()
}

fn is_disallowed_label_char(ch: char) -> bool {
    if ch.is_control() {
        return true;
    }
    let code = ch as u32;
    matches!(
        code,
        // Line / Paragraph separators (Zl / Zp)
        0x2028..=0x2029
        // Unicode General Category Cf (Format characters), plus full Unicode Tag block
        | 0x00AD
        | 0x0600..=0x0605
        | 0x061C
        | 0x06DD
        | 0x070F
        | 0x0890..=0x0891
        | 0x08E2
        | 0x180E
        | 0x200B..=0x200F
        | 0x202A..=0x202E
        | 0x2060..=0x2064
        | 0x2066..=0x206F
        | 0xFEFF
        | 0xFFF9..=0xFFFB
        | 0x110BD
        | 0x110CD
        | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A
        | 0xE0000..=0xE007F
    )
}
