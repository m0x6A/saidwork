use async_trait::async_trait;

use crate::domain::{Format, FormatId};
use crate::ports::{FormatSelector, SelectionError};

/// Selector that returns a format chosen up front by the user.
#[derive(Debug, Clone)]
pub struct ManualSelector {
    chosen: FormatId,
}

impl ManualSelector {
    #[must_use]
    pub fn new(chosen: FormatId) -> Self {
        Self { chosen }
    }
}

#[async_trait]
impl FormatSelector for ManualSelector {
    async fn select(
        &self,
        candidates: &[Format],
        _transcript: &str,
    ) -> Result<FormatId, SelectionError> {
        if candidates.is_empty() {
            return Err(SelectionError::NoCandidates);
        }
        if candidates.iter().any(|f| f.id == self.chosen) {
            Ok(self.chosen.clone())
        } else {
            Err(SelectionError::NotACandidate {
                id: self.chosen.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(id: &str) -> Format {
        Format {
            id: FormatId::new(id).unwrap(),
            name: id.to_owned(),
            description: String::new(),
            when_to_use: None,
            template: "x".to_owned(),
        }
    }

    #[tokio::test]
    async fn returns_the_chosen_format_when_present() {
        let sel = ManualSelector::new(FormatId::new("b").unwrap());
        let id = sel.select(&[format("a"), format("b")], "").await.unwrap();
        assert_eq!(id.as_str(), "b");
    }

    #[tokio::test]
    async fn errors_when_chosen_format_is_absent() {
        let sel = ManualSelector::new(FormatId::new("zzz").unwrap());
        let err = sel.select(&[format("a")], "").await.unwrap_err();
        assert!(matches!(err, SelectionError::NotACandidate { .. }));
    }

    #[tokio::test]
    async fn errors_without_candidates() {
        let sel = ManualSelector::new(FormatId::new("a").unwrap());
        assert!(matches!(
            sel.select(&[], "").await.unwrap_err(),
            SelectionError::NoCandidates
        ));
    }
}
