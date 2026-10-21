use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Local;
use saidwork_capture::{CaptureError, Recording};
use saidwork_formats::{
    FormatError, FormatId, FormatRepository, FormatSelector, ManualSelector, SelectionError,
};
use saidwork_kernel::Clock;
use saidwork_notes::{Note, NoteError, NoteStore};
use saidwork_providers::{ProviderError, ProviderName, ProviderSupervisor};
use saidwork_rewriting::{FormatSpec, RewriteError, RewriteRequest, Rewriter, RewrittenNote};
use saidwork_transcription::{AudioInput, Transcriber, TranscriptionError, TranscriptionRequest};
use tokio::sync::mpsc::UnboundedSender;

use crate::events::SessionEvent;

/// Errors of a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Transcription(#[from] TranscriptionError),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error(transparent)]
    Selection(#[from] SelectionError),
    #[error(transparent)]
    Rewrite(#[from] RewriteError),
    #[error(transparent)]
    Note(#[from] NoteError),
    #[error("could not write `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the recording is silent; nothing to transcribe")]
    SilentRecording,
}

/// What a finished session produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutcome {
    pub note_path: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub recording_path: Option<PathBuf>,
    pub title: Option<String>,
    pub markdown: String,
}

/// The record → transcribe → rewrite → save flow, with its collaborators injected.
pub struct Pipeline {
    pub supervisor: Arc<ProviderSupervisor>,
    pub transcriber_name: ProviderName,
    pub rewriter_name: ProviderName,
    pub transcriber: Arc<dyn Transcriber>,
    pub rewriter: Arc<dyn Rewriter>,
    pub formats: Arc<dyn FormatRepository>,
    pub notes: Arc<dyn NoteStore>,
    pub sessions_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub language: Option<String>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("transcriber", &self.transcriber_name)
            .field("rewriter", &self.rewriter_name)
            .field("sessions_dir", &self.sessions_dir)
            .finish_non_exhaustive()
    }
}

fn emit(events: Option<&UnboundedSender<SessionEvent>>, event: SessionEvent) {
    if let Some(tx) = events {
        let _ = tx.send(event);
    }
}

impl Pipeline {
    /// Make sure both providers answer, launching them if configured. Runs the two checks concurrently.
    pub async fn prepare_providers(
        &self,
        events: Option<&UnboundedSender<SessionEvent>>,
    ) -> Result<(), SessionError> {
        let names = if self.transcriber_name == self.rewriter_name {
            vec![self.transcriber_name.clone()]
        } else {
            vec![self.transcriber_name.clone(), self.rewriter_name.clone()]
        };
        for name in &names {
            emit(events, SessionEvent::ProviderStarting(name.clone()));
        }
        let results =
            futures::future::join_all(names.iter().map(|n| self.supervisor.ensure_running(n)))
                .await;
        for (name, result) in names.into_iter().zip(results) {
            emit(events, SessionEvent::ProviderReady(name, result?));
        }
        Ok(())
    }

    /// Full processing of a finished recording.
    pub async fn process(
        &self,
        recording: &Recording,
        format: &FormatId,
        context: Option<String>,
        events: Option<&UnboundedSender<SessionEvent>>,
    ) -> Result<SessionOutcome, SessionError> {
        if recording.is_silent() {
            return Err(SessionError::SilentRecording);
        }
        let wav = recording.to_wav()?;
        let recording_path = self.sessions_dir.join(format!("{}.wav", recording.id));
        write(&recording_path, &wav)?;
        emit(events, SessionEvent::RecordingSaved(recording_path.clone()));

        emit(events, SessionEvent::Transcribing);
        // An unknown format is reported by the rewrite step; here it only means no hint.
        let vocabulary = self.formats.get(format).ok().and_then(|f| f.vocabulary);
        let transcript = self.transcribe_wav(wav, vocabulary).await?;
        let text = transcript.as_plain_text();
        let transcript_path = self.sessions_dir.join(format!("{}.txt", recording.id));
        write(&transcript_path, text.as_bytes())?;
        emit(
            events,
            SessionEvent::Transcribed {
                characters: text.chars().count(),
                path: transcript_path.clone(),
            },
        );

        let (note, note_path) = self
            .rewrite_and_save(&text, format, context, events)
            .await?;
        Ok(SessionOutcome {
            note_path,
            transcript_path: Some(transcript_path),
            recording_path: Some(recording_path),
            title: note.title,
            markdown: note.markdown,
        })
    }

