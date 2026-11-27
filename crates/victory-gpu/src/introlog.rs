//! Diagnostics: a short record, kept in memory and written to the log after
//! it ends, of what the title, its movie and our checks of texture memory do
//! over some presents. `VICTORY_INTRO_LOG=<from>,<to>`: from present `from`
//! to present `to` (`queue::presented`); unset: nothing is recorded and
//! every note is one load.
//!
//! Lines are `intro-log <us> <present> <thread> <kind> <a> <b> <c>`, the time
//! in microseconds from the first note. Kinds:
//! - `present`: the title's Present call (a: planes);
//! - `shown`: our swap chain's Present of present `a`, the picture on its way
//!   to the screen;
//! - `wait`: a wait for a frame event (a: its kind, b: time-out ms | flags
//!   << 32, c: bit 63 set when it ended on its tick, else the time-out; the
//!   rest the microseconds it blocked);
//! - `refresh`: what a check of a texture's memory did (a: address, b: bytes,
//!   c: the answer (1 passed over, the staging buffer was already filled from
//!   an earlier look; 2 the write watch saw no write; 3 the write watch saw
//!   one and the texture was detiled; 4 the guard saw no write, and it was
//!   neither hot nor its turn to be hashed; 5 partly mapped, looked at too
//!   lately; 6 partly mapped, hashed; 7 hashed, as it was; 8 hashed, changed
//!   and detiled; 9 read through the kernel), plus flags 0x100 the guard saw
//!   no write, 0x200 hot, 0x400 the texture's turn to be hashed, 0x800 an
//!   exact read);
//! - `upload`/`kept`: a list recorded the texture's upload, or left it out
//!   because its staging buffer held what was last uploaded (a: address, b:
//!   bytes, c: 1 when the staging buffer was dirty);
//! - `slot`: a sampled hash of the memory of a texture of a movie frame's
//!   size, taken at every present (a: address, b: hash, c: the hash at the
//!   present before, so equal hashes in a row show for how many presents a
//!   memory held the same picture, whatever our checks made of it).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Event {
    us: u64,
    present: u64,
    thread: u32,
    kind: &'static str,
    a: u64,
    b: u64,
    c: u64,
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
static WATCHED: Mutex<Vec<(usize, usize, u64)>> = Mutex::new(Vec::new());
static DONE: AtomicU64 = AtomicU64::new(0);

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn window() -> Option<(u64, u64)> {
    static WINDOW: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *WINDOW.get_or_init(|| {
        let text = std::env::var("VICTORY_INTRO_LOG").ok()?;
        let (from, to) = text.split_once(',')?;
        let window = (from.trim().parse().ok()?, to.trim().parse().ok()?);
        tracing::warn!(?window, "VICTORY_INTRO_LOG: events of these presents are kept and written after the last one");
        start();
        Some(window)
    })
}

/// Whether the presents now are in the window.
pub(crate) fn active() -> bool {
    window().is_some_and(|(from, to)| {
        let now = crate::queue::presented();
        // (The present that made the log is the one after `to`.)
        now + 1 >= from && now <= to
    })
}

/// Records one event of the calling thread, if the window is open.
pub(crate) fn note(kind: &'static str, a: u64, b: u64, c: u64) {
    if !active() {
        return;
    }
    let event = Event {
        us: start().elapsed().as_micros() as u64,
        present: crate::queue::presented(),
        // SAFETY: a plain query.
        thread: unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
        kind,
        a,
        b,
        c,
    };
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push(event);
}

/// A texture of the size of a movie's luma plane is looked at at every
/// present from now on.
pub(crate) fn watch(address: usize, bytes: usize) {
    if !active() || !(7 << 20..10 << 20).contains(&bytes) {
        return;
    }
    let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
    if watched.len() < 8 && !watched.iter().any(|w| w.0 == address) {
        watched.push((address, bytes, 0));
    }
}

/// A hash of 256 pieces of the memory spread over it (what the memory is
/// now, whatever the write watch and the guards were told).
fn sampled(address: usize, bytes: usize) -> u64 {
    const PIECES: usize = 256;
    const LEN: usize = 64;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut piece = [0u8; LEN];
    for i in 0..PIECES {
        let at = address + (bytes / PIECES) * i;
        crate::host::copy_mapped(piece.as_mut_ptr(), at as *const u8, LEN, false);
        for word in piece.chunks_exact(8) {
            hash = (hash ^ u64::from_le_bytes(word.try_into().expect("eight bytes"))).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// At the title's Present: notes it, samples the movie memory, and after the
/// window's last present writes the record out.
pub(crate) fn present(planes: u32) {
    let Some((_, to)) = window() else { return };
    note("present", u64::from(planes), 0, 0);
    if active() {
        let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for w in watched.iter_mut() {
            let hash = sampled(w.0, w.1);
            let previous = w.2;
            w.2 = hash;
            note("slot", w.0 as u64, hash, previous);
        }
        // (The hash before is kept here, not in the shared list.)
        let mut shared = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
        for (s, w) in shared.iter_mut().zip(&watched) {
            s.2 = w.2;
        }
    }
    if crate::queue::presented() > to && DONE.swap(1, Ordering::Relaxed) == 0 {
        let events = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()));
        for e in events {
            tracing::info!(
                "intro-log {} {} {} {} {:#x} {} {:#x}",
                e.us,
                e.present,
                e.thread,
                e.kind,
                e.a,
                e.b,
                e.c
            );
        }
    }
}
