//! Diagnostics: how much a frame cares about a little more work. A probe adds
//! a known cost to every frame and a run with `VICTORY_AB` (the probe's
//! switch on and off in turn) says how much of it the frame time shows: all
//! of it where that work is on the chain the frame comes out of, none where
//! the thread or the card had to wait anyway.
//!
//! - `VICTORY_BURN_CPU_US=<n>`: every submission spins `n` microseconds on the
//!   submitting thread, inside the submission lock, where its preparing is
//!   (29 submissions a frame: 100 is 2.9 ms a frame). Switch
//!   `VICTORY_BURN_CPU`.
//! - `VICTORY_BURN_GPU_COPIES=<n>`: every present has `n` more copies of the
//!   frame into the back buffer on the card (8 MB each; some 20 us on a card
//!   with 900 GB/s). Switch `VICTORY_BURN_GPU`.
//!
//! Without the variables nothing is done.

use std::sync::OnceLock;

fn number(name: &str) -> u64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Spins for the microseconds asked for (if the probe is on).
pub(super) fn burn_cpu() {
    static US: OnceLock<u64> = OnceLock::new();
    let us = *US.get_or_init(|| number("VICTORY_BURN_CPU_US"));
    if us == 0 || !crate::toggled!("VICTORY_BURN_CPU") {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_micros(us);
    while std::time::Instant::now() < until {
        std::hint::spin_loop();
    }
}

/// The copies to add to a present (0 if the probe is off).
pub(super) fn gpu_copies() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    let n = *N.get_or_init(|| number("VICTORY_BURN_GPU_COPIES"));
    if n == 0 || !crate::toggled!("VICTORY_BURN_GPU") {
        return 0;
    }
    n
}
