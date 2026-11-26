//! Diagnostics: what the guards and the runtime's notices knew of the writes
//! to the memory of textures picked by index, so that a texture a look found
//! changed (`crate::draw`, `State::heap_textures`) can be told apart by
//! cause: was the write in the journal (a guard saw it, or a notice opened the
//! block), was a notice told after the upload (and which), what were the
//! guards' bits, which of the three windows the fingerprint reads changed.
//!
//! Only blocks of textures under a look are stamped ([`watch`]); a stamp is
//! a sequence number, the frame and the thread. Builds with diagnostics only;
//! nothing here changes what the guards do, but for the askings ([`asking`])
//! that read the journal.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Mutex};

use super::{BLOCK, Bits};

/// What told of a write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub(crate) enum Kind {
    /// `watch::writing`: the runtime (a stream's notice, after or before the
    /// bytes), or a copy of ours, is about to write.
    Writing,
    /// `watch::landing`: what shaders wrote is about to be put there.
    Landing,
    /// `watch::kernel_writing`: a kernel read is about to land there.
    Kernel,
    /// `watch::landed_beside`: bytes were put in beside the guard.
    Beside,
    /// The runtime mapped, unmapped or freed the memory.
    Changed,
    /// `watch::title_memory_written`: the bytes of a write are there.
    PageWritten,
    /// The title's store faulted on a guarded block and opened it.
    Fault,
    /// Another view of the block's page could not be made read-only: the
    /// block counts as written at every asking.
    Uncovered,
    /// A fault's notice was kept for the next texture pass at this block
    /// (`watch::faulted`).
    Told,
}

const NAMES: [&str; 9] =
    ["writing", "landing", "kernel", "beside", "changed", "page-written", "fault", "uncovered", "notice"];

#[derive(Clone, Copy, Default)]
struct Stamp {
    seq: u64,
    frame: u32,
    tid: u32,
}

/// The last two stamps of each kind of a block, the older first.
type Row = [[Stamp; 2]; 9];

static WATCHED: Bits = Bits::new();
static SEQ: AtomicU64 = AtomicU64::new(1);
static STAMPS: LazyLock<Mutex<HashMap<usize, Row>>> = LazyLock::new(Default::default);

/// Whether the notes are kept (`VICTORY_TEXTURE_CAUSES=1`; off unless asked
/// for: every upload takes two fingerprints and an asking more).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TEXTURE_CAUSES").is_some_and(|v| v == "1"))
}

/// The number the next stamp or upload gets.
pub(crate) fn seq() -> u64 {
    SEQ.load(Relaxed)
}

/// The blocks of `[at, at + len)` are those of a texture under a look from now
/// on: writes told of them are stamped.
pub(crate) fn watch(at: usize, len: usize) {
    if !on() {
        return;
    }
    for block in at / BLOCK..at.saturating_add(len).div_ceil(BLOCK) {
        WATCHED.set(block);
    }
}

fn thread() -> u32 {
    // SAFETY: a question about this thread.
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}

/// A write of `kind` to `[at, at + len)` was told (or happened).
pub(crate) fn note(kind: Kind, at: usize, len: usize) {
    let (first, last) = (at / BLOCK, at.saturating_add(len.max(1)).div_ceil(BLOCK));
    // (Nothing is watched while the notes are off.)
    if last - first > 1 << 16 || !WATCHED.any(first, last) {
        return;
    }
    let stamp = Stamp { seq: SEQ.fetch_add(1, Relaxed), frame: crate::frame::current() as u32, tid: thread() };
    let mut rows = STAMPS.lock().unwrap_or_else(|e| e.into_inner());
    for block in first..last {
        if WATCHED.any(block, block + 1) {
            let at = &mut rows.entry(block).or_default()[kind as usize];
            at[0] = at[1];
            at[1] = stamp;
        }
    }
}

/// A write to the page at block `block` through any view of it (a fault).
pub(crate) fn note_views(kind: Kind, block: usize) {
    note(kind, block * BLOCK, BLOCK);
    for other in crate::inplace::pages_of(block * BLOCK) {
        if other / BLOCK != block {
            note(kind, other, BLOCK);
        }
    }
}

/// What was told of `[at, at + len)` since `seq`, by kind: how many blocks,
/// the frame of the first stamp after `seq` (that the two kept show) and of
/// the last, and the thread of the first.
pub(crate) struct Told {
    pub(crate) kind: Kind,
    pub(crate) blocks: usize,
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) tid: u32,
}

