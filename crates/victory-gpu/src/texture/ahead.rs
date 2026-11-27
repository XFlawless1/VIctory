//! First uploads of textures picked by index on a copy queue of their own
//! (`VICTORY_UPLOAD_AHEAD`): read, detiled and copied by threads of ours as
//! soon as the texture pass has made them, and the slots that wait for them
//! shown when the copy has run. Recorded into the pass's list, as every other
//! upload is, the copy ran with the frame that needs it, so a slot that shows
//! its texture only after its upload ([`crate::descriptor::host::show_after`])
//! was black or grey in that frame, and in the next.
//!
//! Only a texture that no list has used is taken: made in the last second,
//! in the common state, and never asked for an upload before
//! ([`crate::resource::HostTexture::ahead`]). It stays in the common state
//! (a copy queue's copies leave it there), from which the card's shaders read
//! it as any texture.

use std::collections::VecDeque;
use std::sync::atomic::{
    AtomicU8, AtomicU64, AtomicUsize,
    Ordering::{AcqRel, Acquire, Relaxed, Release},
};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

use super::Staging;
use crate::object::This;
use crate::resource::HostTexture;

/// [`HostTexture::ahead`]: no upload was asked for yet; one is on its way
/// here; it has run; one was recorded into a list.
pub(crate) const NONE: u8 = 0;
const PENDING: u8 = 1;
const DONE: u8 = 2;
pub(crate) const LISTED: u8 = 3;

/// Bytes of title memory on their way at most: past it uploads go into the
/// lists, as before.
const MOST: u64 = 96 << 20;
/// Bytes and textures of one submission at most.
const BATCH: (u64, usize) = (16 << 20, 64);

/// Whether it is on (`VICTORY_UPLOAD_AHEAD`; off).
pub(crate) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_UPLOAD_AHEAD") {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_UPLOAD_AHEAD").is_some_and(|v| v != "0"))
}

/// Whether the three races of this path are closed (`VICTORY_AHEAD_SAFE`, on;
/// 0: as before; A/B-able). After Belobug/VIctory-Belobug-AMD #454 (by its
/// author, GPL-3.0-or-later):
///
/// 1. A copy that did not run ("uploads ahead did not run") marked its
///    textures uploaded all the same, showed their slots, and kept lists from
///    uploading them: the title sampled what the card never got.
/// 2. [`claim`] took a texture (`ahead` pending) a while before it showed up
///    among those on their way, the while its staging buffer was made: a list
///    that recorded its own upload of the texture in that while found nothing
///    to wait for, and both uploads ran.
/// 3. A slot shown when its upload had run ignored [`show_new_later`]: lists
///    sent before the texture was made could still read the slot by index.
///
/// [`show_new_later`]: crate::descriptor::host::show_new_later
pub(crate) fn safe() -> bool {
    crate::toggled!("VICTORY_AHEAD_SAFE")
}

/// What a job's `then` calls wait for.
enum State {
    /// The copy has not run: what to call when it has.
    Pending(Vec<Box<dyn FnOnce() + Send>>),
    /// The copy has run.
    Done,
    /// The copy did not run: nothing waits for it any more, the calls that
    /// did are dropped, and the texture is the lists'.
    Failed,
}

/// An upload on its way.
pub(crate) struct Job {
    staging: Arc<Staging>,
    copy: Arc<HostTexture>,
    asked: std::time::Instant,
    /// What to do when it has run.
    then: Mutex<State>,
}

impl Job {
    /// Calls `f` when the copy has run: now if it has, never if it did not.
    pub(crate) fn then(&self, f: impl FnOnce() + Send + 'static) {
        let mut state = self.then.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *state {
            State::Pending(calls) => calls.push(Box::new(f)),
            State::Done => {
                drop(state);
                f();
            }
            State::Failed => {}
        }
    }

    /// The submissions begun when this job's texture was made, if one of
    /// them may still run ([`crate::pool::begun_before`]).
    pub(crate) fn begun_before(&self) -> Option<u64> {
        crate::pool::begun_before(&self.copy.texture)
    }
}

/// What a texture asked for here gets.
pub(crate) enum Taken {
    /// A job of its own, to be [`send`] once what follows it is added.
    New(Arc<Job>),
    /// One already on its way, or that has run.
    Known(Arc<Job>),
}

