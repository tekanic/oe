//! Synchronization layer between the raw text buffer and the JSON tree.
//!
//! Three operations:
//!
//! * `raw_to_tree` — parse the raw string using the active `FileFormat`; on
//!   success produce a new `JsonTree`, preserving the previous collapse state.
//! * `tree_to_raw` — serialize the current `JsonTree` root back to formatted
//!   text using the active `FileFormat`.
//! * `find_line_for_path` — delegate to `FileFormat::line_for_path`.

use std::collections::HashSet;

use crate::format::FileFormat;
use crate::tree::JsonTree;

/// Result of attempting to parse the raw text buffer.
pub enum ParseResult {
    /// Parsing succeeded.  Contains the new tree.
    Ok(JsonTree),
    /// Parsing failed.  Contains a human-readable error string.
    Err(String),
}

/// Parse `raw` using `format`.  On success, return a new `JsonTree` with the
/// previous collapse state transplanted.  On failure, return an error message
/// suitable for the status bar.
pub fn raw_to_tree(raw: &str, previous_collapsed: HashSet<String>, format: FileFormat) -> ParseResult {
    match format.parse(raw) {
        std::result::Result::Ok(value) => ParseResult::Ok(JsonTree::from_value(value, previous_collapsed)),
        std::result::Result::Err(msg) => ParseResult::Err(msg),
    }
}

/// Serialize the tree root back to formatted text using `format`.
///
/// Called after every tree-pane mutation (add / rename / delete / edit).
pub fn tree_to_raw(tree: &JsonTree, format: FileFormat) -> String {
    format.serialize(&tree.root)
}

/// Find the 0-based line number in `raw` that corresponds to `path`.
/// Delegates to the format-specific implementation in `format.rs`.
pub fn find_line_for_path(raw: &str, path: &str, format: FileFormat) -> Option<usize> {
    format.line_for_path(raw, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn parse_valid_json() {
        let raw = r#"{"hello": "world"}"#;
        let result = raw_to_tree(raw, HashSet::new(), FileFormat::Json);
        assert!(matches!(result, ParseResult::Ok(_)));
    }

    #[test]
    fn parse_invalid_json() {
        let raw = r#"{"hello": }"#;
        let result = raw_to_tree(raw, HashSet::new(), FileFormat::Json);
        assert!(matches!(result, ParseResult::Err(_)));
    }

    #[test]
    fn roundtrip_json() {
        let raw = "{\n  \"a\": 1,\n  \"b\": \"hi\"\n}";
        let ParseResult::Ok(tree) = raw_to_tree(raw, HashSet::new(), FileFormat::Json) else {
            panic!("parse failed");
        };
        let out = tree_to_raw(&tree, FileFormat::Json);
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
    }

    #[test]
    fn roundtrip_yaml() {
        let raw = "a: 1\nb: hello\n";
        let ParseResult::Ok(tree) = raw_to_tree(raw, HashSet::new(), FileFormat::Yaml) else {
            panic!("yaml parse failed");
        };
        let out = tree_to_raw(&tree, FileFormat::Yaml);
        // Re-parse to confirm valid YAML.
        let result = raw_to_tree(&out, HashSet::new(), FileFormat::Yaml);
        assert!(matches!(result, ParseResult::Ok(_)));
    }

    /// CloudFormation templates use YAML tags (!Ref, !Sub, !Equals, etc.).
    /// These were previously rejected with "invalid type: enum" when trying
    /// to deserialize directly into serde_json::Value.
    #[test]
    fn parse_yaml_with_tags() {
        let cfn = "\
AWSTemplateFormatVersion: '2010-09-09'
Parameters:
  Environment:
    Type: String
Conditions:
  IsProduction: !Equals [!Ref Environment, production]
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Sub '${Environment}-bucket'
";
        let result = raw_to_tree(cfn, HashSet::new(), FileFormat::Yaml);
        assert!(
            matches!(result, ParseResult::Ok(_)),
            "CloudFormation YAML with !Ref/!Sub/!Equals tags should parse"
        );
    }

    #[test]
    fn roundtrip_toml() {
        let raw = "a = 1\nb = \"hello\"\n";
        let ParseResult::Ok(tree) = raw_to_tree(raw, HashSet::new(), FileFormat::Toml) else {
            panic!("toml parse failed");
        };
        let out = tree_to_raw(&tree, FileFormat::Toml);
        let result = raw_to_tree(&out, HashSet::new(), FileFormat::Toml);
        assert!(matches!(result, ParseResult::Ok(_)));
    }
}