    /// Transcribe WAV bytes with the configured transcriber; `vocabulary` (a format's names and
    /// terms) is passed on as the transcription prompt.
    pub async fn transcribe_wav(
        &self,
        wav: Vec<u8>,
        vocabulary: Option<String>,
    ) -> Result<saidwork_transcription::Transcript, SessionError> {
        let request = TranscriptionRequest {
            language: self.language.clone(),
            prompt: vocabulary,
        };
        Ok(self
            .transcriber
            .transcribe(&AudioInput::wav(wav), &request)
            .await?)
    }

    /// Rewrite a transcript into `format` and store the note.
    pub async fn rewrite_and_save(
        &self,
        transcript: &str,
        format: &FormatId,
        context: Option<String>,
        events: Option<&UnboundedSender<SessionEvent>>,
    ) -> Result<(RewrittenNote, PathBuf), SessionError> {
        let candidates = self.formats.list()?;
        let chosen = ManualSelector::new(format.clone())
            .select(&candidates, transcript)
            .await?;
        let format = candidates
            .into_iter()
            .find(|f| f.id == chosen)
            .ok_or_else(|| FormatError::NotFound {
                id: chosen.to_string(),
            })?;
        emit(
            events,
            SessionEvent::Rewriting {
                format: format.name.clone(),
            },
        );

        let now = self.clock.now();
        let request = RewriteRequest {
            transcript: transcript.to_owned(),
            format: FormatSpec {
                name: format.name.clone(),
                template: format.template.clone(),
            },
            date: now.with_timezone(&Local).format("%Y-%m-%d").to_string(),
            context,
        };
        let note = self.rewriter.rewrite(&request).await?;
        let path = self.notes.save(&Note {
            markdown: note.markdown.clone(),
            format_id: format.id.to_string(),
            title: note.title.clone(),
            created_at: now,
        })?;
        emit(events, SessionEvent::NoteSaved(path.clone()));
        Ok((note, path))
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), SessionError> {
    let io = |source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, bytes).map_err(io)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use saidwork_capture::{AudioSpec, RecordingId};
    use saidwork_formats::FsFormatRepository;
    use saidwork_kernel::FixedClock;
    use saidwork_notes::FsNoteStore;
    use saidwork_providers::{
        HealthProbe, LaunchSpec, ManagedProcess, ProcessLauncher, Provider, ProviderKind,
    };
    use saidwork_rewriting::RewrittenNote;
    use saidwork_transcription::Transcript;
    use url::Url;

    use super::*;

    struct UpProbe;
    #[async_trait]
    impl HealthProbe for UpProbe {
        async fn is_up(&self, _: &Url) -> bool {
            true
        }
    }

    struct NoLauncher;
    #[async_trait]
    impl ProcessLauncher for NoLauncher {
        async fn spawn(&self, spec: &LaunchSpec) -> Result<Box<dyn ManagedProcess>, ProviderError> {
            Err(ProviderError::Spawn {
                command: spec.command.clone(),
                source: std::io::Error::other("no"),
            })
        }
    }

    struct FakeTranscriber(Mutex<Vec<usize>>);
    #[async_trait]
    impl Transcriber for FakeTranscriber {
        async fn transcribe(
            &self,
            audio: &AudioInput,
            req: &TranscriptionRequest,
        ) -> Result<Transcript, TranscriptionError> {
            assert_eq!(req.language.as_deref(), Some("en"));
            self.0.lock().unwrap().push(audio.wav_bytes.len());
            Ok(Transcript {
                text: "we ship on friday".into(),
                language: Some("en".into()),
                segments: vec![],
            })
        }
    }