static QUEUE: (Mutex<VecDeque<Arc<Job>>>, Condvar) = (Mutex::new(VecDeque::new()), Condvar::new());
/// Those on their way, by host copy ([`HostTexture::serial`]); no job yet
/// while [`claim`] makes the staging buffer ([`safe`]).
static ON_THEIR_WAY: Mutex<Vec<(u64, Option<Arc<Job>>)>> = Mutex::new(Vec::new());
/// How many are on their way ([`drain`] asks without the lock first).
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static LANDED: Condvar = Condvar::new();
static BYTES: AtomicU64 = AtomicU64::new(0);
/// Since the start: uploads, bytes, microseconds from the asking until the
/// copy had run, the longest; askings turned down; waits of lists; those of
/// the waits that found the texture taken and its job not made yet.
static STATS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// What [`claim`] drops when it gives up after taking a texture: the texture
/// is the lists' and what waited for the staging buffer is woken.
struct Starting(Option<Arc<HostTexture>>);

impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(copy) = self.0.take() {
            copy.ahead.store(LISTED, Release);
            let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
            way.retain(|(serial, _)| *serial != copy.serial);
            IN_FLIGHT.store(way.len(), Release);
            LANDED.notify_all();
        }
    }
}

/// The job of `copy`, which a [`claim`] has taken and is making: waits for
/// it to show up. `None`: it was given up, or has run, or a second went by
/// (as [`listed`] gives up; the claim makes a staging buffer, nothing that
/// should take a second).
fn wait_known(copy: &HostTexture) -> Option<Taken> {
    let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
    let started = std::time::Instant::now();
    loop {
        match way.iter().find(|(serial, _)| *serial == copy.serial) {
            Some((_, Some(job))) => return Some(Taken::Known(job.clone())),
            Some((_, None)) => {
                if started.elapsed().as_millis() >= 1000 {
                    tracing::warn!(serial = copy.serial, "timed out waiting for a texture being taken for an upload ahead");
                    return None;
                }
                way = LANDED.wait_timeout(way, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
            }
            None => return None,
        }
    }
}

/// Takes the first upload of `resource`'s levels from `first` on, `there` of
/// whose bytes are mapped (`u64::MAX`: not known). `None`: the list's.
pub(crate) fn take(resource: This, first: u32, there: u64) -> Option<Taken> {
    if on() { claim(resource, first, there) } else { None }
}

fn claim(resource: This, first: u32, there: u64) -> Option<Taken> {
    claim_as(resource, first, there, safe())
}

/// [`claim`], with the races [`safe`] closes (`closed`) or not.
fn claim_as(resource: This, first: u32, there: u64, closed: bool) -> Option<Taken> {
    workers()?;
    let turned_down = || {
        STATS[4].fetch_add(1, Relaxed);
        None
    };
    let copy = crate::resource::host_copy(resource)?;
    if copy.ahead.load(Acquire) == PENDING {
        if closed {
            return wait_known(&copy).or_else(turned_down);
        }
        let way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
        return way
            .iter()
            .find(|(serial, _)| *serial == copy.serial)
            .and_then(|(_, job)| job.clone())
            .map(Taken::Known);
    }
    if BYTES.load(Relaxed) >= MOST
        || copy.flags() & 0x7 != 0
        || copy.state.load(Relaxed) != 0
        || copy.grown.load(Relaxed)
        || copy.tiled.is_some()
        || copy.holds.lock().unwrap_or_else(|e| e.into_inner()).is_some()
        || !crate::pool::fresh(&copy.texture)
        || super::host_wrote(resource as usize)
        || crate::resource::alias_of(resource).is_some()
    {
        return turned_down();
    }
    // (Closed: on their way from the moment it is taken, under the lock that
    // a list's wait ([`listed`]) takes after it saw the texture pending, not
    // when its staging buffer is made.)
    let taken = if closed {
        let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
        let taken = copy.ahead.compare_exchange(NONE, PENDING, AcqRel, Acquire).is_ok();
        if taken {
            way.push((copy.serial, None));
            IN_FLIGHT.store(way.len(), Release);
        }
        taken
    } else {
        copy.ahead.compare_exchange(NONE, PENDING, Relaxed, Relaxed).is_ok()
    };
    if !taken {
        return if closed && copy.ahead.load(Acquire) == PENDING {
            wait_known(&copy).or_else(turned_down)
        } else {
            turned_down()
        };
    }
    let mut starting = Starting(Some(copy.clone()));
    #[cfg(test)]
    tests::pause_staging(copy.serial);
    super::PICKING.set(Some(there));
    let staging = super::staging(resource, &copy, first);
    super::PICKING.set(None);
    let Some(staging) = staging.filter(|s| {
        !s.on_demand && s.first == s.level0 && !s.uploaded.load(Relaxed) && s.once.load(Relaxed)
    }) else {
        return turned_down();
    };
    if super::no_wait() && super::fill_unlocked() {
        staging.expected.store(if there == u64::MAX { super::mapped_bytes(staging.memory, staging.read) } else { there }, Relaxed);
    }
    let job = Arc::new(Job { staging, copy, asked: std::time::Instant::now(), then: Mutex::new(State::Pending(Vec::new())) });
    BYTES.fetch_add(job.staging.read as u64, Relaxed);
    {
        let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
        match way.iter().position(|(serial, _)| *serial == job.copy.serial) {
            Some(at) => way[at].1 = Some(job.clone()),
            None => way.push((job.copy.serial, Some(job.clone()))),
        }
        IN_FLIGHT.store(way.len(), Release);
        LANDED.notify_all();
    }
    starting.0 = None;
    Some(Taken::New(job))
}

/// Sends a job of [`take`] on its way.
pub(crate) fn send(job: &Arc<Job>) {
    QUEUE.0.lock().unwrap_or_else(|e| e.into_inner()).push_back(job.clone());
    QUEUE.1.notify_one();
}

/// Waits, for `limit_ms` at most, until every upload now on its way has run
/// its copy and its `then` calls (which note the slots that show the new
/// texture: [`crate::descriptor::host::flip_now`], or the next texture pass,
/// makes them). A draw of the title's bake samples slots through the pass
/// before it: one whose upload was still on its way showed what stood in for
/// the texture, and the bake kept that for good in the texture it made.
/// Returns how many were on their way when it began, and whether the wait
/// ended with some still there.
pub(crate) fn drain(limit_ms: u64) -> (usize, bool) {
    if IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire) == 0 {
        return (0, false);
    }
    let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
    let waiting = way.len();
    let started = std::time::Instant::now();
    while !way.is_empty() && (started.elapsed().as_millis() as u64) < limit_ms {
        way = LANDED.wait_timeout(way, std::time::Duration::from_millis(5)).unwrap_or_else(|e| e.into_inner()).0;
    }
    (waiting, !way.is_empty())
}

