//! File-format detection and per-format parse / serialize codecs.
//!
//! # Architecture
//!
//! Every supported format is a variant of [`FileFormat`].  Each variant
//! implements three operations through match arms:
//!
//! | Method              | Purpose                                         |
//! |---------------------|-------------------------------------------------|
//! | `parse`             | raw text → `serde_json::Value` (internal tree)  |
//! | `serialize`         | `serde_json::Value` → raw text                  |
//! | `line_for_path`     | dot-notation path → 0-based line number         |
//!
//! # Adding a new format
//!
//! 1. Add a variant to `FileFormat`.
//! 2. Extend **every** `match` arm in the `impl FileFormat` block — the
//!    compiler will flag any arm you forget.
//! 3. Add the crate dependency to `Cargo.toml` if needed.
//! 4. Add file extensions to `all_extensions()`.
//! 5. Implement the private `parse_xxx` / `serialize_xxx` helpers below.

use std::path::Path;

use serde_json::Value;

// ── FileFormat ────────────────────────────────────────────────────────────────

/// All file formats the editor can open and save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileFormat {
    #[default]
    Json,
    Yaml,
    Toml,
    Xml,
}

impl FileFormat {
    /// Detect the format from a file path's extension.  Falls back to JSON.
    pub fn from_path(path: &Path) -> Self {
        path.extension()
            .and_then(|e| e.to_str())
            .map(Self::from_extension)
            .unwrap_or_default()
    }

    /// Detect format from a bare extension string (case-insensitive, no dot).
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "yaml" | "yml" => Self::Yaml,
            "toml" => Self::Toml,
            "xml" => Self::Xml,
            _ => Self::Json,
        }
    }

    /// Short uppercase label shown in the status bar / pane title.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Xml => "XML",
        }
    }

    /// File extensions associated with this format (lowercase, no leading dot).
    #[allow(dead_code)]
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Json => &["json"],
            Self::Yaml => &["yaml", "yml"],
            Self::Toml => &["toml"],
            Self::Xml => &["xml"],
        }
    }

    /// Every extension across all supported formats.
    ///
    /// Used by the file picker to decide which files to show and by the
    /// `is_supported` helper below.
    pub fn all_extensions() -> &'static [&'static str] {
        &["json", "yaml", "yml", "toml", "xml"]
    }

    /// Returns `true` if `ext` (no dot, any case) is handled by any format.
    pub fn is_supported_extension(ext: &str) -> bool {
        let lower = ext.to_lowercase();
        Self::all_extensions().contains(&lower.as_str())
    }

    /// Default filename for a "new file" save (no path was given on open).
    pub fn default_filename(&self) -> &'static str {
        match self {
            Self::Json => "output.json",
            Self::Yaml => "output.yaml",
            Self::Toml => "output.toml",
            Self::Xml => "output.xml",
        }
    }

    /// Canonical file extension for this format (no leading dot).
    pub fn default_extension(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Xml => "xml",
        }
    }

    // ── core codec operations ─────────────────────────────────────────────────

    /// Parse `raw` text into a `serde_json::Value` tree.
    ///
    /// Returns a human-readable error string on failure.
    pub fn parse(&self, raw: &str) -> Result<Value, String> {
        match self {
            Self::Json => parse_json(raw),
            Self::Yaml => parse_yaml(raw),
            Self::Toml => parse_toml(raw),
            Self::Xml => parse_xml(raw),
        }
    }

    /// Serialize a `serde_json::Value` back to this format's canonical text.
    pub fn serialize(&self, value: &Value) -> String {
        match self {
            Self::Json => serialize_json(value),
            Self::Yaml => serialize_yaml(value),
            Self::Toml => serialize_toml(value),
            Self::Xml => serialize_xml(value),
        }
    }

    /// Find the 0-based line number in `raw` that corresponds to the tree
    /// `path` (dot-notation, e.g. `"users[0].name"`).
    ///
    /// Returns `None` if the mapping is not supported for this format or the
    /// path cannot be located.  When `None`, the raw pane simply does not
    /// scroll on `g`/jump operations.
    pub fn line_for_path(&self, raw: &str, path: &str) -> Option<usize> {
        match self {
            Self::Json => json_line_for_path(raw, path),
            Self::Yaml => yaml_line_for_path(raw, path),
            Self::Toml => toml_line_for_path(raw, path),
            Self::Xml => None, // TODO: XML path→line
        }
    }
}

// ── JSON codec ────────────────────────────────────────────────────────────────

