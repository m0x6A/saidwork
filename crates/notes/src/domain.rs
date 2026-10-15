use chrono::{DateTime, Local, Utc};

/// A finished Markdown note ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub markdown: String,
    /// Id of the format it was written in, used in the file name.
    pub format_id: String,
    /// Optional title, used to make the file name descriptive.
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Errors of the notes slice.
#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("could not write note to `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// `YYYYMMDD-HHMMSS-<format>[-<slug>].md` in local time.
#[must_use]
pub fn file_name_for(note: &Note) -> String {
    let stamp = note
        .created_at
        .with_timezone(&Local)
        .format("%Y%m%d-%H%M%S");
    let mut name = format!("{stamp}-{}", slugify(&note.format_id));
    if let Some(slug) = note.title.as_deref().map(slugify).filter(|s| !s.is_empty()) {
        name.push('-');
        name.push_str(&slug);
    }
    name.push_str(".md");
    name
}

/// Lowercase ASCII letters, digits and single dashes; at most 40 characters.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in text
        .chars()
        .flat_map(char::to_lowercase)
        .flat_map(transliterate)
    {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    out.trim_matches('-').to_owned()
}

/// Map common accented letters to ASCII so Swedish titles stay readable in file names.
fn transliterate(c: char) -> impl Iterator<Item = char> {
    let mapped: &str = match c {
        'å' | 'ä' | 'á' | 'à' | 'â' | 'ã' => "a",
        'ö' | 'ó' | 'ò' | 'ô' | 'õ' | 'ø' => "o",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'ü' | 'ú' | 'ù' | 'û' => "u",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'æ' => "ae",
        'ß' => "ss",
        'ç' => "c",
        'ñ' => "n",
        _ => return Either::Char(Some(c)),
    };
    Either::Str(mapped.chars())
}

enum Either {
    Char(Option<char>),
    Str(std::str::Chars<'static>),
}

impl Iterator for Either {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        match self {
            Self::Char(c) => c.take(),
            Self::Str(it) => it.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: Option<&str>) -> Note {
        Note {
            markdown: "# x\n".into(),
            format_id: "meeting-notes".into(),
            title: title.map(Into::into),
            created_at: DateTime::parse_from_rfc3339("2026-10-14T12:34:56Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn file_name_has_timestamp_format_and_slug() {
        let name = file_name_for(&note(Some("Weekly Sync: Ship Friday!")));
        assert!(
            name.ends_with("-meeting-notes-weekly-sync-ship-friday.md"),
            "{name}"
        );
        assert_eq!(
            name.len(),
            "20261014-123456-".len() + "meeting-notes-weekly-sync-ship-friday.md".len()
        );
    }

    #[test]
    fn file_name_without_title() {
        assert!(file_name_for(&note(None)).ends_with("-meeting-notes.md"));
        assert!(file_name_for(&note(Some("***"))).ends_with("-meeting-notes.md"));
    }

    #[test]
    fn slug_is_bounded() {
        let long = "a".repeat(100);
        assert!(slugify(&long).len() <= 40);
        assert_eq!(slugify("Héllo wörld"), "hello-world");
        assert_eq!(
            slugify("Lansering av version två på fredag"),
            "lansering-av-version-tva-pa-fredag"
        );
        assert_eq!(slugify("Ærø straße"), "aero-strasse");
    }
}
