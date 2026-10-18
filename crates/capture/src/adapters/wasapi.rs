//! WASAPI backend (through cpal). Capture endpoints are microphones; render endpoints
//! are opened in loopback mode, which records everything Windows plays on them.

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{DeviceId, ErrorKind, FromSample, SampleFormat, SizedSample};

use crate::domain::{AudioDevice, AudioSpec, CaptureError, SourceKind, resample};
use crate::ports::{AudioBackend, AudioStream};

/// How long a read waits for the next packet before returning silence. Loopback streams
/// deliver nothing while no application plays audio; this keeps them flowing like a
/// PulseAudio monitor does.
const SILENCE_AFTER: Duration = Duration::from_millis(250);

/// How long to wait for WASAPI to start a stream.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Backend talking to the Windows Audio Session API.
#[derive(Debug, Clone, Default)]
pub struct WasapiBackend;

impl WasapiBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn unavailable(err: impl std::fmt::Display) -> CaptureError {
    CaptureError::BackendUnavailable(err.to_string())
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

fn describe(device: &cpal::Device, kind: SourceKind, default: Option<&str>) -> Option<AudioDevice> {
    let id = device_id(device)?;
    let label = device
        .description()
        .map_or_else(|_| device.to_string(), |d| d.name().to_owned());
    let label = match kind {
        SourceKind::Microphone => label,
        SourceKind::SystemAudio => format!("{label} (loopback)"),
    };
    Some(AudioDevice {
        is_default: default == Some(id.as_str()),
        id,
        label,
        kind,
    })
}

impl AudioBackend for WasapiBackend {
    fn list_devices(&self) -> Result<Vec<AudioDevice>, CaptureError> {
        let host = cpal::default_host();
        let default_input = host.default_input_device().as_ref().and_then(device_id);
        let default_output = host.default_output_device().as_ref().and_then(device_id);
        let inputs = host.input_devices().map_err(unavailable)?;
        let outputs = host.output_devices().map_err(unavailable)?;
        let mut devices: Vec<AudioDevice> =
            inputs
                .filter_map(|d| describe(&d, SourceKind::Microphone, default_input.as_deref()))
                .chain(outputs.filter_map(|d| {
                    describe(&d, SourceKind::SystemAudio, default_output.as_deref())
                }))
                .collect();
        devices.sort_by(|a, b| {
            (a.kind as u8, !a.is_default, &a.label).cmp(&(b.kind as u8, !b.is_default, &b.label))
        });
        Ok(devices)
    }

    fn open(
        &self,
        device: &AudioDevice,
        spec: AudioSpec,
    ) -> Result<Box<dyn AudioStream>, CaptureError> {
        let open_failed = |reason: String| CaptureError::OpenFailed {
            device: device.id.clone(),
            reason,
        };
        let id = DeviceId::from_str(&device.id).map_err(|e| open_failed(e.to_string()))?;
        let kind = device.kind;
        let (packets, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel::<()>();
        // cpal streams are not `Send`, so one thread owns it until the reader drops us.
        let thread = std::thread::spawn(move || match start_stream(&id, kind, spec, packets) {
            Ok(stream) => {
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(reason) => {
                let _ = ready_tx.send(Err(reason));
            }
        });
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Err(open_failed(reason)),
            Err(_) => return Err(open_failed("timed out starting the stream".into())),
        }
        tracing::info!(device = %device.id, kind = %device.kind, "opened wasapi stream");
        Ok(Box::new(WasapiStream {
            packets: rx,
            pending: VecDeque::new(),
            stop: Some(stop),
            thread: Some(thread),
        }))
    }
}

type Packet = Result<Vec<i16>, String>;

fn start_stream(
    id: &DeviceId,
    kind: SourceKind,
    spec: AudioSpec,
    packets: Sender<Packet>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .device_by_id(id)
        .ok_or_else(|| "device is no longer available".to_owned())?;
    let supported = match kind {
        SourceKind::Microphone => device.default_input_config(),
        SourceKind::SystemAudio => device.default_output_config(),
    }
    .map_err(|e| e.to_string())?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, spec, packets),
        SampleFormat::I16 => build::<i16>(&device, config, spec, packets),
        SampleFormat::I32 => build::<i32>(&device, config, spec, packets),
        SampleFormat::U16 => build::<u16>(&device, config, spec, packets),
        SampleFormat::U8 => build::<u8>(&device, config, spec, packets),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Build an input stream delivering `spec`-shaped packets: downmixed, resampled,
/// then copied to every requested channel.
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    spec: AudioSpec,
    packets: Sender<Packet>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let rate = config.sample_rate;
    let errors = packets.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mono: Vec<i16> = data
                    .chunks(channels)
                    .map(|frame| {
                        let sum: i32 = frame.iter().map(|&s| i32::from(s.to_sample::<i16>())).sum();
                        (sum / frame.len() as i32) as i16
                    })
                    .collect();
                let resampled = resample(&mono, rate, spec.sample_rate);
                let out_channels = usize::from(spec.channels.max(1));
                let packet = resampled
                    .into_iter()
                    .flat_map(|s| std::iter::repeat_n(s, out_channels))
                    .collect();
                let _ = packets.send(Ok(packet));
            },
            move |err: cpal::Error| {
                if matches!(
                    err.kind(),
                    ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated
                ) {
                    let _ = errors.send(Err(err.to_string()));
                } else {
                    tracing::warn!(%err, "wasapi stream error");
                }
            },
            Some(OPEN_TIMEOUT),
        )
        .map_err(|e| e.to_string())
}

struct WasapiStream {
    packets: Receiver<Packet>,
    pending: VecDeque<i16>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream for WasapiStream {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize, CaptureError> {
        let mut filled = 0;
        while filled < buf.len() {
            if self.pending.is_empty() {
                match self.packets.recv_timeout(SILENCE_AFTER) {
                    Ok(Ok(packet)) => self.pending.extend(packet),
                    Ok(Err(reason)) => return Err(CaptureError::Read(reason)),
                    Err(RecvTimeoutError::Timeout) => {
                        buf[filled..].fill(0);
                        return Ok(buf.len());
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(CaptureError::Read("wasapi stream stopped".into()));
                    }
                }
            }
            let n = self.pending.len().min(buf.len() - filled);
            for (slot, sample) in buf[filled..filled + n]
                .iter_mut()
                .zip(self.pending.drain(..n))
            {
                *slot = sample;
            }
            filled += n;
        }
        Ok(filled)
    }
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
