//! Prompt construction and response clean-up shared by every rewriter adapter.

use std::fmt::Write as _;

use crate::domain::{RewriteRequest, RewrittenNote};

/// A system/user message pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

const RULES: &str = "\
You are a meticulous editor. You receive the raw transcript of a spoken recording and must \
rewrite it into the Markdown format described below.

Rules:
- Be faithful to the transcript. Never invent facts, names, numbers or decisions that were not said.
- Remove filler words, false starts and repetition; fix grammar and punctuation.
- Write in the same language as the transcript.
- Follow the format's structure and headings exactly. Leave out a section only if the transcript \
has nothing for it, and then write \"None\" under it.
- Replace `{{date}}` with the date given below and `{{title}}` with a short descriptive title.
- Output only the finished Markdown document. No preamble, no explanations, no code fences.";

/// Build the prompt for a request.
#[must_use]
pub fn build_prompt(request: &RewriteRequest) -> Prompt {
    let mut system = format!("{RULES}\n\nDate: {}\n", request.date);
    if let Some(context) = request
        .context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        system.push_str("\nContext from the user: ");
        system.push_str(context);
        system.push('\n');
    }
    let _ = write!(
        system,
        "\n--- Format: {} ---\n{}\n--- End of format ---",
        request.format.name,
        request.format.template.trim()
    );
    let user = format!("Transcript:\n\n{}", request.transcript.trim());
    Prompt { system, user }
}

/// Clean the raw model answer: strip a surrounding code fence and pick out the title.
#[must_use]
pub fn post_process(raw: &str) -> RewrittenNote {
    let mut text = raw.trim();
    if let Some(rest) = text.strip_prefix("```") {
        // Drop the fence line (which may carry a language tag) and the closing fence.
        let rest = rest.split_once('\n').map_or("", |(_, body)| body);
        text = rest.trim_end().strip_suffix("```").unwrap_or(rest).trim();
    }
    let title = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    RewrittenNote {
        markdown: format!("{text}\n"),
        title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FormatSpec;

    fn request(context: Option<&str>) -> RewriteRequest {
        RewriteRequest {
            transcript: "  so um today we uh decided to ship friday  ".into(),
            format: FormatSpec {
                name: "Meeting notes".into(),
                template: "# {{title}}\n\n## Decisions\n- ...\n".into(),
            },
            date: "2026-10-14".into(),
            context: context.map(Into::into),
        }
    }

    #[test]
    fn prompt_snapshot() {
        insta::assert_snapshot!(build_prompt(&request(Some("weekly sync"))).system);
    }

    #[test]
    fn user_message_contains_trimmed_transcript() {
        let p = build_prompt(&request(None));
        assert_eq!(
            p.user,
            "Transcript:\n\nso um today we uh decided to ship friday"
        );
        assert!(!p.system.contains("Context from the user"));
    }

    #[test]
    fn blank_context_is_ignored() {
        assert!(
            !build_prompt(&request(Some("   ")))
                .system
                .contains("Context from the user")
        );
    }

    #[test]
    fn post_process_strips_fences_and_finds_title() {
        let note = post_process("```markdown\n# Ship Friday\n\n## Decisions\n- ship\n```\n");
        assert_eq!(note.markdown, "# Ship Friday\n\n## Decisions\n- ship\n");
        assert_eq!(note.title.as_deref(), Some("Ship Friday"));
    }

    #[test]
    fn post_process_without_fence_or_title() {
        let note = post_process("## Only a subheading\ntext");
        assert_eq!(note.markdown, "## Only a subheading\ntext\n");
        assert_eq!(note.title, None);
    }

    #[test]
    fn post_process_handles_bare_fence_without_language() {
        let note = post_process("```\n# T\n```");
        assert_eq!(note.markdown, "# T\n");
    }
}