fn parse_json(raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|e| {
        let line = e.line();
        let col = e.column();
        let snippet = raw
            .lines()
            .nth(line.saturating_sub(1))
            .unwrap_or("")
            .trim();
        let preview = if snippet.len() > 40 {
            format!("{}…", &snippet[..40])
        } else {
            snippet.to_string()
        };
        format!(
            "JSON error at {}:{} — {} (near: \"{}\")",
            line, col, e, preview
        )
    })
}

fn serialize_json(value: &Value) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|e| format!("/* serialization error: {} */", e))
}

/// Depth-aware path→line mapping for serde_json pretty-printed output
/// (2-space indent per level).
///
/// Walks path segments one-by-one, searching forward from the previous match
/// so that sibling objects with the same key names cannot steal the result.
fn json_line_for_path(raw: &str, path: &str) -> Option<usize> {
    if path.is_empty() {
        return Some(0);
    }
    let parts = path_segments(path);
    if parts.is_empty() {
        return Some(0);
    }
    let lines: Vec<&str> = raw.lines().collect();
    let mut from_line = 0usize;

    for (depth_idx, part) in parts.iter().enumerate() {
        let indent_count = (depth_idx + 1) * 2;

        if let Ok(arr_idx) = part.parse::<usize>() {
            // Array element: count non-bracket lines at exactly this indent depth.
            let mut count = 0usize;
            let mut found = None;
            for li in from_line..lines.len() {
                let line = lines[li];
                let leading = line.len() - line.trim_start_matches(' ').len();
                if leading == indent_count {
                    let rest = line.trim_start();
                    if !rest.is_empty() && !rest.starts_with(']') && !rest.starts_with('}') {
                        if count == arr_idx {
                            found = Some(li);
                            break;
                        }
                        count += 1;
                    }
                }
            }
            from_line = found?;
        } else {
            // Object key: find `{indent}"key"` starting from from_line.
            let key_str = format!("{}\"{}\"", " ".repeat(indent_count), part);
            let found = lines[from_line..]
                .iter()
                .enumerate()
                .find(|(_, line)| line.contains(&key_str))
                .map(|(i, _)| from_line + i)?;
            from_line = found;
        }
    }
    Some(from_line)
}