    struct PromptTranscriber(Mutex<Vec<Option<String>>>);
    #[async_trait]
    impl Transcriber for PromptTranscriber {
        async fn transcribe(
            &self,
            _: &AudioInput,
            req: &TranscriptionRequest,
        ) -> Result<Transcript, TranscriptionError> {
            self.0.lock().unwrap().push(req.prompt.clone());
            Ok(Transcript {
                text: "hello".into(),
                ..Transcript::default()
            })
        }
    }

    struct FailingTranscriber;
    #[async_trait]
    impl Transcriber for FailingTranscriber {
        async fn transcribe(
            &self,
            _: &AudioInput,
            _: &TranscriptionRequest,
        ) -> Result<Transcript, TranscriptionError> {
            Err(TranscriptionError::Status {
                status: 500,
                body: "down".into(),
            })
        }
    }

    struct FakeRewriter(Mutex<Option<RewriteRequest>>);
    #[async_trait]
    impl Rewriter for FakeRewriter {
        async fn rewrite(&self, req: &RewriteRequest) -> Result<RewrittenNote, RewriteError> {
            *self.0.lock().unwrap() = Some(req.clone());
            Ok(RewrittenNote {
                markdown: format!("# Ship Friday\n\n{}\n", req.transcript),
                title: Some("Ship Friday".into()),
            })
        }
    }

