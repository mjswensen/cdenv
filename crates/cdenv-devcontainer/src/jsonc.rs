//! Strict, bounded JSONC parsing with stable source diagnostics.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str;

use jsonc_parser::ast::{Object, Value as AstValue};
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};
use serde_json::Value;
use thiserror::Error;

use crate::{ConfigPath, PROFILE_REVISION};

/// Default maximum configuration size: one mebibyte.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Default maximum JSON container nesting.
pub const MAX_NESTING_DEPTH: usize = 64;
/// Default maximum entries in any array.
pub const MAX_ARRAY_ITEMS: usize = 4096;
/// Default maximum entries in any object.
pub const MAX_OBJECT_ITEMS: usize = 4096;
/// Default maximum decoded UTF-8 bytes in a string or object key.
pub const MAX_STRING_BYTES: usize = 256 * 1024;
/// Default maximum concurrent entries in a lifecycle command object.
pub const MAX_LIFECYCLE_GROUP_ITEMS: usize = 4096;

const LIFECYCLE_PROPERTIES: &[&str] = &[
    "initializeCommand",
    "onCreateCommand",
    "updateContentCommand",
    "postCreateCommand",
    "postStartCommand",
    "postAttachCommand",
];

/// Resource limits applied before a raw document is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    /// Maximum source bytes.
    pub file_bytes: usize,
    /// Maximum object/array nesting, counting the root container as one.
    pub nesting_depth: usize,
    /// Maximum entries in one array.
    pub array_items: usize,
    /// Maximum entries in one object.
    pub object_items: usize,
    /// Maximum decoded bytes in one string or key.
    pub string_bytes: usize,
    /// Maximum entries in one top-level lifecycle command object.
    pub lifecycle_group_items: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_CONFIG_BYTES,
            nesting_depth: MAX_NESTING_DEPTH,
            array_items: MAX_ARRAY_ITEMS,
            object_items: MAX_OBJECT_ITEMS,
            string_bytes: MAX_STRING_BYTES,
            lifecycle_group_items: MAX_LIFECYCLE_GROUP_ITEMS,
        }
    }
}

/// A half-open byte span with one-indexed display coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    /// First source byte.
    pub start: usize,
    /// Byte after the diagnostic range.
    pub end: usize,
    /// One-indexed starting line.
    pub line: usize,
    /// One-indexed starting Unicode-scalar column.
    pub column: usize,
}

/// A stable profile-aware source diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Selected repository-relative file.
    pub file: ConfigPath,
    /// JSON property path, or `$` for a syntax-level diagnostic.
    pub property_path: String,
    /// Exact source location.
    pub span: SourceSpan,
    /// Human-readable detail without source contents.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}: {} at `{}` ({PROFILE_REVISION})",
            self.file, self.span.line, self.span.column, self.message, self.property_path
        )
    }
}

/// The resource category exceeded by a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundKind {
    /// Source byte count.
    FileBytes,
    /// Container nesting.
    NestingDepth,
    /// Array entries.
    ArrayItems,
    /// Object entries.
    ObjectItems,
    /// Decoded string bytes.
    StringBytes,
    /// Decoded object-key bytes.
    KeyBytes,
    /// Concurrent lifecycle commands.
    LifecycleGroupItems,
}

/// A strict JSONC parsing failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsoncError {
    /// A configured resource bound was exceeded.
    #[error("{diagnostic}")]
    Bound {
        /// Exceeded category.
        kind: BoundKind,
        /// Configured maximum.
        limit: usize,
        /// Located diagnostic.
        diagnostic: Diagnostic,
    },
    /// Input was not UTF-8.
    #[error("{diagnostic}")]
    Utf8 {
        /// Located diagnostic.
        diagnostic: Diagnostic,
    },
    /// JSONC syntax was malformed or used a disabled extension.
    #[error("{diagnostic}")]
    Syntax {
        /// Located diagnostic.
        diagnostic: Diagnostic,
    },
    /// An object repeated a key.
    #[error("{diagnostic}")]
    DuplicateKey {
        /// Located diagnostic for the later key.
        diagnostic: Diagnostic,
    },
}

impl JsoncError {
    /// Returns the stable diagnostic carried by every parse failure.
    #[must_use]
    pub const fn diagnostic(&self) -> &Diagnostic {
        match self {
            Self::Bound { diagnostic, .. }
            | Self::Utf8 { diagnostic }
            | Self::Syntax { diagnostic }
            | Self::DuplicateKey { diagnostic } => diagnostic,
        }
    }
}

