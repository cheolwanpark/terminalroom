//! Deterministic per-pixel value noise from a seed.
//!
//! Used by Grain. Reproducible from sidecar: same `(x, y, seed)` always
//! returns the same value. Output range is [-1, 1].

/// Hash three u32 inputs with a SplitMix64-flavored mixing function.
fn hash3(x: u32, y: u32, seed: u64) -> u32 {
    let mut z = seed
        .wrapping_add((x as u64).wrapping_mul(0x9E3779B97F4A7C15))
        .wrapping_add((y as u64).wrapping_mul(0xBF58476D1CE4E5B9));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    (z >> 32) as u32
}

/// Per-pixel noise in [-1, 1].
pub fn noise_at(x: u32, y: u32, seed: u64) -> f32 {
    let h = hash3(x, y, seed);
    // Map to [-1, 1).
    (h as f32) / (u32::MAX as f32 * 0.5) - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_in_range() {
        for x in 0..32 {
            for y in 0..32 {
                let v = noise_at(x, y, 0xDEAD_BEEF_CAFE_F00D);
                assert!(v >= -1.0 && v <= 1.0, "out of range at ({x},{y}): {v}");
            }
        }
    }

    #[test]
    fn noise_deterministic() {
        let a = noise_at(7, 13, 42);
        let b = noise_at(7, 13, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_decorrelate() {
        let a = noise_at(7, 13, 1);
        let b = noise_at(7, 13, 2);
        assert!(a != b);
    }

    #[test]
    fn distribution_centered_near_zero() {
        let mut sum = 0.0_f64;
        let mut count = 0_u32;
        for x in 0..64 {
            for y in 0..64 {
                sum += noise_at(x, y, 99) as f64;
                count += 1;
            }
        }
        let mean = sum / count as f64;
        assert!(mean.abs() < 0.05, "mean drift: {mean}");
    }
}
