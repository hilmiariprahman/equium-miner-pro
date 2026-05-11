//! Pure-Rust Equihash solver (Wagner's algorithm), parameter-flexible.
//!
//! Single-threaded, sort-based collision detection. Optimized for
//! correctness + small-parameter throughput, not Zcash-mainnet-scale
//! performance. Drives:
//!   - M0 bench solution generation
//!   - Reference miner correctness baseline (M3)
//!   - WASM browser miner (no FFI; runs anywhere `std` runs)

#![cfg(feature = "solver")]

use blake2b_simd::{Params as Blake2bParams, State as Blake2bState, PERSONALBYTES};

use crate::challenge::I_LEN;

#[derive(Debug, Clone)]
pub struct Solution {
    pub nonce: [u8; 32],
    pub soln_indices: Vec<u8>,
}

#[derive(Debug)]
pub enum SolveError {
    InvalidParams,
    NoSolutionFound,
}

#[derive(Clone)]
struct Row {
    hash: Vec<u8>,
    indices: Vec<u32>,
}

fn hash_output_len(n: u32) -> usize {
    let indices_per_hash = (512 / n) as usize;
    indices_per_hash * (n as usize) / 8
}

fn cbits_of(n: u32, k: u32) -> usize {
    (n / (k + 1)) as usize
}

fn cbytes_of(n: u32, k: u32) -> usize {
    cbits_of(n, k).div_ceil(8)
}

fn init_state(n: u32, k: u32, hash_output: u8) -> Blake2bState {
    let mut p = Vec::with_capacity(PERSONALBYTES);
    p.extend_from_slice(b"ZcashPoW");
    p.extend_from_slice(&n.to_le_bytes());
    p.extend_from_slice(&k.to_le_bytes());
    Blake2bParams::new()
        .hash_length(hash_output as usize)
        .personal(&p)
        .to_state()
}

fn generate_leaf_hash(base: &Blake2bState, n: u32, i: u32) -> Vec<u8> {
    let indices_per = (512 / n) as usize;
    let n_bytes = (n / 8) as usize;
    let mut state = base.clone();
    state.update(&(i / indices_per as u32).to_le_bytes());
    let full = state.finalize();
    let off = (i as usize % indices_per) * n_bytes;
    full.as_bytes()[off..off + n_bytes].to_vec()
}

fn first_cbits_eq(a: &[u8], b: &[u8], cbits: usize) -> bool {
    let full_bytes = cbits / 8;
    if a[..full_bytes] != b[..full_bytes] {
        return false;
    }
    let rem_bits = cbits % 8;
    if rem_bits == 0 {
        return true;
    }
    let mask = 0xFFu8 << (8 - rem_bits);
    (a[full_bytes] & mask) == (b[full_bytes] & mask)
}

fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

/// Indices are stored in tree-concatenation order (NOT sorted), so the
/// disjoint check must be O(n²). This matches the upstream verifier's
/// `distinct_indices`.
fn distinct_indices(a: &[u32], b: &[u32]) -> bool {
    for x in a {
        for y in b {
            if x == y {
                return false;
            }
        }
    }
    true
}

/// Concatenate index lists in canonical tree order: subtree with the smaller
/// minimum index goes first. This is exactly what the upstream verifier's
/// `Node::from_children` does, and the encoded solution must mirror it for
/// the tree-validator to walk it correctly.
fn concat_canonical(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    if a[0] < b[0] {
        out.extend_from_slice(a);
        out.extend_from_slice(b);
    } else {
        out.extend_from_slice(b);
        out.extend_from_slice(a);
    }
    out
}

/// One Wagner round: sort by hash, pair entries whose first `cbits` match,
/// XOR + trim the matched bytes.
fn round(rows: Vec<Row>, cbits: usize) -> Vec<Row> {
    let cbytes = cbits.div_ceil(8);
    let mut sorted = rows;
    sorted.sort_by(|a, b| a.hash.cmp(&b.hash));

    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i + 1;
        while j < sorted.len() && first_cbits_eq(&sorted[i].hash, &sorted[j].hash, cbits) {
            j += 1;
        }
        for ia in i..j {
            for ib in (ia + 1)..j {
                let a = &sorted[ia];
                let b = &sorted[ib];
                if !distinct_indices(&a.indices, &b.indices) {
                    continue;
                }
                let xored = xor(&a.hash, &b.hash);
                let new_hash = if xored.len() <= cbytes {
                    Vec::new()
                } else {
                    xored[cbytes..].to_vec()
                };
                out.push(Row {
                    hash: new_hash,
                    indices: concat_canonical(&a.indices, &b.indices),
                });
            }
        }
        i = j;
    }
    out
}

fn compress_indices(n: u32, k: u32, indices: &[u32]) -> Vec<u8> {
    let bits_per = cbits_of(n, k) + 1;
    let total_bits = bits_per * indices.len();
    let total_bytes = total_bits.div_ceil(8);
    let mut out = vec![0u8; total_bytes];
    let mut pos = 0usize;
    for &idx in indices {
        for b in (0..bits_per).rev() {
            let bit = (idx >> b) & 1;
            let byte = pos / 8;
            let shift = 7 - (pos % 8);
            out[byte] |= (bit as u8) << shift;
            pos += 1;
        }
    }
    out
}

/// Solve Equihash for the given (n, k) and base input bytes.
///
/// Algorithm: generate `2^(cbits+1)` initial leaves; run `k` Wagner rounds
/// (each pairs on `cbits` and trims `cbytes` from the hash); after the
/// final round, scan remaining rows for one whose residual hash is all
/// zero — that's a candidate solution. Re-verify via `equihash::is_valid_solution`
/// before returning.
pub fn solve<F>(n: u32, k: u32, input: &[u8; I_LEN], mut next_nonce: F) -> Result<Solution, SolveError>
where
    F: FnMut() -> Option<[u8; 32]>,
{
    if n == 0 || k == 0 || n % 8 != 0 || n % (k + 1) != 0 || k >= n {
        return Err(SolveError::InvalidParams);
    }

    let cbits = cbits_of(n, k);
    let cbytes = cbytes_of(n, k);
    let hash_output = hash_output_len(n) as u8;
    let n_init: u32 = 1u32 << (cbits + 1);

    let mut base_state = init_state(n, k, hash_output);
    base_state.update(input);

    loop {
        let nonce = match next_nonce() {
            Some(nn) => nn,
            None => return Err(SolveError::NoSolutionFound),
        };

        let mut state_with_nonce = base_state.clone();
        state_with_nonce.update(&nonce);

        let mut rows: Vec<Row> = (0..n_init)
            .map(|i| Row {
                hash: generate_leaf_hash(&state_with_nonce, n, i),
                indices: vec![i],
            })
            .collect();

        for _ in 0..k {
            rows = round(rows, cbits);
            if rows.is_empty() {
                break;
            }
        }

        if rows.is_empty() {
            continue;
        }

        // After k rounds the residual hash is `cbits` bits. Candidates are
        // rows whose residual hash is all zero AND have exactly 2^k indices.
        let target_indices_len = 1usize << k;
        for row in rows {
            if row.indices.len() != target_indices_len {
                continue;
            }
            if !row.hash.iter().all(|&b| b == 0) {
                continue;
            }
            let compressed = compress_indices(n, k, &row.indices);
            if equihash::is_valid_solution(n, k, input, &nonce, &compressed).is_ok() {
                return Ok(Solution {
                    nonce,
                    soln_indices: compressed,
                });
            }
        }
    }
}