/// Split a dot-notation path into individual string segments.
/// `"users[0].name"` → `["users", "0", "name"]`
fn path_segments(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for c in path.chars() {
        match c {
            '.' | '[' => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            ']' => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

// ── YAML codec ────────────────────────────────────────────────────────────────

/// Convert a `serde_yaml::Value` tree to `serde_json::Value`.
///
/// Direct `serde_yaml::from_str::<serde_json::Value>()` fails for any YAML
/// that contains:
///   * **Tagged values** — e.g. CloudFormation `!Ref`, `!Sub`, `!If` etc.
///     serde_yaml parses these as enum variants; serde_json::Value has no
///     enum type and rejects them with "invalid type: enum".
///   * **Non-string mapping keys** — YAML allows integer / boolean keys;
///     JSON requires strings.
///
/// This two-step approach parses to `serde_yaml::Value` first (which accepts
/// everything), then converts with the rules below:
///   * `!Tag scalar`   → `{ "!Tag": scalar }`   (CloudFormation convention)
///   * `!Tag sequence` → `{ "!Tag": [...] }`
///   * Non-string key  → `key.to_string()`
fn yaml_to_json(y: serde_yaml::Value) -> Value {
    match y {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            // Prefer integer representation; fall back to float; fall back to string.
            if let Some(i) = n.as_i64() {
                serde_json::json!(i)
            } else if let Some(u) = n.as_u64() {
                serde_json::json!(u)
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or_else(|| Value::String(n.to_string()))
            } else {
                Value::String(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(seq) => {
            Value::Array(seq.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                // Coerce non-string keys to their string representation.
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    serde_yaml::Value::Null => "null".to_string(),
                    other => format!("{other:?}"),
                };
                obj.insert(key, yaml_to_json(v));
            }
            Value::Object(obj)
        }
        serde_yaml::Value::Tagged(tagged) => {
            // Represent `!Tag value` as `{ "!Tag": value }` so the tag is
            // preserved in the tree pane and survives a round-trip back to YAML.
            let tag = tagged.tag.to_string();
            let inner = yaml_to_json(tagged.value);
            Value::Object(serde_json::Map::from_iter([(tag, inner)]))
        }
    }
}

fn parse_yaml(raw: &str) -> Result<Value, String> {
    // Always go through serde_yaml::Value so that YAML tags (!Ref, !Sub, …)
    // and non-string keys are handled rather than causing a deserialisation
    // error when mapping to serde_json::Value.
    let y: serde_yaml::Value =
        serde_yaml::from_str(raw).map_err(|e| format!("YAML error: {}", e))?;
    Ok(yaml_to_json(y))
}

fn serialize_yaml(value: &Value) -> String {
    serde_yaml::to_string(value)
        .unwrap_or_else(|e| format!("# serialization error: {}", e))
}

/// Simple YAML path→line mapping.
///
/// Strategy: for each path segment, look for `{indent}key:` at the expected
/// indentation level, searching forward from the previous match.
/// Array indices are found by counting `- ` lines at the right indent depth.
fn yaml_line_for_path(raw: &str, path: &str) -> Option<usize> {
    if path.is_empty() {
        return Some(0);
    }
    let parts = path_segments(path);
    if parts.is_empty() {
        return Some(0);
    }
    let lines: Vec<&str> = raw.lines().collect();
    let mut from_line = 0usize;

    for (depth_idx, part) in parts.iter().enumerate() {
        let indent_count = depth_idx * 2; // serde_yaml uses 2-space indent

        if let Ok(arr_idx) = part.parse::<usize>() {
            // Array element: count `- ` lines at exactly this indent.
            let prefix = format!("{}- ", " ".repeat(indent_count));
            let mut count = 0usize;
            let mut found = None;
            for li in from_line..lines.len() {
                if lines[li].starts_with(&prefix) {
                    if count == arr_idx {
                        found = Some(li);
                        break;
                    }
                    count += 1;
                }
            }
            from_line = found?;
        } else {
            // Object key: find `{indent}key:` starting from from_line.
            let key_str = format!("{}{}:", " ".repeat(indent_count), part);
            let found = lines[from_line..]
                .iter()
                .enumerate()
                .find(|(_, line)| {
                    line.trim_start_matches(' ').starts_with(&format!("{}:", part))
                        && line.len() - line.trim_start_matches(' ').len() == indent_count
                })
                .map(|(i, _)| from_line + i);
            let _ = key_str; // pattern built above used for clarity
            from_line = found?;
        }
    }
    Some(from_line)
}

// ── TOML codec ────────────────────────────────────────────────────────────────

fn parse_toml(raw: &str) -> Result<Value, String> {
    let toml_val: toml::Value = raw
        .parse()
        .map_err(|e: toml::de::Error| format!("TOML error: {}", e))?;
    // `serde_json::to_value` converts via toml::Value's serde::Serialize impl.
    serde_json::to_value(toml_val).map_err(|e| format!("TOML conversion error: {}", e))
}

fn serialize_toml(value: &Value) -> String {
    // Convert serde_json::Value → toml::Value by deserializing from the JSON
    // value using toml::Value's serde::Deserialize impl.
    match serde_json::from_value::<toml::Value>(value.clone()) {
        Ok(tv) => toml::to_string_pretty(&tv)
            .unwrap_or_else(|e| format!("# serialization error: {}", e)),
        Err(e) => format!("# TOML conversion error: {}", e),
    }
}

/// Simple TOML path→line mapping.
///
/// Top-level keys are found as `key = ` at column 0.  Nested tables are
/// found as `[parent.child]` section headers.  Array indices scan `[[table]]`
/// or inline arrays.
///
/// This is a best-effort heuristic; complex TOML layouts may not map
/// perfectly.
fn toml_line_for_path(raw: &str, path: &str) -> Option<usize> {
    if path.is_empty() {
        return Some(0);
    }
    let parts = path_segments(path);
    if parts.is_empty() {
        return Some(0);
    }
    let lines: Vec<&str> = raw.lines().collect();

    // For a simple top-level key, search for `key = ` at the start of a line.
    if parts.len() == 1 {
        let needle = format!("{} ", parts[0]);
        return lines
            .iter()
            .position(|l| l.starts_with(&needle) || l.starts_with(&format!("{}=", parts[0])));
    }

    // For nested paths, look for a `[section]` or `[[section]]` header.
    let section = parts[..parts.len() - 1].join(".");
    let table_header = format!("[{}]", section);
    let array_header = format!("[[{}]]", section);

    let section_line = lines.iter().position(|l| {
        let trimmed = l.trim();
        trimmed == table_header || trimmed == array_header
    });

    if let Some(start) = section_line {
        let last_key = &parts[parts.len() - 1];
        let needle = format!("{} ", last_key);
        for (i, line) in lines[start..].iter().enumerate() {
            if line.starts_with(&needle) || line.starts_with(&format!("{}=", last_key)) {
                return Some(start + i);
            }
            // Stop at the next section header.
            if i > 0 && (line.starts_with('[') || line.starts_with("[[")) {
                break;
            }
        }
    }
    None
}

// ── XML codec ────────────────────────────────────────────────────────────────
//
// Convention (BadgerFish-inspired):
//   • An element with only text content       → String value
//   • An element with child elements          → Object (keys = child tag names)
//   • Multiple siblings with the same tag     → Array (coerced automatically)
//   • XML attributes                          → `@attr` keys inside the Object
//   • Mixed text + children                   → Object with `$text` key

fn parse_xml(raw: &str) -> Result<Value, String> {
    let doc = roxmltree::Document::parse(raw).map_err(|e| format!("XML error: {}", e))?;
    let root = doc.root_element();
    let val = element_to_value(root);
    // Wrap in an object keyed by the root element's tag name.
    let mut map = serde_json::Map::new();
    map.insert(root.tag_name().name().to_string(), val);
    Ok(Value::Object(map))
}

fn element_to_value(node: roxmltree::Node) -> Value {
    let mut map = serde_json::Map::new();

    // Attributes → `@key` entries.
    for attr in node.attributes() {
        map.insert(
            format!("@{}", attr.name()),
            Value::String(attr.value().to_string()),
        );
    }

    // Children.
    let mut text_content = String::new();
    for child in node.children() {
        if child.is_element() {
            let tag = child.tag_name().name().to_string();
            let child_val = element_to_value(child);
            merge_xml_child(&mut map, tag, child_val);
        } else if child.is_text() {
            if let Some(t) = child.text() {
                text_content.push_str(t);
            }
        }
    }

    let trimmed_text = text_content.trim().to_string();

    if map.is_empty() {
        // Pure text node (or empty element).
        Value::String(trimmed_text)
    } else {
        // Has child elements; stash any text under `$text`.
        if !trimmed_text.is_empty() {
            map.insert("$text".to_string(), Value::String(trimmed_text));
        }
        Value::Object(map)
    }
}

/// When a child is added to a map, handle duplicate tag names by coercing to
/// an Array (e.g. multiple `<item>` siblings become `["a", "b", …]`).
fn merge_xml_child(map: &mut serde_json::Map<String, Value>, key: String, new_val: Value) {
    match map.get_mut(&key) {
        Some(Value::Array(arr)) => arr.push(new_val),
        Some(existing) => {
            let old = std::mem::replace(existing, Value::Null);
            *existing = Value::Array(vec![old, new_val]);
        }
        None => {
            map.insert(key, new_val);
        }
    }
}

fn serialize_xml(value: &Value) -> String {
    let mut buf = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    match value {
        Value::Object(map) if map.len() == 1 => {
            // Single-key object: use the key as the root element name.
            let (tag, inner) = map.iter().next().unwrap();
            value_to_xml(inner, tag, 0, &mut buf);
        }
        _ => {
            // Fallback: wrap everything in a generic root element.
            value_to_xml(value, "root", 0, &mut buf);
        }
    }
    buf
}

fn value_to_xml(value: &Value, tag: &str, depth: usize, buf: &mut String) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(map) => {
            // Collect attributes (keys starting with `@`) and child elements.
            let attr_str: String = map
                .iter()
                .filter_map(|(k, v)| {
                    if k.starts_with('@') {
                        if let Value::String(s) = v {
                            Some(format!(" {}=\"{}\"", &k[1..], xml_escape(s)))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            let children: Vec<_> = map.iter().filter(|(k, _)| !k.starts_with('@')).collect();

            if children.is_empty() {
                buf.push_str(&format!("{}<{}{}/>\n", indent, tag, attr_str));
            } else {
                buf.push_str(&format!("{}<{}{}>\n", indent, tag, attr_str));
                for (k, v) in &children {
                    if *k == "$text" {
                        if let Value::String(s) = v {
                            buf.push_str(&format!("{}  {}\n", indent, xml_escape(s)));
                        }
                    } else {
                        value_to_xml(v, k, depth + 1, buf);
                    }
                }
                buf.push_str(&format!("{}</{}>\n", indent, tag));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                value_to_xml(item, tag, depth, buf);
            }
        }
        Value::String(s) => {
            buf.push_str(&format!("{}<{}>{}</{}>\n", indent, tag, xml_escape(s), tag));
        }
        Value::Number(n) => {
            buf.push_str(&format!("{}<{}>{}</{}>\n", indent, tag, n, tag));
        }
        Value::Bool(b) => {
            buf.push_str(&format!("{}<{}>{}</{}>\n", indent, tag, b, tag));
        }
        Value::Null => {
            buf.push_str(&format!("{}<{}/>\n", indent, tag));
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
