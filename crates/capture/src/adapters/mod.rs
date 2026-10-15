mod fake;
#[cfg(target_os = "linux")]
mod pulse;

pub use fake::FakeBackend;
#[cfg(target_os = "linux")]
pub use pulse::PulseBackend;
