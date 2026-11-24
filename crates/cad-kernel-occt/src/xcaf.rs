//! Assembly, color, and PMI metadata for OCCT STEP XCAF exchange.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{KernelError, KernelOperationError};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafAssemblyNode {
    #[serde(with = "occurrence_id")]
    pub id: String,
    #[serde(with = "occurrence_id::optional")]
    pub parent_id: Option<String>,
    pub name: String,
    pub is_assembly: bool,
    pub is_reference: bool,
    /// Column-major transform relative to the parent, with translation in millimetres.
    pub location_matrix_4x4: [f64; 16],
    /// Linear RGBA, if the occurrence or its definition has a color.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub color_rgba: Option<[f64; 4]>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafPmiItem {
    pub name: String,
    pub kind: String,
    pub value_text: String,
}

/// A box part to create in an exported XCAF document.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafExportPartSpec {
    pub name: String,
    /// Positive box dimensions in millimetres.
    pub size_mm: [f64; 3],
    /// Translation from the assembly origin in millimetres.
    pub translation_mm: [f64; 3],
    /// Linear RGBA in [0, 1].
    pub color_rgba: [f64; 4],
}

/// Assembly and PMI metadata to export as an AP242 STEP document.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafExportSpec {
    pub assembly_name: String,
    pub parts: Vec<XcafExportPartSpec>,
    /// PMI on the first box. Dimensions are edge lengths and geometric tolerances are flatness.
    /// Their values are decimal millimetres. Each datum value is a unique identifier.
    pub pmi_items: Vec<XcafPmiItem>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafStats {
    pub total_shapes: usize,
    pub colored_shapes: usize,
    pub pmi_count: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct XcafStepMetadata {
    pub assembly_nodes: Vec<XcafAssemblyNode>,
    pub pmi_items: Vec<XcafPmiItem>,
    pub stats: XcafStats,
}

impl XcafStepMetadata {
    pub fn colored_nodes(&self) -> Vec<&XcafAssemblyNode> {
        self.assembly_nodes
            .iter()
            .filter(|node| node.color_rgba.is_some())
            .collect()
    }

    pub fn pmi_by_kind(&self, kind: &str) -> Vec<&XcafPmiItem> {
        self.pmi_items
            .iter()
            .filter(|item| item.kind == kind)
            .collect()
    }
}

/// Parse the JSON payload produced by the STEP XCAF bridge.
pub fn parse_xcaf_metadata_json(raw: &str) -> Result<XcafStepMetadata, KernelError> {
    json::parse(raw)
        .map_err(|error| KernelError::OperationFailed(format!("Invalid STEP XCAF JSON: {error}")))
}

/// Read assembly placements, colors, and PMI directly from a STEP file.
pub fn import_step_xcaf_metadata(path: &Path) -> Result<XcafStepMetadata, KernelError> {
    #[cfg(feature = "occt-ffi")]
    {
        native::import_step_xcaf_metadata(path)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = path;
        Err(KernelError::BackendUnavailable)
    }
}

/// Serialize an AP242 XCAF export specification to the bridge's JSON format.
pub fn serialize_xcaf_export_spec_json(spec: &XcafExportSpec) -> Result<String, KernelError> {
    validate_export_spec(spec)?;

    let mut json = String::new();
    json.push('{');
    json.push_str("\"assembly_name\":");
    push_json_string(&mut json, &spec.assembly_name);
    json.push_str(",\"parts\":[");
    for (index, part) in spec.parts.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        json.push('{');
        json.push_str("\"name\":");
        push_json_string(&mut json, &part.name);
        json.push_str(",\"size_mm\":");
        push_json_numbers(&mut json, &part.size_mm);
        json.push_str(",\"translation_mm\":");
        push_json_numbers(&mut json, &part.translation_mm);
        json.push_str(",\"color_rgba\":");
        push_json_numbers(&mut json, &part.color_rgba);
        json.push('}');
    }
    json.push_str("],\"pmi_items\":[");
    for (index, item) in spec.pmi_items.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        json.push('{');
        json.push_str("\"name\":");
        push_json_string(&mut json, &item.name);
        json.push_str(",\"kind\":");
        push_json_string(&mut json, &item.kind);
        json.push_str(",\"value_text\":");
        push_json_string(&mut json, &item.value_text);
        json.push('}');
    }
    json.push_str("]}");
    Ok(json)
}

