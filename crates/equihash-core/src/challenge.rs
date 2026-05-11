//! Challenge derivation and Equihash input-block (`I`) construction.
//!
//! Both must be byte-identical on-chain and off-chain — any divergence breaks
//! verification. All callers MUST go through these helpers.

use sha2::{Digest, Sha256};

use crate::PERSONALIZATION;

/// Length of the Equihash input block `I` for Equium: 9 + 32 + 32 + 8.
pub const I_LEN: usize = 9 + 32 + 32 + 8;

/// Build the Equihash input block bound to a specific miner.
///
/// `I = "Equium-v1" || current_challenge || miner_pubkey || block_height_le`
///
/// Binding the miner's pubkey here is what defeats front-running: a copyist
/// must re-sign with their own pubkey, which produces a different `I` and
/// invalidates the solution.
pub fn build_input(
    current_challenge: &[u8; 32],
    miner_pubkey: &[u8; 32],
    block_height: u64,
) -> [u8; I_LEN] {
    let mut buf = [0u8; I_LEN];
    buf[..9].copy_from_slice(PERSONALIZATION);
    buf[9..41].copy_from_slice(current_challenge);
    buf[41..73].copy_from_slice(miner_pubkey);
    buf[73..81].copy_from_slice(&block_height.to_le_bytes());
    buf
}

/// Derive the next round's challenge after a winning mine.
///
/// `next = sha256(prev_challenge || winner_pubkey || open_slot_hash)`
///
/// `open_slot_hash` comes from the SlotHashes sysvar at the slot the new round
/// opens. Empty rounds (no winner) pass `[0u8; 32]` as `winner_pubkey`.
pub fn derive_next_challenge(
    prev_challenge: &[u8; 32],
    winner_pubkey: &[u8; 32],
    open_slot_hash: &[u8; 32],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev_challenge);
    h.update(winner_pubkey);
    h.update(open_slot_hash);
    h.finalize().into()
}

/// SHA-256 hash of the solution indices concatenated with the input block.
/// Compared against the difficulty target `T` (lexicographic big-endian).
pub fn solution_hash(soln_indices: &[u8], input: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(soln_indices);
    h.update(input);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_block_is_deterministic() {
        let challenge = [1u8; 32];
        let pubkey = [2u8; 32];
        let height = 42u64;
        let i1 = build_input(&challenge, &pubkey, height);
        let i2 = build_input(&challenge, &pubkey, height);
        assert_eq!(i1, i2);
    }

    #[test]
    fn input_block_changes_with_pubkey() {
        let challenge = [1u8; 32];
        let height = 42u64;
        let i_a = build_input(&challenge, &[0xAAu8; 32], height);
        let i_b = build_input(&challenge, &[0xBBu8; 32], height);
        assert_ne!(i_a, i_b);
    }

    #[test]
    fn next_challenge_is_pure() {
        let prev = [9u8; 32];
        let winner = [3u8; 32];
        let slot = [7u8; 32];
        let a = derive_next_challenge(&prev, &winner, &slot);
        let b = derive_next_challenge(&prev, &winner, &slot);
        assert_eq!(a, b);
    }
}
