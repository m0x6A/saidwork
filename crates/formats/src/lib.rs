//! Markdown format templates and format selection.
//!
//! A *format* is a Markdown file with YAML frontmatter describing when it is
//! suitable; its body is the template the rewriting LLM is asked to follow.
//! Formats live in the user's config directory; three built-ins are
//! materialised there on first run.

mod adapters;
mod builtin;
mod domain;
mod ports;
mod selector;

pub use adapters::{BuiltinSync, FsFormatRepository};
pub use builtin::{BUILTIN_FORMATS, NEW_FORMAT_TEMPLATE};
pub use domain::{Format, FormatError, FormatId, parse_format};
pub use ports::{FormatRepository, FormatSelector, SelectionError};
pub use selector::ManualSelector;
