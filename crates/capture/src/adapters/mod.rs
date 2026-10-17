mod fake;
#[cfg(target_os = "linux")]
mod pulse;
#[cfg(windows)]
mod wasapi;

pub use fake::FakeBackend;
#[cfg(target_os = "linux")]
pub use pulse::PulseBackend;
#[cfg(windows)]
pub use wasapi::WasapiBackend;
