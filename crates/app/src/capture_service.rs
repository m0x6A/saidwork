use std::sync::Arc;

use saidwork_capture::{
    AudioBackend, AudioDevice, AudioSpec, CaptureError, Recorder, RecorderHandle, Recording,
    SourceKind,
};

use crate::config::{CaptureConfig, DeviceChoice};

/// Devices chosen for a recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSelection {
    pub microphone: Option<AudioDevice>,
    pub system_audio: Option<AudioDevice>,
}

impl DeviceSelection {
    #[must_use]
    pub fn devices(&self) -> Vec<AudioDevice> {
        self.microphone
            .iter()
            .chain(self.system_audio.iter())
            .cloned()
            .collect()
    }
}

/// Thin service over the capture slice: resolves configured devices and runs the
/// blocking recorder on the tokio blocking pool.
#[derive(Clone)]
pub struct CaptureService {
    backend: Arc<dyn AudioBackend>,
    spec: AudioSpec,
}

impl std::fmt::Debug for CaptureService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureService")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl CaptureService {
    #[must_use]
    pub fn new(backend: Arc<dyn AudioBackend>) -> Self {
        Self {
            backend,
            spec: AudioSpec::SPEECH,
        }
    }

    #[must_use]
    pub fn spec(&self) -> AudioSpec {
        self.spec
    }

    pub async fn list_devices(&self) -> Result<Vec<AudioDevice>, CaptureError> {
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || backend.list_devices())
            .await
            .map_err(|e| CaptureError::Read(e.to_string()))?
    }

    /// Resolve the configured choices against the available devices.
    ///
    /// A missing default for a kind is not an error (a laptop without a mic, say) as long
    /// as at least one device ends up selected; an explicitly named device must exist.
    pub async fn resolve(&self, config: &CaptureConfig) -> Result<DeviceSelection, CaptureError> {
        let devices = self.list_devices().await?;
        let pick = |choice: &DeviceChoice,
                    kind: SourceKind|
         -> Result<Option<AudioDevice>, CaptureError> {
            Ok(match choice {
                DeviceChoice::None => None,
                DeviceChoice::Default => devices
                    .iter()
                    .find(|d| d.kind == kind && d.is_default)
                    .or_else(|| devices.iter().find(|d| d.kind == kind))
                    .cloned(),
                DeviceChoice::Id(id) => Some(
                    devices
                        .iter()
                        .find(|d| &d.id == id)
                        .cloned()
                        .ok_or_else(|| CaptureError::OpenFailed {
                            device: id.clone(),
                            reason: "no such device; see `saidwork devices`".into(),
                        })?,
                ),
            })
        };
        let selection = DeviceSelection {
            microphone: pick(&config.microphone, SourceKind::Microphone)?,
            system_audio: pick(&config.system_audio, SourceKind::SystemAudio)?,
        };
        if selection.devices().is_empty() {
            return Err(CaptureError::NoSources);
        }
        Ok(selection)
    }

    /// Start recording the selected devices.
    pub async fn start(&self, selection: &DeviceSelection) -> Result<RecorderHandle, CaptureError> {
        let backend = self.backend.clone();
        let devices = selection.devices();
        let spec = self.spec;
        tokio::task::spawn_blocking(move || Recorder::start(&*backend, devices, spec))
            .await
            .map_err(|e| CaptureError::Read(e.to_string()))?
    }

    /// Stop a recording, joining its threads off the async runtime.
    pub async fn stop(handle: RecorderHandle) -> Result<Recording, CaptureError> {
        tokio::task::spawn_blocking(move || handle.stop())
            .await
            .map_err(|e| CaptureError::Read(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use saidwork_capture::FakeBackend;

    use super::*;

    fn service() -> CaptureService {
        CaptureService::new(Arc::new(
            FakeBackend::new()
                .with_endless_device(SourceKind::Microphone, "mic-a", 1)
                .with_endless_device(SourceKind::Microphone, "mic-b", 2)
                .with_endless_device(SourceKind::SystemAudio, "mon", 3),
        ))
    }

    #[tokio::test]
    async fn resolves_defaults_and_explicit_ids() {
        let s = service();
        let sel = s.resolve(&CaptureConfig::default()).await.unwrap();
        assert_eq!(sel.microphone.unwrap().id, "mic-a");
        assert_eq!(sel.system_audio.unwrap().id, "mon");

        let cfg = CaptureConfig {
            microphone: DeviceChoice::Id("mic-b".into()),
            system_audio: DeviceChoice::None,
        };
        let sel = s.resolve(&cfg).await.unwrap();
        assert_eq!(sel.devices().len(), 1);
        assert_eq!(sel.microphone.unwrap().id, "mic-b");
    }

    #[tokio::test]
    async fn unknown_id_and_nothing_selected_are_errors() {
        let s = service();
        let cfg = CaptureConfig {
            microphone: DeviceChoice::Id("ghost".into()),
            system_audio: DeviceChoice::None,
        };
        assert!(matches!(
            s.resolve(&cfg).await.unwrap_err(),
            CaptureError::OpenFailed { .. }
        ));
        let cfg = CaptureConfig {
            microphone: DeviceChoice::None,
            system_audio: DeviceChoice::None,
        };
        assert!(matches!(
            s.resolve(&cfg).await.unwrap_err(),
            CaptureError::NoSources
        ));
    }

    #[tokio::test]
    async fn records_through_the_blocking_pool() {
        let s = service();
        let sel = s.resolve(&CaptureConfig::default()).await.unwrap();
        let handle = s.start(&sel).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let rec = CaptureService::stop(handle).await.unwrap();
        assert_ne!(rec.samples.len(), 0);
        assert!(rec.samples.iter().all(|&x| x == 4), "mic-a (1) + mon (3)");
    }
}
