//! Pure mixing helpers.

/// Add `source` into `target` sample by sample with saturation.
///
/// `target` keeps its length; extra samples of `source` are ignored and a shorter
/// `source` leaves the tail of `target` untouched (i.e. it is treated as silence).
pub fn mix_into(target: &mut [i16], source: &[i16]) {
    for (t, &s) in target.iter_mut().zip(source) {
        *t = t.saturating_add(s);
    }
}

/// Root-mean-square level of a chunk in `0.0..=1.0`.
#[must_use]
pub fn rms_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let rms = (sum / samples.len() as f64).sqrt() / f64::from(i16::MAX);
    rms.clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn mixes_with_saturation() {
        let mut t = vec![100, i16::MAX, i16::MIN, 5];
        mix_into(&mut t, &[50, 10, -10]);
        assert_eq!(t, [150, i16::MAX, i16::MIN, 5]);
    }

    #[test]
    fn rms_of_silence_is_zero_and_full_scale_is_one() {
        assert_eq!(rms_level(&[]), 0.0);
        assert_eq!(rms_level(&[0; 10]), 0.0);
        assert!((rms_level(&[i16::MAX; 10]) - 1.0).abs() < 1e-3);
        let half = rms_level(&[i16::MAX / 2; 10]);
        assert!((half - 0.5).abs() < 1e-2, "{half}");
    }

    proptest! {
        #[test]
        fn mix_never_panics_and_keeps_length(a in prop::collection::vec(any::<i16>(), 0..64), b in prop::collection::vec(any::<i16>(), 0..64)) {
            let mut t = a.clone();
            mix_into(&mut t, &b);
            prop_assert_eq!(t.len(), a.len());
        }

        #[test]
        fn rms_is_within_unit_range(s in prop::collection::vec(any::<i16>(), 0..256)) {
            let l = rms_level(&s);
            prop_assert!((0.0..=1.0).contains(&l));
        }
    }
}
