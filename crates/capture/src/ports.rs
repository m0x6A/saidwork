use crate::domain::{AudioDevice, AudioSpec, CaptureError, SourceKind};

/// A blocking PCM source producing interleaved signed 16-bit samples.
pub trait AudioStream: Send {
    /// Fill `buf` with samples. Returns the number of samples written; `0` means the
    /// stream has ended. Implementations may block until the buffer is full.
    fn read(&mut self, buf: &mut [i16]) -> Result<usize, CaptureError>;
}

/// Platform audio system.
pub trait AudioBackend: Send + Sync {
    /// Every capturable device, microphones and system-audio monitors alike.
    fn list_devices(&self) -> Result<Vec<AudioDevice>, CaptureError>;

    /// The device marked default for `kind`, if any.
    fn default_device(&self, kind: SourceKind) -> Result<Option<AudioDevice>, CaptureError> {
        Ok(self
            .list_devices()?
            .into_iter()
            .find(|d| d.kind == kind && d.is_default))
    }

    /// Start capturing `device` at `spec`. The backend resamples/downmixes as needed.
    fn open(
        &self,
        device: &AudioDevice,
        spec: AudioSpec,
    ) -> Result<Box<dyn AudioStream>, CaptureError>;
}