/// A parsed, bounded raw document awaiting profile validation.
#[derive(Clone, Debug, PartialEq)]
pub struct RawDocument {
    path: ConfigPath,
    value: Value,
    property_spans: BTreeMap<String, SourceSpan>,
    diagnostics: Vec<Diagnostic>,
}

impl RawDocument {
    /// Returns the selected source path.
    #[must_use]
    pub const fn path(&self) -> &ConfigPath {
        &self.path
    }

    /// Returns the unmerged, unsubstituted JSON value.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }

    /// Returns the source span for a property/value path when one exists.
    #[must_use]
    pub fn property_span(&self, property_path: &str) -> Option<SourceSpan> {
        self.property_spans.get(property_path).copied()
    }

    /// Returns non-fatal parse diagnostics. Strict V1 parsing currently emits none.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Parses comments-capable JSONC while rejecting all other JSON extensions.
///
/// This operation is pure: it performs no filesystem, network, Docker, or
/// subprocess access. It does not validate profile properties, merge metadata,
/// or substitute variables.
///
/// # Errors
///
/// Returns [`JsoncError`] for oversized/non-UTF-8 input, malformed syntax,
/// trailing commas, duplicate keys, or any configured structural bound.
pub fn parse_jsonc(
    path: &ConfigPath,
    bytes: &[u8],
    limits: ParseLimits,
) -> Result<RawDocument, JsoncError> {
    if bytes.len() > limits.file_bytes {
        return Err(bound_error(
            path,
            "$",
            SourceSpan {
                start: limits.file_bytes.min(bytes.len()),
                end: limits.file_bytes.min(bytes.len()),
                line: 1,
                column: 1,
            },
            BoundKind::FileBytes,
            limits.file_bytes,
        ));
    }
    let text = str::from_utf8(bytes).map_err(|error| {
        let index = error.valid_up_to();
        JsoncError::Utf8 {
            diagnostic: Diagnostic {
                file: path.clone(),
                property_path: "$".to_owned(),
                span: SourceSpan {
                    start: index,
                    end: (index + error.error_len().unwrap_or(0)).min(bytes.len()),
                    line: 1,
                    column: index + 1,
                },
                message: "configuration is not UTF-8".to_owned(),
            },
        }
    })?;
    let lines = LineIndex::new(text);
    let options = ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    };
    let parsed = parse_to_ast(text, &CollectOptions::default(), &options).map_err(|error| {
        JsoncError::Syntax {
            diagnostic: Diagnostic {
                file: path.clone(),
                property_path: "$".to_owned(),
                span: lines.span(error.range().start, error.range().end),
                message: error.kind().to_string(),
            },
        }
    })?;
    let ast = parsed.value.ok_or_else(|| JsoncError::Syntax {
        diagnostic: Diagnostic {
            file: path.clone(),
            property_path: "$".to_owned(),
            span: lines.span(0, 0),
            message: "configuration is empty".to_owned(),
        },
    })?;
    let mut spans = BTreeMap::new();
    check_value(path, &ast, "$", 0, &lines, limits, &mut spans)?;
    Ok(RawDocument {
        path: path.clone(),
        value: Value::from(ast),
        property_spans: spans,
        diagnostics: Vec::new(),
    })
}

fn check_value(
    file: &ConfigPath,
    value: &AstValue<'_>,
    path: &str,
    parent_depth: usize,
    lines: &LineIndex,
    limits: ParseLimits,
    spans: &mut BTreeMap<String, SourceSpan>,
) -> Result<(), JsoncError> {
    let range = ast_range(value);
    spans.insert(path.to_owned(), lines.span(range.start, range.end));
    let depth =
        parent_depth + usize::from(matches!(value, AstValue::Object(_) | AstValue::Array(_)));
    if depth > limits.nesting_depth {
        return Err(bound_error(
            file,
            path,
            lines.span(range.start, range.end),
            BoundKind::NestingDepth,
            limits.nesting_depth,
        ));
    }
    match value {
        AstValue::StringLit(string) if string.value.len() > limits.string_bytes => {
            Err(bound_error(
                file,
                path,
                lines.span(string.range.start, string.range.end),
                BoundKind::StringBytes,
                limits.string_bytes,
            ))
        }
        AstValue::Array(array) if array.elements.len() > limits.array_items => Err(bound_error(
            file,
            path,
            lines.span(array.range.start, array.range.end),
            BoundKind::ArrayItems,
            limits.array_items,
        )),
        AstValue::Object(object) if object.properties.len() > limits.object_items => {
            Err(bound_error(
                file,
                path,
                lines.span(object.range.start, object.range.end),
                BoundKind::ObjectItems,
                limits.object_items,
            ))
        }
        AstValue::Array(array) => {
            for (index, child) in array.elements.iter().enumerate() {
                check_value(
                    file,
                    child,
                    &format!("{path}[{index}]"),
                    depth,
                    lines,
                    limits,
                    spans,
                )?;
            }
            Ok(())
        }
        AstValue::Object(object) => check_object(file, object, path, depth, lines, limits, spans),
        _ => Ok(()),
    }
}