/// Export an AP242 STEP document containing boxes, colors, assembly names, and PMI.
pub fn export_step_xcaf_metadata(path: &Path, spec: &XcafExportSpec) -> Result<(), KernelError> {
    try_export_step_xcaf_metadata(path, spec).map_err(Into::into)
}

/// Export STEP XCAF metadata with the native diagnostic preserved on failure.
pub fn try_export_step_xcaf_metadata(
    path: &Path,
    spec: &XcafExportSpec,
) -> Result<(), KernelOperationError> {
    #[cfg(feature = "occt-ffi")]
    {
        native::export_step_xcaf_metadata(path, spec)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = (path, spec);
        Err(KernelError::BackendUnavailable.into())
    }
}

fn validate_export_spec(spec: &XcafExportSpec) -> Result<(), KernelError> {
    if spec.assembly_name.is_empty() || spec.assembly_name.contains('\0') {
        return Err(KernelError::OperationFailed(
            "STEP XCAF export requires a non-empty assembly name without NUL bytes".into(),
        ));
    }
    if spec.parts.is_empty() {
        return Err(KernelError::OperationFailed(
            "STEP XCAF export requires at least one part".into(),
        ));
    }
    for part in &spec.parts {
        if part.name.is_empty() || part.name.contains('\0') {
            return Err(KernelError::OperationFailed(
                "STEP XCAF export part names must not be empty or contain NUL bytes".into(),
            ));
        }
        if part
            .size_mm
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(KernelError::OperationFailed(
                "STEP XCAF export box dimensions must be finite and positive".into(),
            ));
        }
        if part.translation_mm.iter().any(|value| !value.is_finite()) {
            return Err(KernelError::OperationFailed(
                "STEP XCAF export translations must be finite".into(),
            ));
        }
        if part
            .color_rgba
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(KernelError::OperationFailed(
                "STEP XCAF export colors must be finite values in [0,1]".into(),
            ));
        }
    }
    let mut datum_ids = HashSet::new();
    for item in &spec.pmi_items {
        if item.name.is_empty() || item.name.contains('\0') {
            return Err(KernelError::OperationFailed(
                "STEP XCAF PMI names must not be empty or contain NUL bytes".into(),
            ));
        }
        if item.value_text.is_empty() || item.value_text.contains('\0') {
            return Err(KernelError::OperationFailed(
                "STEP XCAF PMI values must not be empty or contain NUL bytes".into(),
            ));
        }
        match item.kind.as_str() {
            "dimension" | "geometric_tolerance" => {
                if !item
                    .value_text
                    .parse::<f64>()
                    .is_ok_and(|value| value.is_finite() && value >= 0.0)
                {
                    return Err(KernelError::OperationFailed(
                        "STEP XCAF PMI magnitudes must be finite and nonnegative".into(),
                    ));
                }
            }
            "datum" => {
                if !datum_ids.insert(item.value_text.as_str()) {
                    return Err(KernelError::OperationFailed(
                        "STEP XCAF datum identifiers must be unique".into(),
                    ));
                }
            }
            kind => {
                return Err(KernelError::OperationFailed(format!(
                    "unsupported STEP XCAF PMI kind {kind:?}"
                )));
            }
        }
    }
    Ok(())
}

fn push_json_numbers<const N: usize>(json: &mut String, values: &[f64; N]) {
    json.push('[');
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        json.push_str(&value.to_string());
    }
    json.push(']');
}

fn push_json_string(json: &mut String, value: &str) {
    json.push('"');
    for ch in value.chars() {
        match ch {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            '\u{08}' => json.push_str("\\b"),
            '\u{0c}' => json.push_str("\\f"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(json, "\\u{:04x}", ch as u32);
            }
            ch => json.push(ch),
        }
    }
    json.push('"');
}