pub(crate) fn since(at: usize, len: usize, seq: u64) -> Vec<Told> {
    let (first, last) = (at / BLOCK, at.saturating_add(len.max(1)).div_ceil(BLOCK));
    let rows = STAMPS.lock().unwrap_or_else(|e| e.into_inner());
    let kinds = [
        Kind::Writing,
        Kind::Landing,
        Kind::Kernel,
        Kind::Beside,
        Kind::Changed,
        Kind::PageWritten,
        Kind::Fault,
        Kind::Uncovered,
        Kind::Told,
    ];
    let mut told: Vec<Told> = Vec::new();
    for block in first..last {
        let Some(row) = rows.get(&block) else { continue };
        for kind in kinds {
            let [old, new] = row[kind as usize];
            if new.seq == 0 || new.seq < seq {
                continue;
            }
            let from = if old.seq >= seq && old.seq != 0 { old } else { new };
            match told.iter_mut().find(|t| t.kind == kind) {
                Some(t) => {
                    t.blocks += 1;
                    if from.frame < t.first {
                        t.first = from.frame;
                        t.tid = from.tid;
                    }
                    t.last = t.last.max(new.frame);
                }
                None => told.push(Told { kind, blocks: 1, first: from.frame, last: new.frame, tid: from.tid }),
            }
        }
    }
    told
}

/// The guards' bits of the blocks of `[at, at + len)`.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) blocks: usize,
    pub(crate) protected: usize,
    pub(crate) clean: usize,
    pub(crate) aliased: usize,
    pub(crate) absent: usize,
    pub(crate) kernel: usize,
    pub(crate) beside: usize,
    pub(crate) published: usize,
}

pub(crate) fn state(at: usize, len: usize) -> State {
    let (first, last) = (at / BLOCK, at.saturating_add(len.max(1)).div_ceil(BLOCK));
    let g = super::guards();
    let mut s = State { blocks: last - first, ..State::default() };
    for block in first..last {
        s.protected += usize::from(g.protected.contains(block));
        s.clean += usize::from(g.clean.contains(block));
        s.aliased += usize::from(g.aliased.contains(block));
        s.absent += usize::from(g.absent.contains(block));
        s.kernel += usize::from(g.left_alone(block));
        s.beside += usize::from(super::BESIDE.any(block, block + 1));
        s.published += usize::from(super::PUBLISHED.any(block, block + 1));
    }
    s
}

/// An asking about `[at, at + len)` as a host copy of it makes (`watch::writes`,
/// guarding the memory where it is view memory): its number, 0 if the memory
/// is not tracked.
pub(crate) fn asking(at: usize, len: usize) -> u64 {
    super::writes(at, at.saturating_add(len), true).map_or(0, |(now, _)| now)
}

/// Pages of `[at, at + len)` the journal has as found written by an asking
/// after `asked`, and the pages it knows of; `None` if the memory is not
/// tracked.
pub(crate) fn written_since(at: usize, len: usize, asked: u64) -> Option<(usize, usize)> {
    let (_, pages) = super::writes(at, at.saturating_add(len), true)?;
    Some((pages.iter().filter(|&&(_, w)| w > asked).count(), pages.len()))
}

/// What the look that uploaded slot `slot` knew.
#[derive(Clone, Copy)]
struct Upload {
    seq: u64,
    asked: u64,
    frame: u64,
    parts: [u64; 3],
}

static UPLOADS: LazyLock<Mutex<HashMap<u32, Upload>>> = LazyLock::new(Default::default);

/// Slot `slot`'s texture at `[at, at + len)` was taken at `frame`, with the
/// stamps numbered from `seq` on counted as after it; `asked`: the number of
/// the asking its bytes were taken after, if its staging buffer knows one.
pub(crate) fn uploaded(slot: u32, seq: u64, at: usize, len: usize, frame: u64, asked: Option<u64>) {
    if !on() {
        return;
    }
    watch(at, len);
    let asked = asked.filter(|&n| n != 0).unwrap_or_else(|| asking(at, len));
    let parts = crate::texture::fingerprint_parts(at as u64, len as u64);
    UPLOADS.lock().unwrap_or_else(|e| e.into_inner()).insert(slot, Upload { seq, asked, frame, parts });
}

/// Why a look found a change.
pub(crate) struct Cause {
    /// By what the guards and notices knew, for counting.
    pub(crate) class: String,
    /// All of it, for the log.
    pub(crate) detail: String,
}

