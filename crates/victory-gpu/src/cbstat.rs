//! Diagnostics (`VICTORY_CBSTAT=1`, diagnostics builds): how much of a recording thread's time in `State::tables` is
//! spent on tables that exist only because a constant buffer view sits in them (the table is made afresh for the draw,
//! all its entries copied), against tables that are the title's own, and what the entries of such tables are:
//! counts, nanoseconds, whether the title's own slot of an entry already shows its record, and how often a table's
//! constant buffers (or its other entries) were the same as an earlier table's in the list.
//!
//! Counted per thread bank and logged with the frame-rate line (`copystat::log`), per frame.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// Whether the probes run.
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_CBSTAT"))
}

#[derive(Clone, Copy)]
pub(crate) enum S {
    /// `State::tables` calls (stages of draws) by kind: no fresh table, a table made afresh for a constant buffer, a
    /// table made afresh only because it reaches past the host heap; and their nanoseconds.
    PlainCalls,
    PlainNs,
    CbvCalls,
    CbvNs,
    EdgeCalls,
    EdgeNs,
    /// Of the calls that made tables afresh for a constant buffer: nanoseconds of their loop over the title's own
    /// tables' entries (`table_entry`), and of the loop over the fresh tables.
    CbvPlainNs,
    CbvFreshNs,
    /// Fresh tables (for a constant buffer): new in the list, known in the list; their nanoseconds (the slot's whole
    /// iteration, key included); and the key's nanoseconds.
    NewTables,
    NewNs,
    KnownTables,
    KnownNs,
    KeyNs,
    /// Of the fresh tables: entries (n), used entries that are constant buffers, used entries that are not, and the
    /// span of the constant buffers (last index + 1) and of the span from the first.
    Entries,
    CbvEntries,
    OtherUsed,
    Span,
    SpanFromFirst,
    /// Sampled (one new table in eight): the loop body per entry, nanoseconds and entries, of constant buffer entries
    /// and others, by whether the entry was found in the thread's table of views (`make_listed`).
    SampleCbvN,
    SampleCbvNs,
    SampleOtherN,
    SampleOtherNs,
    SampleOtherListed,
    /// Sampled: the readiness of an entry (`readable`), calls and nanoseconds, in the fresh path.
    ReadN,
    ReadNs,
    /// New fresh tables' other used entries: whether the title's own slot shows the entry's record now (`mirror_current`).
    SlotCurrent,
    SlotStale,
    /// Sampled (one new table in sixteen): `HostHeap::mirror` of the other used entries, as the title's own table would
    /// take them: calls and nanoseconds.
    MirrorN,
    MirrorNs,
    /// The title's own tables' entries (`table_entry`, one call in four timed): calls, timed calls, nanoseconds.
    EntryCalls,
    EntryTimed,
    EntryNs,
    /// Of the new fresh tables: the constant buffer entries' records (with the span) were met in this list before, or
    /// not; the other used entries' records, likewise.
    CbvKeySeen,
    CbvKeyNew,
    OtherKeySeen,
    OtherKeyNew,
    /// Of the fresh tables: whole keys met before (the table's address included) in this list by a new table: never
    /// counted twice as `Known`.
    /// Tables whose constant buffers are all of one (address, size) across the list, a count of distinct such sizes.
    CbvBytes,
    /// Of all `tables` calls: used entries of tables with no constant buffer.
    PlainEntries,
    /// Tables of constant buffers by what became of them, per stage kind (vertex, pixel, mesh, compute): made
    /// afresh whole, or split (constant buffer views in a block, the rest the title's). And, of the whole ones, how
    /// many were so because the shader has no base for them (not translated with it, or a table it reads by a
    /// computed index), and how many although it has one (the switch off, indexed, past the heap).
    VsFresh,
    VsSplit,
    PsFresh,
    PsSplit,
    MsFresh,
    MsSplit,
    CsFresh,
    CsSplit,
    FreshUnbased,
    FreshBased,
}

const N: usize = 64;

#[repr(align(64))]
struct Bank([AtomicU64; N]);

const BANKS: usize = 16;
static COUNT: [Bank; BANKS] = [const { Bank([const { AtomicU64::new(0) }; N]) }; BANKS];

fn bank() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    thread_local!(static MINE: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) });
    MINE.try_with(|mine| {
        if mine.get() == usize::MAX {
            mine.set(NEXT.fetch_add(1, Relaxed) % BANKS);
        }
        mine.get()
    })
    .unwrap_or(0)
}

pub(crate) fn add(s: S, n: u64) {
    COUNT[bank()].0[s as usize].fetch_add(n, Relaxed);
}

/// A timer: `Some` when the probes run.
pub(crate) fn begin() -> Option<Instant> {
    on().then(Instant::now)
}

/// Nanoseconds since `began`.
pub(crate) fn since(began: Option<Instant>) -> Option<u64> {
    began.map(|b| b.elapsed().as_nanos() as u64)
}