// The bridge encodes occurrence IDs as unsigned integers. Expose their decimal strings in Rust.
mod occurrence_id {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::Error as _};

    pub(super) fn serialize<S: Serializer>(id: &str, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(id.parse().map_err(S::Error::custom)?)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<String, D::Error> {
        Ok(u64::deserialize(deserializer)?.to_string())
    }

    pub(super) mod optional {
        use super::*;

        pub(in crate::xcaf) fn serialize<S: Serializer>(
            id: &Option<String>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            id.as_deref()
                .map(str::parse::<u64>)
                .transpose()
                .map_err(S::Error::custom)?
                .serialize(serializer)
        }

        pub(in crate::xcaf) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<String>, D::Error> {
            Ok(Option::<u64>::deserialize(deserializer)?.map(|id| id.to_string()))
        }
    }
}

#[cfg(feature = "occt-ffi")]
mod native {
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::ptr::{self, NonNull};

    use super::{
        KernelError, KernelOperationError, Path, XcafExportSpec, XcafStepMetadata,
        parse_xcaf_metadata_json, serialize_xcaf_export_spec_json,
    };
    use crate::occt_ffi::OCCT_OK;

    unsafe extern "C" {
        fn occt_bridge_import_step_xcaf_json(
            step_path: *const c_char,
            out_json: *mut *mut c_char,
            err_buf: *mut c_char,
            err_buf_len: usize,
        ) -> c_int;
        fn occt_bridge_export_step_xcaf_json(
            step_path: *const c_char,
            spec_json: *const c_char,
            err_buf: *mut c_char,
            err_buf_len: usize,
        ) -> c_int;
        fn occt_bridge_free_string(ptr: *mut c_char);
    }

    struct OwnedJson(NonNull<c_char>);

    impl Drop for OwnedJson {
        fn drop(&mut self) {
            // SAFETY: This pointer came from the bridge and is owned by this guard.
            unsafe { occt_bridge_free_string(self.0.as_ptr()) };
        }
    }

    fn step_path(path: &Path) -> Result<CString, KernelError> {
        let utf8_path = path.to_str().ok_or_else(|| {
            KernelError::OperationFailed("STEP XCAF path must be valid UTF-8".into())
        })?;
        CString::new(utf8_path)
            .map_err(|_| KernelError::OperationFailed("STEP XCAF path contains a NUL byte".into()))
    }

    fn check_status(
        status: c_int,
        operation: &str,
        err_buf: &[u8],
    ) -> Result<(), KernelOperationError> {
        if status == OCCT_OK {
            return Ok(());
        }
        let native = crate::occt_ffi::take_last_error();
        if status == -4 {
            return Err(KernelError::BackendUnavailable.into());
        }
        if let Some(error) = native {
            return Err(error.into());
        }
        let detail = CStr::from_bytes_until_nul(err_buf)
            .map(CStr::to_string_lossy)
            .unwrap_or_default();
        let mut message = format!("STEP XCAF {operation} failed (status {status})");
        if !detail.is_empty() {
            message.push_str(": ");
            message.push_str(&detail);
        }
        Err(KernelError::OperationFailed(message).into())
    }

    pub(super) fn import_step_xcaf_metadata(path: &Path) -> Result<XcafStepMetadata, KernelError> {
        let path = step_path(path)?;
        let mut out_json = ptr::null_mut();
        let mut err_buf = [0_u8; 512];
        // SAFETY: The path is NUL-terminated, the out pointer is writable, and the
        // error buffer has the capacity passed to the bridge.
        let status = unsafe {
            occt_bridge_import_step_xcaf_json(
                path.as_ptr(),
                &mut out_json,
                err_buf.as_mut_ptr().cast(),
                err_buf.len(),
            )
        };
        let json = NonNull::new(out_json).map(OwnedJson);
        check_status(status, "import", &err_buf)?;
        let json = json.ok_or_else(|| {
            KernelError::OperationFailed("STEP XCAF import returned no JSON".into())
        })?;
        // SAFETY: On success, the bridge returns a NUL-terminated string owned by json.
        let raw = unsafe { CStr::from_ptr(json.0.as_ptr()) }
            .to_str()
            .map_err(|error| {
                KernelError::OperationFailed(format!("STEP XCAF JSON is not UTF-8: {error}"))
            })?;
        parse_xcaf_metadata_json(raw)
    }