fn check_object(
    file: &ConfigPath,
    object: &Object<'_>,
    path: &str,
    depth: usize,
    lines: &LineIndex,
    limits: ParseLimits,
    spans: &mut BTreeMap<String, SourceSpan>,
) -> Result<(), JsoncError> {
    if path == "$" {
        for property in &object.properties {
            if LIFECYCLE_PROPERTIES.contains(&property.name.as_str())
                && let AstValue::Object(group) = &property.value
                && group.properties.len() > limits.lifecycle_group_items
            {
                let property_path = child_path(path, property.name.as_str());
                return Err(bound_error(
                    file,
                    &property_path,
                    lines.span(group.range.start, group.range.end),
                    BoundKind::LifecycleGroupItems,
                    limits.lifecycle_group_items,
                ));
            }
        }
    }
    let mut names = BTreeSet::new();
    for property in &object.properties {
        let name = property.name.as_str();
        let property_path = child_path(path, name);
        if name.len() > limits.string_bytes {
            return Err(bound_error(
                file,
                path,
                lines.span(property.range.start, property.range.end),
                BoundKind::KeyBytes,
                limits.string_bytes,
            ));
        }
        if !names.insert(name) {
            return Err(JsoncError::DuplicateKey {
                diagnostic: Diagnostic {
                    file: file.clone(),
                    property_path,
                    span: lines.span(property.range.start, property.range.end),
                    message: format!("duplicate object key `{name}`"),
                },
            });
        }
        check_value(
            file,
            &property.value,
            &property_path,
            depth,
            lines,
            limits,
            spans,
        )?;
    }
    Ok(())
}

fn child_path(parent: &str, name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        format!("{parent}.{name}")
    } else {
        format!("{parent}[{}]", Value::String(name.to_owned()))
    }
}

fn ast_range(value: &AstValue<'_>) -> jsonc_parser::common::Range {
    match value {
        AstValue::StringLit(node) => node.range,
        AstValue::NumberLit(node) => node.range,
        AstValue::BooleanLit(node) => node.range,
        AstValue::Object(node) => node.range,
        AstValue::Array(node) => node.range,
        AstValue::NullKeyword(node) => node.range,
    }
}

fn bound_error(
    file: &ConfigPath,
    property_path: &str,
    span: SourceSpan,
    kind: BoundKind,
    limit: usize,
) -> JsoncError {
    JsoncError::Bound {
        kind,
        limit,
        diagnostic: Diagnostic {
            file: file.clone(),
            property_path: property_path.to_owned(),
            span,
            message: format!("{kind:?} exceeds configured limit {limit}"),
        },
    }
}

