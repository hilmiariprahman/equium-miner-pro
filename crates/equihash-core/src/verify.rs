//! Thin wrapper around the upstream `equihash` crate's verifier, used both
//! on-chain (via the program crate's dependency) and off-chain (clients
//! sanity-check their solutions before broadcast).

use crate::challenge::{solution_hash, I_LEN};
use crate::target::hash_under_target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// Equihash puzzle did not verify (bad indices, bad nonce, malformed soln).
    InvalidEquihash,
    /// Puzzle was valid but the solution hash did not fall under the difficulty
    /// target.
    AboveTarget,
}

/// Full verification: Equihash puzzle + difficulty threshold.
///
/// `input` is exactly the I-block built by `challenge::build_input`.
/// `nonce` is the miner-chosen 32-byte nonce.
/// `soln_indices` is the canonical Equihash solution-byte encoding produced by
/// the upstream solver.
/// `target` is the 256-bit big-endian difficulty threshold.
pub fn verify(
    n: u32,
    k: u32,
    input: &[u8; I_LEN],
    nonce: &[u8; 32],
    soln_indices: &[u8],
    target: &[u8; 32],
) -> Result<[u8; 32], VerifyError> {
    equihash::is_valid_solution(n, k, input, nonce, soln_indices)
        .map_err(|_| VerifyError::InvalidEquihash)?;

    let h = solution_hash(soln_indices, input);
    if !hash_under_target(&h, target) {
        return Err(VerifyError::AboveTarget);
    }
    Ok(h)
}
