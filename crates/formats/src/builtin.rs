/// Built-in format templates shipped with the binary, as `(file name, contents)`.
///
/// They are written to the user's formats directory the first time it is found empty,
/// so users can edit or delete them freely afterwards.
pub const BUILTIN_FORMATS: &[(&str, &str)] = &[
    (
        "meeting-notes.md",
        include_str!("../../../formats/meeting-notes.md"),
    ),
    (
        "journal-entry.md",
        include_str!("../../../formats/journal-entry.md"),
    ),
    (
        "task-list.md",
        include_str!("../../../formats/task-list.md"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FormatId, parse_format};

    #[test]
    fn every_builtin_parses() {
        for (file, src) in BUILTIN_FORMATS {
            let id = FormatId::new(file.trim_end_matches(".md")).unwrap();
            let f = parse_format(id, src).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(
                f.when_to_use.is_some(),
                "{file} should document when to use it"
            );
        }
    }
}