thread_local! {
    static SAMPLES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether this is a sampled unit: one in `every`, by this thread's count.
pub(crate) fn sampled(every: u32) -> bool {
    on() && SAMPLES.with(|s| {
        let n = s.get().wrapping_add(1);
        s.set(n);
        n % every == 0
    })
}

fn taken(i: usize) -> u64 {
    COUNT.iter().map(|b| b.0[i].swap(0, Relaxed)).sum()
}

/// The line, per frame.
pub(crate) fn log(frames: f64) {
    if !on() {
        return;
    }
    let c: Vec<f64> = (0..N).map(taken).map(|v| v as f64).collect();
    let g = |s: S| c[s as usize];
    let ms = |s: S| g(s) / 1e6 / frames;
    let per = |ns: S, n: S| g(ns) / g(n).max(1.0);
    tracing::info!(
        "cbstat per frame: tables() plain {:.0} calls {:.2} ms ({:.0} ns each, {:.1} entries each); with a fresh table for a constant buffer {:.0} calls {:.2} ms \
         ({:.0} ns each: plain entries {:.2} ms, fresh tables {:.2} ms); fresh only past the heap {:.0} calls {:.2} ms",
        g(S::PlainCalls) / frames,
        ms(S::PlainNs),
        per(S::PlainNs, S::PlainCalls),
        g(S::PlainEntries) / g(S::PlainCalls).max(1.0),
        g(S::CbvCalls) / frames,
        ms(S::CbvNs),
        per(S::CbvNs, S::CbvCalls),
        ms(S::CbvPlainNs),
        ms(S::CbvFreshNs),
        g(S::EdgeCalls) / frames,
        ms(S::EdgeNs),
    );
    let fresh = g(S::NewTables) + g(S::KnownTables);
    tracing::info!(
        "cbstat fresh tables per frame: {:.0} new {:.2} ms ({:.0} ns each), {:.0} known {:.2} ms ({:.0} ns each); key {:.2} ms; \
         {:.1} entries each ({:.2} constant buffers, {:.2} other used; span {:.2}, from first {:.2})",
        g(S::NewTables) / frames,
        ms(S::NewNs),
        per(S::NewNs, S::NewTables),
        g(S::KnownTables) / frames,
        ms(S::KnownNs),
        per(S::KnownNs, S::KnownTables),
        ms(S::KeyNs),
        g(S::Entries) / fresh.max(1.0),
        g(S::CbvEntries) / fresh.max(1.0),
        g(S::OtherUsed) / fresh.max(1.0),
        g(S::Span) / fresh.max(1.0),
        g(S::SpanFromFirst) / fresh.max(1.0),
    );
    tracing::info!(
        "cbstat tables of constant buffers per frame, made afresh whole / split: vertex {:.0} / {:.0}, pixel {:.0} / {:.0}, mesh {:.0} / {:.0}, compute {:.0} / {:.0}; \
         whole because the shader has no base for the table {:.0}, although it has one {:.0}",
        g(S::VsFresh) / frames,
        g(S::VsSplit) / frames,
        g(S::PsFresh) / frames,
        g(S::PsSplit) / frames,
        g(S::MsFresh) / frames,
        g(S::MsSplit) / frames,
        g(S::CsFresh) / frames,
        g(S::CsSplit) / frames,
        g(S::FreshUnbased) / frames,
        g(S::FreshBased) / frames,
    );
    let new = g(S::NewTables).max(1.0);
    tracing::info!(
        "cbstat sampled entries: constant buffer {:.0} ns x {:.2} per new table; other {:.0} ns x {:.2} per new table ({:.0} % found in the thread's views); \
         readable {:.0} ns x {:.2}; title slot of an other used entry current {:.0} % ({:.0} of {:.0} a frame); mirror of them {:.0} ns ({:.0} sampled); \
         title's own entry {:.0} ns (of {:.0} calls a frame, {:.0} timed); \
         records seen before in the list: constant buffers {:.0} % of new tables, other entries {:.0} %",
        per(S::SampleCbvNs, S::SampleCbvN),
        g(S::CbvEntries) / new,
        per(S::SampleOtherNs, S::SampleOtherN),
        g(S::OtherUsed) / new,
        100.0 * g(S::SampleOtherListed) / g(S::SampleOtherN).max(1.0),
        per(S::ReadNs, S::ReadN),
        g(S::ReadN) / g(S::SampleOtherN).max(1.0),
        100.0 * g(S::SlotCurrent) / (g(S::SlotCurrent) + g(S::SlotStale)).max(1.0),
        g(S::SlotCurrent) / frames,
        (g(S::SlotCurrent) + g(S::SlotStale)) / frames,
        per(S::MirrorNs, S::MirrorN),
        g(S::MirrorN),
        per(S::EntryNs, S::EntryTimed),
        g(S::EntryCalls) / frames,
        g(S::EntryTimed),
        100.0 * g(S::CbvKeySeen) / (g(S::CbvKeySeen) + g(S::CbvKeyNew)).max(1.0),
        100.0 * g(S::OtherKeySeen) / (g(S::OtherKeySeen) + g(S::OtherKeyNew)).max(1.0),
    );
}

/// The iteration of a fresh table: records which kind it was when it ends.
pub(crate) struct Fresh {
    began: Option<Instant>,
    /// 0: left alone (no room, or skipped), 1 new, 2 known.
    pub kind: u8,
}

impl Fresh {
    pub(crate) fn begin() -> Fresh {
        Fresh { began: begin(), kind: 0 }
    }
}

impl Drop for Fresh {
    fn drop(&mut self) {
        let Some(ns) = since(self.began) else { return };
        match self.kind {
            1 => {
                add(S::NewTables, 1);
                add(S::NewNs, ns);
            }
            2 => {
                add(S::KnownTables, 1);
                add(S::KnownNs, ns);
            }
            _ => {}
        }
    }
}

/// One entry of a sampled new table: its nanoseconds when it ends, by what it was.
pub(crate) struct Entry {
    began: Option<Instant>,
    pub cbv: bool,
    pub listed: bool,
}

impl Entry {
    pub(crate) fn begin(sampled: bool) -> Entry {
        Entry { began: sampled.then(Instant::now), cbv: false, listed: false }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let Some(ns) = since(self.began) else { return };
        if self.cbv {
            add(S::SampleCbvN, 1);
            add(S::SampleCbvNs, ns);
        } else {
            add(S::SampleOtherN, 1);
            add(S::SampleOtherNs, ns);
            if self.listed {
                add(S::SampleOtherListed, 1);
            }
        }
    }
}