struct LineIndex {
    starts: Vec<usize>,
    scalar_prefix: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        let mut scalar_prefix = vec![0; text.len() + 1];
        let mut scalar_count = 0_usize;
        for (start, character) in text.char_indices() {
            for offset in 0..character.len_utf8() {
                scalar_prefix[start + offset] = scalar_count;
            }
            scalar_count += 1;
        }
        scalar_prefix[text.len()] = scalar_count;
        Self {
            starts,
            scalar_prefix,
        }
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        let line_index = self
            .starts
            .partition_point(|line_start| *line_start <= start)
            - 1;
        let line_start = self.starts[line_index];
        SourceSpan {
            start,
            end,
            line: line_index + 1,
            column: self.scalar_prefix[start] - self.scalar_prefix[line_start] + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> ConfigPath {
        ConfigPath::parse(".devcontainer/devcontainer.json").expect("fixture path should be valid")
    }

    #[test]
    fn comments_are_accepted() {
        let document = parse_jsonc(
            &path(),
            b"{ // image selection\n \"image\": \"debian:13\"\n}",
            ParseLimits::default(),
        )
        .expect("comments should parse");
        assert_eq!(document.value()["image"], "debian:13");
    }

    #[test]
    fn trailing_commas_are_rejected_with_file_location_and_revision() {
        let error = parse_jsonc(
            &path(),
            b"{\n  \"image\": \"debian:13\",\n}",
            ParseLimits::default(),
        )
        .expect_err("trailing comma should fail");
        assert!(matches!(&error, JsoncError::Syntax { .. }));
        assert_eq!(
            (
                error.diagnostic().span.line,
                error.diagnostic().file.as_str()
            ),
            (2, ".devcontainer/devcontainer.json")
        );
        assert!(error.to_string().contains(PROFILE_REVISION));
    }

    #[test]
    fn malformed_tokens_have_stable_source_spans() {
        let error = parse_jsonc(&path(), b"{\n \"image\": @\n}", ParseLimits::default())
            .expect_err("invalid token should fail");
        assert_eq!(
            (error.diagnostic().span.line, error.diagnostic().span.column),
            (2, 11)
        );
    }

    #[test]
    fn duplicate_keys_are_rejected_at_the_property_path() {
        let error = parse_jsonc(
            &path(),
            br#"{"image":"a","image":"b"}"#,
            ParseLimits::default(),
        )
        .expect_err("duplicate key should fail");
        assert!(matches!(&error, JsoncError::DuplicateKey { .. }));
        assert_eq!(error.diagnostic().property_path, "$.image");
    }

    #[test]
    fn file_byte_bound_is_enforced() {
        let limits = ParseLimits {
            file_bytes: 2,
            ..ParseLimits::default()
        };
        let error = parse_jsonc(&path(), b"{} ", limits).expect_err("oversized input should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::FileBytes,
                limit: 2,
                ..
            }
        ));
    }

    #[test]
    fn nesting_bound_is_enforced() {
        let limits = ParseLimits {
            nesting_depth: 2,
            ..ParseLimits::default()
        };
        let error =
            parse_jsonc(&path(), br#"{"a":{"b":[]}}"#, limits).expect_err("deep input should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::NestingDepth,
                limit: 2,
                ..
            }
        ));
    }

    #[test]
    fn string_bound_is_enforced() {
        let limits = ParseLimits {
            string_bytes: 3,
            ..ParseLimits::default()
        };
        let error =
            parse_jsonc(&path(), br#"{"a":"four"}"#, limits).expect_err("long string should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::StringBytes,
                limit: 3,
                ..
            }
        ));
    }

    #[test]
    fn key_bound_is_enforced() {
        let limits = ParseLimits {
            string_bytes: 3,
            ..ParseLimits::default()
        };
        let error =
            parse_jsonc(&path(), br#"{"four":1}"#, limits).expect_err("long key should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::KeyBytes,
                limit: 3,
                ..
            }
        ));
    }

    #[test]
    fn array_bound_is_enforced() {
        let limits = ParseLimits {
            array_items: 1,
            ..ParseLimits::default()
        };
        let error =
            parse_jsonc(&path(), br#"{"a":[1,2]}"#, limits).expect_err("long array should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::ArrayItems,
                limit: 1,
                ..
            }
        ));
    }

    #[test]
    fn object_bound_is_enforced() {
        let limits = ParseLimits {
            object_items: 1,
            ..ParseLimits::default()
        };
        let error = parse_jsonc(&path(), br#"{"a":1,"b":2}"#, limits)
            .expect_err("large object should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::ObjectItems,
                limit: 1,
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_group_bound_is_enforced_separately() {
        let limits = ParseLimits {
            lifecycle_group_items: 1,
            ..ParseLimits::default()
        };
        let error = parse_jsonc(
            &path(),
            br#"{"postCreateCommand":{"a":"one","b":"two"}}"#,
            limits,
        )
        .expect_err("large lifecycle group should fail");
        assert!(matches!(
            error,
            JsoncError::Bound {
                kind: BoundKind::LifecycleGroupItems,
                limit: 1,
                ..
            }
        ));
    }

    #[test]
    fn property_spans_are_retained_on_raw_document() {
        let document = parse_jsonc(
            &path(),
            b"{\n  \"image\": \"debian:13\"\n}",
            ParseLimits::default(),
        )
        .expect("document should parse");
        assert_eq!(
            document
                .property_span("$.image")
                .map(|span| (span.line, span.column)),
            Some((2, 12))
        );
    }
}