/// A list is about to record an upload into `copy`: one on its way here runs
/// first, and no other is taken after it.
pub(crate) fn listed(copy: &HostTexture) {
    if copy.ahead.compare_exchange(NONE, LISTED, AcqRel, Acquire) != Err(PENDING) {
        return;
    }
    STATS[5].fetch_add(1, Relaxed);
    let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
    // The texture is taken and its staging buffer is being made: with
    // `VICTORY_AHEAD_SAFE=0` there is nothing on the list yet and the list
    // goes on, to record its own upload beside the one ahead.
    if way.iter().find(|(serial, _)| *serial == copy.serial).is_none_or(|(_, job)| job.is_none()) {
        STATS[6].fetch_add(1, Relaxed);
    }
    let started = std::time::Instant::now();
    while way.iter().any(|(serial, _)| *serial == copy.serial) {
        if started.elapsed().as_millis() >= 1000 {
            tracing::warn!(serial = copy.serial, "timed out waiting for an upload ahead");
            break;
        }
        way = LANDED.wait_timeout(way, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// The copy queue and its threads, made on first use.
fn workers() -> Option<&'static ID3D12CommandQueue> {
    /// Free-threaded.
    struct Queue(ID3D12CommandQueue);
    // SAFETY: a command queue is free-threaded.
    unsafe impl Send for Queue {}
    // SAFETY: as above.
    unsafe impl Sync for Queue {}
    static WORKERS: OnceLock<Option<Queue>> = OnceLock::new();
    WORKERS
        .get_or_init(|| {
            let host = crate::host::host()?;
            // SAFETY: plain object creation.
            let queue: ID3D12CommandQueue = unsafe {
                host.device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_COPY,
                        ..Default::default()
                    })
                    .ok()?
            };
            for n in 0..2 {
                let worker = Worker::new(&host.device, queue.clone()).ok()?;
                std::thread::Builder::new().name(format!("uploads ahead {n}")).spawn(move || worker.work()).ok()?;
            }
            Some(Queue(queue))
        })
        .as_ref()
        .map(|q| &q.0)
}