/// What was known of slot `slot`'s texture at `[at, at + len)`, which a look
/// in `frame` found changed since its upload.
pub(crate) fn cause(slot: u32, at: usize, len: usize, frame: u64) -> Cause {
    if !on() {
        return Cause { class: String::new(), detail: String::new() };
    }
    let up = UPLOADS.lock().unwrap_or_else(|e| e.into_inner()).get(&slot).copied();
    let Some(up) = up else {
        return Cause { class: "no record of the upload".into(), detail: String::new() };
    };
    // (The guards' bits first: the asking of the journal guards again.)
    let state = state(at, len);
    let journal = written_since(at, len, up.asked);
    let told = since(at, len, up.seq);
    let (woke, last_woken) = woken_since(slot, up.seq);
    let tabled = crate::descriptor::with_view_heap(|host| host.tabled_lately_for_notes(slot, frame)).unwrap_or(false);
    let why = crate::descriptor::with_view_heap(|host| host.why_not_woken(slot, frame, at as u64, len as u64))
        .unwrap_or_default();
    let now = parts_changed(at, len, &up.parts);
    let guard = if state.blocks == 0 {
        "no blocks"
    } else if state.protected == state.blocks && state.clean == state.blocks {
        "intact"
    } else if state.protected == 0 {
        "open"
    } else {
        "mixed"
    };
    let names: Vec<&str> = told.iter().map(|t| NAMES[t.kind as usize]).collect();
    let journal_says = match journal {
        None => "untracked",
        Some((0, _)) => "no",
        Some(_) => "yes",
    };
    let class = format!(
        "journal={journal_says} guard={guard} told={} woken={} tabled={}",
        if names.is_empty() { "nothing".to_owned() } else { names.join("+") },
        if woke == 0 { "no" } else { "yes" },
        if tabled { "yes" } else { "no" }
    );
    let after: Vec<String> = told
        .iter()
        .map(|t| format!("{}x{}@{}..{}(thread {})", NAMES[t.kind as usize], t.blocks, t.first, t.last, t.tid))
        .collect();
    let detail = format!(
        "journal pages written since the upload {:?}; blocks {} protected {} clean {} aliased {} absent {} left alone {} \
         beside bits {} published {}; told since the upload: [{}]; woken since the upload {woke} times, the last in frame \
         {last_woken}; windows changed {now:03b}; upload frame {}, look frame {frame}; {why}",
        journal,
        state.blocks,
        state.protected,
        state.clean,
        state.aliased,
        state.absent,
        state.kernel,
        state.beside,
        state.published,
        after.join(", "),
        up.frame
    );
    Cause { class, detail }
}

/// The last four times a write fault brought slot's look forward, or made it
/// due: the number stamp and the frame.
static WOKEN: LazyLock<Mutex<HashMap<u32, [(u64, u32); 4]>>> = LazyLock::new(Default::default);

/// A write fault woke slot `slot` in `frame`.
pub(crate) fn woken(slot: u32, frame: u64) {
    if !on() {
        return;
    }
    let seq = SEQ.fetch_add(1, Relaxed);
    let mut all = WOKEN.lock().unwrap_or_else(|e| e.into_inner());
    let ring = all.entry(slot).or_default();
    ring.rotate_left(1);
    ring[3] = (seq, frame as u32);
}

/// How many of the last four wakes of `slot` were after `seq`, and the frame
/// of the last.
fn woken_since(slot: u32, seq: u64) -> (usize, u32) {
    let all = WOKEN.lock().unwrap_or_else(|e| e.into_inner());
    all.get(&slot).map_or((0, 0), |ring| (ring.iter().filter(|w| w.0 != 0 && w.0 >= seq).count(), ring[3].1))
}

/// Which of the fingerprint's windows differ from `before`, as bits.
fn parts_changed(at: usize, len: usize, before: &[u64; 3]) -> u8 {
    let now = crate::texture::fingerprint_parts(at as u64, len as u64);
    (0..3).fold(0, |bits, i| bits | u8::from(now[i] != before[i]) << i)
}

static CAUSES: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);

/// Counts a change found by a look of this class; the counts so far, the most
/// frequent first.
pub(crate) fn count(class: &str) -> Vec<(String, u64)> {
    if class.is_empty() {
        return Vec::new();
    }
    let mut causes = CAUSES.lock().unwrap_or_else(|e| e.into_inner());
    *causes.entry(class.to_owned()).or_insert(0) += 1;
    let mut all: Vec<(String, u64)> = causes.iter().map(|(k, v)| (k.clone(), *v)).collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    all
}
