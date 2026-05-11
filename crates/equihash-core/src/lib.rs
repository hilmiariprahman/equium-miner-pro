//! Shared Equihash primitives used by the Equium on-chain program and all miner
//! clients. Compiles to `no_std` (default) for SBF + WASM, with a `std`/`solver`
//! feature flag for native clients that need the solver.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod challenge;
pub mod target;
pub mod verify;

#[cfg(feature = "solver")]
pub mod solver;

pub const PERSONALIZATION: &[u8; 9] = b"Equium-v1";

/// Equihash parameters locked at the program's `initialize`. The on-chain program
/// stores `(n, k)` in its config so off-chain solvers can read them without
/// hard-coding. Locked to (96, 5) on 2026-05-09 for browser/mobile UX —
/// pending M0 CU benchmark confirmation.
pub const DEFAULT_N: u32 = 96;
pub const DEFAULT_K: u32 = 5;