struct Worker {
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
}

// SAFETY: a worker's objects are its thread's own; the queue is free-threaded.
unsafe impl Send for Worker {}

impl Worker {
    fn new(device: &ID3D12Device, queue: ID3D12CommandQueue) -> windows::core::Result<Worker> {
        // SAFETY: plain object creation.
        unsafe {
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COPY, &allocator, None)?;
            list.Close()?;
            Ok(Worker { queue, allocator, list, fence: device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? })
        }
    }

    fn work(self) {
        let mut value = 0;
        loop {
            let mut jobs = Vec::new();
            {
                let mut queue = QUEUE.0.lock().unwrap_or_else(|e| e.into_inner());
                while queue.is_empty() {
                    queue = QUEUE.1.wait(queue).unwrap_or_else(|e| e.into_inner());
                }
                let mut bytes = 0;
                while bytes < BATCH.0 && jobs.len() < BATCH.1
                    && let Some(job) = queue.pop_front()
                {
                    bytes += job.staging.read as u64;
                    jobs.push(job);
                }
            }
            value += 1;
            let result = self.copy(&jobs, value);
            settle(jobs, result, safe());
        }
    }

    fn copy(&self, jobs: &[Arc<Job>], value: u64) -> windows::core::Result<()> {
        for job in jobs {
            job.staging.refresh();
        }
        // SAFETY: this worker's allocator and list, whose last submission has
        // run; the jobs hold what the list names until after the wait.
        unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;
            for job in jobs {
                job.record(&self.list);
            }
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast()?)]);
            self.queue.Signal(&self.fence, value)?;
            // (Without an event: this waits.)
            self.fence.SetEventOnCompletion(value, windows::Win32::Foundation::HANDLE::default())?;
        }
        Ok(())
    }
}

impl Job {
    /// # Safety
    /// `list` is an open copy list, and the job lives until it has run.
    unsafe fn record(&self, list: &ID3D12GraphicsCommandList) {
        let (s, texture) = (&self.staging, &self.copy.texture);
        for (i, footprint) in s.footprints.iter().enumerate() {
            let to = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: a borrowed pointer the location never releases.
                pResource: unsafe { std::mem::transmute_copy(texture) },
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: s.first - s.level0 + i as u32 },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: as above.
                pResource: unsafe { std::mem::transmute_copy(&s.buffer) },
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT { Offset: s.base + footprint.Offset, ..*footprint },
                },
            };
            // SAFETY: the caller's.
            unsafe { list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
        }
    }
}

/// What a batch's copy came to. A copy that did not run is logged, and its
/// textures are the lists' ([`safe`]; as before, they were counted as
/// uploaded all the same, and `closed` is false).
fn settle(jobs: Vec<Arc<Job>>, result: windows::core::Result<()>, closed: bool) {
    if let Err(e) = &result {
        tracing::warn!(error = %e, "uploads ahead did not run");
    }
    finish(jobs, result.is_ok() || !closed);
}

