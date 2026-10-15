use std::fmt;

use serde::Deserialize;

/// Identifier of a format: the file stem of its template, e.g. `meeting-notes`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FormatId(String);

impl FormatId {
    /// Build an id, normalising to lowercase kebab-case friendly form.
    ///
    /// Only the characters `a-z`, `0-9`, `-` and `_` are kept; whitespace becomes `-`.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, FormatError> {
        let id: String = raw
            .as_ref()
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_whitespace() { '-' } else { c })
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.is_empty() {
            return Err(FormatError::EmptyId);
        }
        Ok(Self(id))
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FormatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A user-defined Markdown output format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    /// Stable identifier (file stem).
    pub id: FormatId,
    /// Human readable name shown in menus.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Guidance for choosing this format automatically; consumed by a future LLM selector.
    pub when_to_use: Option<String>,
    /// The Markdown template / instructions handed to the rewriter.
    pub template: String,
}

/// Errors while parsing or loading a format.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("format id must contain at least one alphanumeric character")]
    EmptyId,
    #[error("format `{id}` is missing its YAML frontmatter block (`---` ... `---`)")]
    MissingFrontmatter { id: String },
    #[error("format `{id}` has invalid frontmatter: {source}")]
    InvalidFrontmatter {
        id: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("format `{id}` has an empty template body")]
    EmptyTemplate { id: String },
    #[error("format `{id}` not found")]
    NotFound { id: String },
    #[error("could not read formats from `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(default)]
    when_to_use: Option<String>,
}

/// Parse a Markdown document with YAML frontmatter into a [`Format`].
///
/// The document must start with a `---` line, followed by YAML, followed by a
/// closing `---` line; everything after is the template. CRLF line endings are accepted.
pub fn parse_format(id: FormatId, source: &str) -> Result<Format, FormatError> {
    let (yaml, body) = split_frontmatter(source)
        .ok_or_else(|| FormatError::MissingFrontmatter { id: id.to_string() })?;
    let meta: Frontmatter =
        serde_yaml_ng::from_str(yaml).map_err(|source| FormatError::InvalidFrontmatter {
            id: id.to_string(),
            source,
        })?;
    let template = body.trim().to_owned();
    if template.is_empty() {
        return Err(FormatError::EmptyTemplate { id: id.to_string() });
    }
    Ok(Format {
        id,
        name: meta.name.trim().to_owned(),
        description: meta.description.trim().to_owned(),
        when_to_use: meta
            .when_to_use
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        template,
    })
}

/// Split `---\nyaml\n---\nbody` into `(yaml, body)`.
fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let yaml_start = first.len();
    let mut offset = yaml_start;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &source[yaml_start..offset];
            let body = &source[offset + line.len()..];
            return Some((yaml, body));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> FormatId {
        FormatId::new(s).unwrap()
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let src = "---\nname: Meeting\ndescription: Minutes\nwhen_to_use: several people talk\n---\n# Title\n\nBody\n";
        let f = parse_format(id("meeting"), src).unwrap();
        assert_eq!(f.name, "Meeting");
        assert_eq!(f.description, "Minutes");
        assert_eq!(f.when_to_use.as_deref(), Some("several people talk"));
        assert_eq!(f.template, "# Title\n\nBody");
    }

    #[test]
    fn when_to_use_is_optional() {
        let src = "---\nname: A\ndescription: B\n---\nbody";
        let f = parse_format(id("a"), src).unwrap();
        assert_eq!(f.when_to_use, None);
    }

    #[test]
    fn accepts_crlf_and_bom() {
        let src = "\u{feff}---\r\nname: A\r\ndescription: B\r\n---\r\nbody\r\n";
        let f = parse_format(id("a"), src).unwrap();
        assert_eq!(f.name, "A");
        assert_eq!(f.template, "body");
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let err = parse_format(id("a"), "# just markdown").unwrap_err();
        assert!(
            matches!(err, FormatError::MissingFrontmatter { .. }),
            "{err}"
        );
    }

    #[test]
    fn rejects_unterminated_frontmatter() {
        let err = parse_format(id("a"), "---\nname: A\n").unwrap_err();
        assert!(
            matches!(err, FormatError::MissingFrontmatter { .. }),
            "{err}"
        );
    }

    #[test]
    fn rejects_missing_required_fields() {
        let err = parse_format(id("a"), "---\nname: A\n---\nbody").unwrap_err();
        assert!(
            matches!(err, FormatError::InvalidFrontmatter { .. }),
            "{err}"
        );
    }

    #[test]
    fn rejects_empty_body() {
        let err = parse_format(id("a"), "---\nname: A\ndescription: B\n---\n\n  \n").unwrap_err();
        assert!(matches!(err, FormatError::EmptyTemplate { .. }), "{err}");
    }

    #[test]
    fn format_id_normalises() {
        assert_eq!(
            FormatId::new("  Meeting Notes ").unwrap().as_str(),
            "meeting-notes"
        );
        assert_eq!(FormatId::new("a/b.c").unwrap().as_str(), "abc");
        assert!(matches!(FormatId::new("///"), Err(FormatError::EmptyId)));
    }
}