    fn provider(name: &str) -> Provider {
        Provider {
            name: ProviderName::new(name).unwrap(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: Url::parse("http://127.0.0.1:1").unwrap(),
            api_key: None,
            model: "m".into(),
            launch: None,
        }
    }

    fn pipeline(
        root: &Path,
        transcriber: Arc<dyn Transcriber>,
        rewriter: Arc<FakeRewriter>,
    ) -> Pipeline {
        let formats = FsFormatRepository::new(root.join("formats"));
        formats.ensure_initialised().unwrap();
        Pipeline {
            supervisor: Arc::new(ProviderSupervisor::new(
                [provider("stt"), provider("llm")],
                Arc::new(UpProbe),
                Arc::new(NoLauncher),
            )),
            transcriber_name: ProviderName::new("stt").unwrap(),
            rewriter_name: ProviderName::new("llm").unwrap(),
            transcriber,
            rewriter,
            formats: Arc::new(formats),
            notes: Arc::new(FsNoteStore::new(root.join("notes"))),
            sessions_dir: root.join("sessions"),
            clock: Arc::new(FixedClock::at("2026-10-14T12:00:00Z")),
            language: Some("en".into()),
        }
    }

    fn recording(samples: Vec<i16>) -> Recording {
        Recording {
            id: RecordingId::random(),
            started_at: chrono::Utc::now(),
            spec: AudioSpec::SPEECH,
            sources: vec![],
            samples,
        }
    }

    #[tokio::test]
    async fn processes_a_recording_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let transcriber = Arc::new(FakeTranscriber(Mutex::new(vec![])));
        let rewriter = Arc::new(FakeRewriter(Mutex::new(None)));
        let p = pipeline(tmp.path(), transcriber.clone(), rewriter.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        p.prepare_providers(Some(&tx)).await.unwrap();
        let outcome = p
            .process(
                &recording(vec![1000; 16_000]),
                &FormatId::new("meeting-notes").unwrap(),
                Some("weekly".into()),
                Some(&tx),
            )
            .await
            .unwrap();

        assert_eq!(outcome.title.as_deref(), Some("Ship Friday"));
        assert!(outcome.note_path.starts_with(tmp.path().join("notes")));
        assert_eq!(
            std::fs::read_to_string(&outcome.note_path).unwrap(),
            outcome.markdown
        );
        assert_eq!(
            std::fs::read_to_string(outcome.transcript_path.unwrap()).unwrap(),
            "we ship on friday"
        );
        assert!(outcome.recording_path.unwrap().exists());
        assert_eq!(
            transcriber.0.lock().unwrap().len(),
            1,
            "one transcription call"
        );

        let req = rewriter.0.lock().unwrap().clone().unwrap();
        assert_eq!(req.format.name, "Meeting notes");
        assert_eq!(req.context.as_deref(), Some("weekly"));
        assert_eq!(req.date, "2026-10-14");

        let mut kinds = vec![];
        while let Ok(e) = rx.try_recv() {
            kinds.push(std::mem::discriminant(&e));
        }
        let expected = [
            SessionEvent::ProviderStarting(ProviderName::new("x").unwrap()),
            SessionEvent::ProviderStarting(ProviderName::new("x").unwrap()),
            SessionEvent::ProviderReady(ProviderName::new("x").unwrap(), Readiness::AlreadyUp),
            SessionEvent::ProviderReady(ProviderName::new("x").unwrap(), Readiness::AlreadyUp),
            SessionEvent::RecordingSaved(PathBuf::new()),
            SessionEvent::Transcribing,
            SessionEvent::Transcribed {
                characters: 0,
                path: PathBuf::new(),
            },
            SessionEvent::Rewriting {
                format: String::new(),
            },
            SessionEvent::NoteSaved(PathBuf::new()),
        ];
        assert_eq!(
            kinds,
            expected
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>()
        );
    }

    use saidwork_providers::Readiness;

    #[tokio::test]
    async fn silent_recording_is_rejected_before_any_upload() {
        let tmp = tempfile::tempdir().unwrap();
        let transcriber = Arc::new(FakeTranscriber(Mutex::new(vec![])));
        let p = pipeline(
            tmp.path(),
            transcriber.clone(),
            Arc::new(FakeRewriter(Mutex::new(None))),
        );
        let err = p
            .process(
                &recording(vec![0; 16_000]),
                &FormatId::new("task-list").unwrap(),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::SilentRecording));
        assert!(transcriber.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn format_vocabulary_becomes_the_transcription_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let transcriber = Arc::new(PromptTranscriber(Mutex::new(vec![])));
        let p = pipeline(
            tmp.path(),
            transcriber.clone(),
            Arc::new(FakeRewriter(Mutex::new(None))),
        );
        std::fs::write(
            tmp.path().join("formats").join("standup.md"),
            "---\nname: Standup\ndescription: d\nvocabulary: Kubernetes, Grafana\n---\n# {{title}}\n",
        )
        .unwrap();
        let rec = recording(vec![5; 1600]);
        for id in ["standup", "task-list"] {
            p.process(&rec, &FormatId::new(id).unwrap(), None, None)
                .await
                .unwrap();
        }
        assert_eq!(
            *transcriber.0.lock().unwrap(),
            [Some("Kubernetes, Grafana".to_owned()), None]
        );
    }

    #[tokio::test]
    async fn transcription_failure_propagates_and_keeps_the_wav() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pipeline(
            tmp.path(),
            Arc::new(FailingTranscriber),
            Arc::new(FakeRewriter(Mutex::new(None))),
        );
        let rec = recording(vec![5; 1600]);
        let err = p
            .process(&rec, &FormatId::new("task-list").unwrap(), None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Transcription(TranscriptionError::Status { status: 500, .. })
            ),
            "{err}"
        );
        assert!(
            tmp.path()
                .join("sessions")
                .join(format!("{}.wav", rec.id))
                .exists()
        );
    }

    #[tokio::test]
    async fn unknown_format_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pipeline(
            tmp.path(),
            Arc::new(FakeTranscriber(Mutex::new(vec![]))),
            Arc::new(FakeRewriter(Mutex::new(None))),
        );
        let err = p
            .rewrite_and_save("text", &FormatId::new("nope").unwrap(), None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Selection(SelectionError::NotACandidate { .. })
            ),
            "{err}"
        );
    }
}
