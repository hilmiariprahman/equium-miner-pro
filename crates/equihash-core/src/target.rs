//! 256-bit difficulty target representation and lexicographic compare.
//!
//! Higher target = easier (more hashes pass the threshold), matching Bitcoin.
//! Stored on-chain as a `[u8; 32]` big-endian value.

/// Returns true iff `hash < target` interpreted as 256-bit big-endian unsigned.
#[inline]
pub fn hash_under_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in 0..32 {
        if hash[i] < target[i] {
            return true;
        }
        if hash[i] > target[i] {
            return false;
        }
    }
    false
}

/// Multiply a 256-bit big-endian target by a damped ratio. The ratio is
/// expressed as a fixed-point fraction (numerator, denominator) so we don't
/// need floats on-chain. Returns the new target, saturating at `[0xFF; 32]`
/// (max difficulty target = trivially easy) on overflow.
///
/// Used by the difficulty retarget: `new = old * num / den`.
pub fn scale_target(old: &[u8; 32], num: u64, den: u64) -> [u8; 32] {
    debug_assert!(den > 0);
    if num == den {
        return *old;
    }

    // Multiply 256-bit by u64, then divide by u64.
    let mut acc = [0u128; 5]; // 4 limbs of 64 bits each, plus carry headroom.

    // Read old as 4 big-endian u64 limbs (most-significant first).
    let limb0 = u64::from_be_bytes(old[0..8].try_into().unwrap()) as u128;
    let limb1 = u64::from_be_bytes(old[8..16].try_into().unwrap()) as u128;
    let limb2 = u64::from_be_bytes(old[16..24].try_into().unwrap()) as u128;
    let limb3 = u64::from_be_bytes(old[24..32].try_into().unwrap()) as u128;

    let n = num as u128;
    let d = den as u128;

    // Multiply each limb by num, accumulating carries from low to high index
    // (limb0 is MSB; we store products into a 5-limb 64-bit array so multiplications
    // up to 2^64 ratio are safe).
    let p0 = limb0 * n;
    let p1 = limb1 * n;
    let p2 = limb2 * n;
    let p3 = limb3 * n;

    // 5-limb sum aligned: p0 occupies acc[0..2], p1 occupies acc[1..3], etc.
    acc[0] = p0 >> 64;
    acc[1] = (p0 & 0xFFFF_FFFF_FFFF_FFFF) + (p1 >> 64);
    acc[2] = (p1 & 0xFFFF_FFFF_FFFF_FFFF) + (p2 >> 64);
    acc[3] = (p2 & 0xFFFF_FFFF_FFFF_FFFF) + (p3 >> 64);
    acc[4] = p3 & 0xFFFF_FFFF_FFFF_FFFF;

    // Propagate carries.
    for i in (1..5).rev() {
        let carry = acc[i] >> 64;
        acc[i] &= 0xFFFF_FFFF_FFFF_FFFF;
        acc[i - 1] += carry;
    }

    // Overflow above 256 bits → cap at maximum target (trivially easy).
    if acc[0] >= d * (1u128 << 64) {
        return [0xFFu8; 32];
    }

    // Long division by d, MSB first.
    let mut rem = 0u128;
    let mut q = [0u64; 5];
    for i in 0..5 {
        let cur = (rem << 64) | acc[i];
        q[i] = (cur / d) as u64;
        rem = cur % d;
    }

    // The quotient occupies q[1..5] (q[0] should be 0 if no overflow).
    if q[0] != 0 {
        return [0xFFu8; 32];
    }
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&q[1].to_be_bytes());
    out[8..16].copy_from_slice(&q[2].to_be_bytes());
    out[16..24].copy_from_slice(&q[3].to_be_bytes());
    out[24..32].copy_from_slice(&q[4].to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        let zero = [0u8; 32];
        let one = {
            let mut a = [0u8; 32];
            a[31] = 1;
            a
        };
        let max = [0xFFu8; 32];
        assert!(hash_under_target(&zero, &one));
        assert!(!hash_under_target(&one, &one));
        assert!(!hash_under_target(&one, &zero));
        assert!(hash_under_target(&zero, &max));
    }

    #[test]
    fn scale_identity() {
        let t = {
            let mut x = [0u8; 32];
            x[10] = 0xAB;
            x[20] = 0xCD;
            x
        };
        assert_eq!(scale_target(&t, 1, 1), t);
    }

    #[test]
    fn scale_double_then_half() {
        let mut t = [0u8; 32];
        t[16] = 0x80;
        let doubled = scale_target(&t, 2, 1);
        let halved = scale_target(&doubled, 1, 2);
        assert_eq!(halved, t);
    }

    #[test]
    fn scale_overflow_saturates() {
        let near_max = [0xF0u8; 32];
        let scaled = scale_target(&near_max, 100, 1);
        assert_eq!(scaled, [0xFFu8; 32]);
    }
}