    pub(super) fn export_step_xcaf_metadata(
        path: &Path,
        spec: &XcafExportSpec,
    ) -> Result<(), KernelOperationError> {
        let path = step_path(path)?;
        let json = serialize_xcaf_export_spec_json(spec)?;
        let json = CString::new(json).map_err(|_| {
            KernelError::OperationFailed("STEP XCAF export JSON contains a NUL byte".into())
        })?;
        let mut err_buf = [0_u8; 512];
        // SAFETY: Both strings are NUL-terminated and the error buffer is writable.
        let status = unsafe {
            occt_bridge_export_step_xcaf_json(
                path.as_ptr(),
                json.as_ptr(),
                err_buf.as_mut_ptr().cast(),
                err_buf.len(),
            )
        };
        check_status(status, "export", &err_buf)
    }
}

// Decode the bridge payload through Serde without requiring an extra crate dependency.
mod json {
    use serde::de::value::{Error, StringDeserializer};
    use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, forward_to_deserialize_any};

    pub(super) fn parse<'de, T: Deserialize<'de>>(raw: &'de str) -> Result<T, Error> {
        let mut parser = Parser {
            rest: raw,
            input_len: raw.len(),
            depth: 0,
        };
        let value = T::deserialize(&mut parser)?;
        if parser.peek().is_some() {
            return Err(parser.error("trailing data"));
        }
        Ok(value)
    }

    struct Parser<'de> {
        rest: &'de str,
        input_len: usize,
        depth: usize,
    }

    impl<'de> Parser<'de> {
        fn error(&self, message: &str) -> Error {
            Error::custom(format!(
                "{message} at byte {}",
                self.input_len - self.rest.len()
            ))
        }

        fn peek(&mut self) -> Option<u8> {
            self.rest = self.rest.trim_start_matches([' ', '\n', '\r', '\t']);
            self.rest.as_bytes().first().copied()
        }

        fn consume(&mut self, prefix: &str) -> bool {
            if let Some(rest) = self.rest.strip_prefix(prefix) {
                self.rest = rest;
                true
            } else {
                false
            }
        }

        fn expect(&mut self, token: &str) -> Result<(), Error> {
            self.peek();
            if self.consume(token) {
                Ok(())
            } else {
                Err(self.error(&format!("expected {token:?}")))
            }
        }

        fn character(&mut self) -> Result<char, Error> {
            let ch = self
                .rest
                .chars()
                .next()
                .ok_or_else(|| self.error("unterminated string"))?;
            self.rest = &self.rest[ch.len_utf8()..];
            Ok(ch)
        }

        fn string(&mut self) -> Result<String, Error> {
            self.expect("\"")?;
            let mut value = String::new();
            loop {
                let ch = match self.character()? {
                    '"' => return Ok(value),
                    '\\' => match self.character()? {
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'u' => self.unicode_escape()?,
                        _ => return Err(self.error("invalid string escape")),
                    },
                    '\0'..='\u{1f}' => return Err(self.error("unescaped control character")),
                    ch => ch,
                };
                value.push(ch);
            }
        }

        fn hex_unit(&mut self) -> Result<u16, Error> {
            let unit = self
                .rest
                .get(..4)
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .and_then(|digits| u16::from_str_radix(digits, 16).ok())
                .ok_or_else(|| self.error("invalid Unicode escape"))?;
            self.rest = &self.rest[4..];
            Ok(unit)
        }

        fn unicode_escape(&mut self) -> Result<char, Error> {
            let first = self.hex_unit()?;
            let code_point = if (0xd800..=0xdbff).contains(&first) {
                if !self.consume("\\u") {
                    return Err(self.error("expected low surrogate"));
                }
                let second = self.hex_unit()?;
                if !(0xdc00..=0xdfff).contains(&second) {
                    return Err(self.error("invalid low surrogate"));
                }
                0x10000 + (u32::from(first - 0xd800) << 10) + u32::from(second - 0xdc00)
            } else {
                u32::from(first)
            };
            char::from_u32(code_point).ok_or_else(|| self.error("invalid Unicode code point"))
        }

        fn digits(&mut self) -> bool {
            let len = self.rest.bytes().take_while(u8::is_ascii_digit).count();
            self.rest = &self.rest[len..];
            len != 0
        }

        fn number<V: Visitor<'de>>(&mut self, visitor: V) -> Result<V::Value, Error> {
            let start = self.rest;
            let negative = self.consume("-");
            if !self.consume("0") && !self.digits() {
                return Err(self.error("expected integer digits"));
            }
            let fractional = self.consume(".");
            if fractional && !self.digits() {
                return Err(self.error("expected decimal digits"));
            }
            let exponent = self.consume("e") || self.consume("E");
            if exponent {
                if !self.consume("+") {
                    self.consume("-");
                }
                if !self.digits() {
                    return Err(self.error("expected exponent digits"));
                }
            }
            let token = &start[..start.len() - self.rest.len()];
            if !fractional && !exponent && token != "-0" {
                if negative {
                    if let Ok(value) = token.parse::<i64>() {
                        return visitor.visit_i64(value);
                    }
                } else if let Ok(value) = token.parse::<u64>() {
                    return visitor.visit_u64(value);
                }
            }
            let value = token
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| self.error("number out of range"))?;
            visitor.visit_f64(value)
        }

        fn container<T>(
            &mut self,
            start: &str,
            end: &str,
            visit: impl FnOnce(Entries<'_, 'de>) -> Result<T, Error>,
        ) -> Result<T, Error> {
            self.expect(start)?;
            if self.depth == 128 {
                return Err(self.error("JSON nesting limit exceeded"));
            }
            self.depth += 1;
            let result = visit(Entries {
                parser: self,
                end: end.as_bytes()[0],
                first: true,
            });
            self.depth -= 1;
            let value = result?;
            self.expect(end)?;
            Ok(value)
        }
    }

    impl<'de> Deserializer<'de> for &mut Parser<'de> {
        type Error = Error;

        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            match self.peek() {
                Some(b'{') => self.container("{", "}", |entries| visitor.visit_map(entries)),
                Some(b'[') => self.container("[", "]", |entries| visitor.visit_seq(entries)),
                Some(b'"') => visitor.visit_string(self.string()?),
                Some(b'-' | b'0'..=b'9') => self.number(visitor),
                Some(b't') if self.consume("true") => visitor.visit_bool(true),
                Some(b'f') if self.consume("false") => visitor.visit_bool(false),
                Some(b'n') if self.consume("null") => visitor.visit_unit(),
                _ => Err(self.error("expected JSON value")),
            }
        }

        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Error> {
            self.container("{", "}", |entries| visitor.visit_map(entries))
        }

        fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            self.peek();
            if self.consume("null") {
                visitor.visit_none()
            } else {
                visitor.visit_some(self)
            }
        }

        forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf unit unit_struct newtype_struct seq tuple tuple_struct map
            enum identifier ignored_any
        }
    }

    struct Entries<'a, 'de> {
        parser: &'a mut Parser<'de>,
        end: u8,
        first: bool,
    }

    impl Entries<'_, '_> {
        fn has_next(&mut self) -> Result<bool, Error> {
            if self.parser.peek() == Some(self.end) {
                return Ok(false);
            }
            if !self.first {
                self.parser.expect(",")?;
            }
            self.first = false;
            Ok(true)
        }
    }

    impl<'de> SeqAccess<'de> for Entries<'_, 'de> {
        type Error = Error;

        fn next_element_seed<T: DeserializeSeed<'de>>(
            &mut self,
            seed: T,
        ) -> Result<Option<T::Value>, Error> {
            if self.has_next()? {
                seed.deserialize(&mut *self.parser).map(Some)
            } else {
                Ok(None)
            }
        }
    }

    impl<'de> MapAccess<'de> for Entries<'_, 'de> {
        type Error = Error;

        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Error> {
            if self.has_next()? {
                let key = self.parser.string()?;
                self.parser.expect(":")?;
                seed.deserialize(StringDeserializer::new(key)).map(Some)
            } else {
                Ok(None)
            }
        }

        fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
            seed.deserialize(&mut *self.parser)
        }
    }
}