/// The jobs' copies have run (`copied`), or have not: those textures were
/// not uploaded, nothing that waited for them is called, and the lists
/// upload them.
fn finish(jobs: Vec<Arc<Job>>, copied: bool) {
    for job in &jobs {
        if copied {
            job.staging.uploaded.store(true, Release);
            job.staging.dirty.store(false, Release);
            job.staging.note_held();
            job.copy.ahead.store(DONE, Release);
        } else {
            job.copy.ahead.store(LISTED, Release);
        }
        BYTES.fetch_sub(job.staging.read as u64, Relaxed);
    }
    for job in &jobs {
        if !copied {
            *job.then.lock().unwrap_or_else(|e| e.into_inner()) = State::Failed;
            continue;
        }
        let us = job.asked.elapsed().as_micros() as u64;
        let n = STATS[0].fetch_add(1, Relaxed) + 1;
        let bytes = STATS[1].fetch_add(job.staging.read as u64, Relaxed) + job.staging.read as u64;
        let all = STATS[2].fetch_add(us, Relaxed) + us;
        let most = STATS[3].fetch_max(us, Relaxed).max(us);
        if n == 1 || n.is_multiple_of(5000) {
            tracing::info!(
                "textures uploaded ahead of the lists (VICTORY_UPLOAD_AHEAD=1), since the start: {n}, {} MB, {:.1} ms from the asking until they were there, the longest {:.0}; {} turned down, {} waited for by lists, {} of those found the texture taken and its upload not made yet (VICTORY_AHEAD_SAFE=0: they went on to record their own)",
                bytes >> 20,
                all as f64 / n as f64 / 1e3,
                most as f64 / 1e3,
                STATS[4].load(Relaxed),
                STATS[5].load(Relaxed),
                STATS[6].load(Relaxed)
            );
        }
        let then = std::mem::replace(&mut *job.then.lock().unwrap_or_else(|e| e.into_inner()), State::Done);
        if let State::Pending(calls) = then {
            for f in calls {
                f();
            }
        }
    }
    // Off the list of those on their way only after their `then` calls have
    // noted their slots, so that [`drain`] returning means the notes are
    // there for the next texture pass to make.
    {
        let mut way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
        way.retain(|(serial, _)| !jobs.iter().any(|job| job.copy.serial == *serial));
        IN_FLIGHT.store(way.len(), std::sync::atomic::Ordering::Release);
        LANDED.notify_all();
    }
    if copied {
        crate::descriptor::host::flip_now();
    }
}

/// Whether `resource`'s host copy was filled here: no list still brings what
/// it shows.
pub(crate) fn there(resource: This) -> bool {
    crate::resource::made_copy(resource).is_some_and(|copy| copy.ahead.load(Acquire) == DONE)
}

