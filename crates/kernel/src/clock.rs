use chrono::{DateTime, Utc};

/// Source of the current time. Injected so that use cases are deterministic in tests.
pub trait Clock: Send + Sync {
    /// Current instant in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// Wall clock backed by the operating system.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Clock that always returns the same instant; for tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl FixedClock {
    /// Build a fixed clock from an RFC 3339 timestamp such as `2026-10-14T12:00:00Z`.
    ///
    /// # Panics
    /// Panics if the string is not valid RFC 3339; intended for test fixtures only.
    #[must_use]
    pub fn at(rfc3339: &str) -> Self {
        Self(
            DateTime::parse_from_rfc3339(rfc3339)
                .expect("valid rfc3339 timestamp")
                .with_timezone(&Utc),
        )
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_returns_configured_instant() {
        let clock = FixedClock::at("2026-10-14T12:34:56Z");
        assert_eq!(clock.now().to_rfc3339(), "2026-10-14T12:34:56+00:00");
    }

    #[test]
    fn system_clock_is_monotonic_enough() {
        let clock = SystemClock;
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a);
    }
}