/// The state of [`HostTexture::ahead`] of a new host copy.
pub(crate) fn fresh() -> AtomicU8 {
    AtomicU8::new(NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by a test to keep [`claim_as`] of the host copy [`GATED`] names
    /// between taking its texture and making its staging buffer.
    static STAGING_GATE: Mutex<()> = Mutex::new(());
    static GATED: AtomicU64 = AtomicU64::new(u64::MAX);
    /// One test at a time uses the gate.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    pub(super) fn pause_staging(serial: u64) {
        if GATED.load(Relaxed) == serial {
            drop(STAGING_GATE.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    /// A placed 128x128 BC7 texture (as the tests below), asked to be made;
    /// `None` without a host GPU, or if the pool did not make it fresh.
    fn placed(shade: u8) -> Option<(This, Arc<HostTexture>)> {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        crate::host::host()?;
        let desc = [3u32, 0, 0, 0, 128, 0, 128, 0x8_0001, 98, 1, 0, 0x105, 0, 0];
        let whole = crate::resource::placed_bytes(&desc).unwrap() as usize;
        // SAFETY: memory of our own that stays for the test.
        let at = unsafe { VirtualAlloc(std::ptr::null(), whole, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) }.cast::<u8>();
        // SAFETY: as above.
        unsafe { std::ptr::write_bytes(at, shade, whole) };
        let texture = crate::resource::texture_at(at as u64, &desc).unwrap();
        crate::resource::want_level(texture, 0);
        let copy = crate::resource::host_copy(texture)?;
        crate::pool::fresh(&copy.texture).then_some((texture, copy))
    }

    /// A copy that did not run ("uploads ahead did not run"): as before
    /// (`closed` false) its texture was counted uploaded, shown, and kept
    /// from the lists; closed ([`safe`]) it is not uploaded, what waited for
    /// it is never called, and the lists have it.
    #[test]
    fn a_copy_that_did_not_run_does_not_count_as_uploaded() {
        use std::sync::atomic::AtomicBool;
        for closed in [false, true] {
            let Some((texture, copy)) = placed(0x31 + u8::from(closed)) else { return };
            let Some(Taken::New(job)) = claim_as(texture, 0, u64::MAX, closed) else { panic!("not taken") };
            let (early, late) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let flag = early.clone();
            job.then(move || flag.store(true, Relaxed));
            // (The worker's copy is not run: this is what it does when it fails.)
            settle(vec![job.clone()], Err(windows::Win32::Foundation::E_FAIL.into()), closed);
            let flag = late.clone();
            job.then(move || flag.store(true, Relaxed));
            let uploaded = job.staging.uploaded.load(Acquire);
            let way = ON_THEIR_WAY.lock().unwrap_or_else(|e| e.into_inner());
            assert!(!way.iter().any(|(serial, _)| *serial == copy.serial), "off the list of those on their way");
            drop(way);
            if closed {
                assert!(!uploaded, "a copy that did not run is no upload");
                assert!(!there(texture), "...and no list may be told that the card has the texels");
                assert_eq!(copy.ahead.load(Acquire), LISTED, "the lists' now");
                assert!(!early.load(Relaxed) && !late.load(Relaxed), "nothing waits for a copy that did not run");
            } else {
                // The bug of VICTORY_AHEAD_SAFE=0.
                assert!(uploaded, "as before: marked uploaded although the copy did not run");
                assert!(there(texture));
                assert!(early.load(Relaxed) && late.load(Relaxed));
            }
        }
    }

    /// A texture is taken, its staging buffer being made, and a list is about
    /// to record an upload of it: as before (`closed` false) the list found
    /// nothing on its way and went on, and both uploads ran; closed ([`safe`])
    /// the texture is on its way from the moment it is taken, and the list
    /// waits for it.
    #[test]
    fn a_list_waits_for_a_texture_that_is_being_taken() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        for closed in [false, true] {
            let Some((texture, copy)) = placed(0x51 + u8::from(closed)) else { return };
            let gate = STAGING_GATE.lock().unwrap_or_else(|e| e.into_inner());
            GATED.store(copy.serial, Relaxed);
            let at = texture as usize;
            let claiming = std::thread::spawn(move || claim_as(at as This, 0, u64::MAX, closed));
            let begun = std::time::Instant::now();
            while copy.ahead.load(Acquire) != PENDING {
                assert!(begun.elapsed().as_secs() < 20, "the texture was not taken");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let (told, heard) = std::sync::mpsc::channel();
            let listing = copy.clone();
            let waiter = std::thread::spawn(move || {
                listed(&listing);
                let _ = told.send(());
            });
            let returned = heard.recv_timeout(std::time::Duration::from_millis(200)).is_ok();
            GATED.store(u64::MAX, Relaxed);
            drop(gate);
            let Some(Taken::New(job)) = claiming.join().unwrap() else { panic!("not taken") };
            let (landed, landing) = std::sync::mpsc::channel();
            job.then(move || landed.send(()).unwrap());
            send(&job);
            assert!(landing.recv_timeout(std::time::Duration::from_secs(20)).is_ok(), "the copy ran");
            waiter.join().unwrap();
            if closed {
                assert!(!returned, "the list waited for the texture being taken");
            } else {
                // The race of VICTORY_AHEAD_SAFE=0.
                assert!(returned, "as before: the list did not wait, with the texture pending");
            }
        }
    }

    /// The texels of a placed texture arrive by the copy queue, all levels of
    /// them, and what waits for them is called then; a second asking finds the
    /// first on its way, and a later one is the list's.
    #[test]
    fn a_placed_texture_is_filled_ahead_of_the_lists() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        let Some(host) = crate::host::host() else { return };
        // 256x256 BC7, nine levels.
        let desc = [3u32, 0, 0, 0, 256, 0, 256, 0x9_0001, 98, 1, 0, 0x105, 0, 0];
        let whole = crate::resource::placed_bytes(&desc).unwrap() as usize;
        // SAFETY: memory of our own that stays.
        let at = unsafe { VirtualAlloc(std::ptr::null(), whole, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) }.cast::<u8>();
        // SAFETY: as above.
        unsafe { (0..whole).for_each(|i| at.add(i).write((i * 13 + i / 509) as u8)) };
        let texture = crate::resource::texture_at(at as u64, &desc).unwrap();
        crate::resource::want_level(texture, 0);
        let copy = crate::resource::host_copy(texture).unwrap();
        if !crate::pool::fresh(&copy.texture) {
            return;
        }
        assert!(!there(texture));
        let Some(Taken::New(job)) = claim(texture, 0, u64::MAX) else { panic!("not taken") };
        let (told, heard) = std::sync::mpsc::channel();
        let also = told.clone();
        let Some(Taken::Known(same)) = claim(texture, 0, u64::MAX) else { panic!("not known") };
        assert!(Arc::ptr_eq(&job, &same));
        job.then(move || told.send(1).unwrap());
        same.then(move || also.send(2).unwrap());
        send(&job);
        let wait = std::time::Duration::from_secs(20);
        assert_eq!((heard.recv_timeout(wait), heard.recv_timeout(wait)), (Ok(1), Ok(2)));
        assert!(there(texture) && claim(texture, 0, u64::MAX).is_none());
        // Level 0 as the card has it, and as the layout says.
        let layout = crate::resource::texture_memory(texture)
            .and_then(|(_, d)| crate::resource::texture_layout(&d).map(|t| t.planes[0].layout.clone()))
            .unwrap();
        let (pitch, rows) = (1024usize, 64usize);
        let mut expected = vec![0u8; pitch * rows];
        // SAFETY: the memory above.
        let tiled = unsafe { std::slice::from_raw_parts(at, whole) };
        layout.detile(tiled, 0, 0, &mut expected, pitch, pitch * rows).unwrap();
        let back = host.read_back_buffer((pitch * rows) as u64).unwrap();
        let list = host.new_list().unwrap();
        list.close();
        list.reset();
        let footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            Offset: 0,
            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(98),
                Width: 256,
                Height: 256,
                Depth: 1,
                RowPitch: pitch as u32,
            },
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(&back) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above.
            pResource: unsafe { std::mem::transmute_copy(&copy.texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        // SAFETY: an open list, live resources; the mapping spans the buffer.
        unsafe {
            list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
            list.close();
            host.execute(&[&list]);
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).unwrap();
            let seen = std::slice::from_raw_parts(data.cast::<u8>(), pitch * rows);
            assert!(seen == expected.as_slice(), "level 0's blocks");
            back.Unmap(0, None);
        }
    }

    /// One on its way: the wait ends after its `then` calls have run, and a
    /// short limit gives up on one that was never sent. (Modelled on a test
    /// of codec-d/cool_fox's `drain`. The jobs on their way are shared with
    /// other tests, so the counts here are lower bounds.)
    #[test]
    fn drain_waits_until_then_calls_have_run() {
        use std::sync::atomic::AtomicBool;
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        let Some(_host) = crate::host::host() else { return };
        let desc = [3u32, 0, 0, 0, 128, 0, 128, 0x8_0001, 98, 1, 0, 0x105, 0, 0];
        let whole = crate::resource::placed_bytes(&desc).unwrap() as usize;
        // SAFETY: memory of our own that stays for the test.
        let at = unsafe { VirtualAlloc(std::ptr::null(), whole, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) }.cast::<u8>();
        // SAFETY: as above.
        unsafe { std::ptr::write_bytes(at, 0x5a, whole) };
        let texture = crate::resource::texture_at(at as u64, &desc).unwrap();
        crate::resource::want_level(texture, 0);
        let copy = crate::resource::host_copy(texture).unwrap();
        if !crate::pool::fresh(&copy.texture) {
            return;
        }
        let Some(Taken::New(job)) = claim(texture, 0, u64::MAX) else { panic!("not taken") };
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        job.then(move || flag.store(true, std::sync::atomic::Ordering::Release));
        // Not sent yet: the limit ends the wait, with the job still there.
        let (waited, left) = drain(30);
        assert!(waited >= 1 && left);
        assert!(!ran.load(std::sync::atomic::Ordering::Acquire));
        send(&job);
        let (waited, left) = drain(20_000);
        assert!(waited >= 1 && !left);
        assert!(ran.load(std::sync::atomic::Ordering::Acquire), "a `then` call must have run before drain returns");
    }
}
