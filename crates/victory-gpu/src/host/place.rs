//! The title's buffer copies, done on the card at their place in the list
//! (`VICTORY_COPIES_IN_PLACE`, on by default; `0`: all at submission, on the
//! CPU, as before).
//!
//! A buffer copy was done in title memory when its list was submitted, before
//! any draw of the submission ran, and the host copies refreshed after it.
//! That is another order than the list's: draws ahead of the copy read what
//! it put; of two copies to the same bytes with a draw behind each, both
//! draws read the second; a source that a dispatch of the same submission
//! writes first was read as it was before it; and a destination the
//! submissions in the air still read changed under them (where its host copy
//! has one side only, `crate::shadow::card_all`).
//!
//! Now the list copies, in the destination's host copy: out of the source's
//! host copy where shaders write that one, else out of upload memory of the
//! list that takes the source's bytes when the list is submitted (a source
//! shaders never wrote is the title's CPU's, or a copy's put there earlier in
//! the submission). What is written there is a shader's write from then on:
//! the bytes are declared, and what the card made of them is put into title
//! memory before the title is told of the submission.
//!
//! Title memory takes the copy at submission as well, as it did: fills of
//! upload memory, textures and host copies that lie over the same bytes read
//! it there. The reference of the destination's host copy takes the same
//! bytes ([`Shadow::placed_copy`]), so what the landing finds is what the card
//! made of them afterwards (a counter copied to zero and counted up; a source
//! that a shader wrote in between), and nothing is sent up over it. Where
//! flights in the air may write the source or the destination this is left to
//! the landing: what they wrote is not in title memory yet, and what they
//! will put there must not stand over this copy's bytes. If a step of the
//! submission reads the destination on the CPU, it waits for them instead
//! ([`Placed::waits`]).
//!
//! Copies to memory that has no host copy yet are done as before: nothing on
//! the card has read or written it. So are those of more than 8 MiB out of
//! memory shaders do not write (streaming).

use super::*;
use crate::shadow::Shadow;

pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_COPIES_IN_PLACE").is_none_or(|v| v != "0"))
}

/// The most bytes copied out of memory shaders do not write.
const MOST: usize = 8 << 20;

/// Since the last frame-rate line: copies at their place, their bytes, those
/// out of a host copy, and texture copies out of one.
pub(super) static STATS: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// What the CPU does at submission for a copy at its place in a list.
pub(super) struct Placed {
    pub(super) dst: usize,
    pub(super) src: usize,
    pub(super) len: usize,
    /// The destination's host copy.
    pub(super) shadow: std::sync::Arc<Shadow>,
    /// Upload memory the list copies from, to be given the source's bytes.
    from: Option<usize>,
}

impl Placed {
    /// Whether flights that write `written` are waited for.
    pub(super) fn waits(&self, written: &[(usize, usize)]) -> Waits {
        let source = touches(written, self.src, self.src + self.len);
        if source && self.from.is_some() {
            return Waits::Yes;
        }
        match source || touches(written, self.dst, self.dst + self.len) {
            true => Waits::ForReaders,
            false => Waits::No,
        }
    }

    /// At submission, in the order of the steps: the list's upload memory
    /// takes the source's bytes and, if `free` (no flight in the air may write
    /// source or destination), title memory at the destination does. Gives
    /// the bytes if it took them.
    pub(super) fn prepare(&self, free: bool) -> Option<Vec<u8>> {
        if !free && self.from.is_none() {
            return None;
        }
        let mut bytes = vec![0u8; self.len];
        // (What is not there reads as zeros, as any copy's source.)
        copy_mapped(bytes.as_mut_ptr(), self.src as *const u8, self.len, false);
        if let Some(to) = self.from {
            // SAFETY: the list's own upload memory of that many bytes.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), to as *mut u8, self.len) };
        }
        if !free {
            return None;
        }
        copy_mapped(self.dst as *mut u8, bytes.as_ptr(), self.len, true);
        crate::watch::wrote(self.dst, self.len);
        // Title memory that is not all there was left alone.
        let mut there = vec![0u8; self.len];
        (read_into(self.dst as *const u8, &mut there) && there == bytes).then_some(bytes)
    }
}

/// Whether a copy waits for flights before its submission is prepared: not;
/// yes, as it takes a source on the CPU that they may write; or only if
/// another step of the submission reads its destination on the CPU.
pub(super) enum Waits {
    No,
    ForReaders,
    Yes,
}

/// Whether a step of `runs` reads one of `ranges` on the CPU.
pub(super) fn read_by(runs: &[&HostList], mut ranges: Vec<(usize, usize)>) -> bool {
    join_ranges(&mut ranges, []);
    runs.iter().any(|list| {
        list.deferred().copies.iter().any(|step| match step {
            &Step::Copy(_, src, len) | &Step::Fill(_, src, len, _) => touches(&ranges, src, src + len),
            Step::Texture(staging) => {
                let (at, len) = staging.memory();
                touches(&ranges, at, at + len)
            }
            Step::Host(_, (from, to)) => touches(&ranges, *from, *to),
            Step::Placed(p) => p.from.is_some() && touches(&ranges, p.src, p.src + p.len),
            _ => false,
        })
    })
}

/// Everything recorded so far is done, and seen, before what follows.
///
/// # Safety
/// An open list.
unsafe fn wait_for_all(host: &Host, list: &ID3D12GraphicsCommandList) {
    static ENHANCED: OnceLock<bool> = OnceLock::new();
    let enhanced = *ENHANCED.get_or_init(|| {
        let mut o = D3D12_FEATURE_DATA_D3D12_OPTIONS12::default();
        // SAFETY: a feature structure of its size.
        let known = unsafe {
            host.device.CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS12,
                (&raw mut o).cast(),
                size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS12>() as u32,
            )
        };
        known.is_ok() && o.EnhancedBarriersSupported.as_bool()
    });
    // SAFETY: the caller's.
    unsafe {
        if enhanced && let Ok(list) = list.cast::<ID3D12GraphicsCommandList7>() {
            let all = [D3D12_GLOBAL_BARRIER {
                SyncBefore: D3D12_BARRIER_SYNC_ALL,
                SyncAfter: D3D12_BARRIER_SYNC_ALL,
                AccessBefore: D3D12_BARRIER_ACCESS_COMMON,
                AccessAfter: D3D12_BARRIER_ACCESS_COMMON,
            }];
            list.Barrier(&[D3D12_BARRIER_GROUP {
                Type: D3D12_BARRIER_TYPE_GLOBAL,
                NumBarriers: 1,
                Anonymous: D3D12_BARRIER_GROUP_0 {
                    pGlobalBarriers: all.as_ptr(),
                },
            }]);
            return;
        }
        let alias = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_ALIASING,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Aliasing: std::mem::ManuallyDrop::new(D3D12_RESOURCE_ALIASING_BARRIER {
                    pResourceBefore: std::mem::ManuallyDrop::new(None),
                    pResourceAfter: std::mem::ManuallyDrop::new(None),
                }),
            },
        };
        let uav = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                    pResource: std::mem::ManuallyDrop::new(None),
                }),
            },
        };
        list.ResourceBarrier(&[alias, uav]);
    }
}

/// The host copy of `[src, src + len)` if shaders write it: what it holds at
/// a place in a list is not what title memory holds at submission.
fn shaders_own(src: usize, len: usize) -> Option<std::sync::Arc<Shadow>> {
    crate::shadow::peek(src as u64, len as u64).filter(|s| s.written() && !s.in_place())
}

impl HostList {
    /// Copies `len` bytes of title memory at `src` to `dst`, at this place of
    /// the list (see the file). `false` if it is to be done at submission.
    pub(crate) fn place_copy(&self, dst: usize, src: usize, len: usize) -> bool {
        if !on() {
            return false;
        }
        let Some(host) = host() else {
            return false;
        };
        let Some(to) = crate::shadow::peek(dst as u64, len as u64).filter(|s| !s.in_place()) else {
            return false;
        };
        let own = shaders_own(src, len);
        let scratch = match &own {
            Some(_) => None,
            None if len <= MOST => match self.scratch_at(len as u64) {
                Some(scratch) => Some(scratch),
                None => return false,
            },
            None => return false,
        };
        let at = to.offset(dst as u64);
        // (`VICTORY_BUFFER_STATES`: the buffer between is made before anything
        // of the list's state changes; none of the barriers below is
        // recorded without it, nor is the buffer made here.)
        let between = match &own {
            Some(from) if super::buffers::on() && from.buffer.as_raw() == to.buffer.as_raw() => {
                let Some(between) = self.between(host, len as u64) else {
                    return false;
                };
                Some(between)
            }
            _ => None,
        };
        // What the title's CPU wrote around and under it so far goes first.
        to.refresh(self, dst as u64, len as u64);
        // SAFETY (both): an open list; live buffers (the list holds the
        // copies and its scratch memory), ranges inside them.
        unsafe { wait_for_all(host, &self.list) };
        match (&own, &scratch) {
            (Some(from), _) => {
                from.refresh(self, src as u64, len as u64);
                let offset = from.offset(src as u64);
                if from.buffer.as_raw() == to.buffer.as_raw() {
                    let Some(between) = between.or_else(|| self.between(host, len as u64)) else {
                        return false;
                    };
                    // The copy out reads the buffer, then the copy back writes
                    // it: out of the state the first left it in, into the
                    // second's, and back to the common state at the end, for
                    // the root views that follow.
                    self.buffer_read(&from.buffer, D3D12_RESOURCE_STATE_COPY_SOURCE);
                    // SAFETY: open list; live buffers; ranges inside them.
                    unsafe {
                        self.list.CopyBufferRegion(&between, 0, &from.buffer, offset, len as u64);
                        let mut mid = vec![transition(&between, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE)];
                        if let Some(b) = self.buffer_write_barrier(&to.buffer, D3D12_RESOURCE_STATE_COPY_DEST) {
                            mid.push(b);
                        }
                        self.list.ResourceBarrier(&mid);
                        wait_for_all(host, &self.list);
                        self.list.CopyBufferRegion(&to.buffer, at, &between, 0, len as u64);
                        if let Some(b) = self.buffer_common_barrier(&to.buffer) {
                            self.list.ResourceBarrier(&[b]);
                        }
                    }
                } else {
                    // Out of the state an earlier read of the list left the
                    // source in, into the destination's copy state, and both
                    // back to the common state.
                    let mut pre = Vec::new();
                    pre.extend(self.buffer_read_barrier(&from.buffer, D3D12_RESOURCE_STATE_COPY_SOURCE));
                    pre.extend(self.buffer_write_barrier(&to.buffer, D3D12_RESOURCE_STATE_COPY_DEST));
                    let mut post = Vec::new();
                    post.extend(self.buffer_common_barrier(&to.buffer));
                    post.extend(self.buffer_common_barrier(&from.buffer));
                    // SAFETY: open list; live buffers; ranges inside them.
                    unsafe {
                        if !pre.is_empty() {
                            self.list.ResourceBarrier(&pre);
                        }
                        self.list.CopyBufferRegion(&to.buffer, at, &from.buffer, offset, len as u64);
                        if !post.is_empty() {
                            self.list.ResourceBarrier(&post);
                        }
                    }
                }
                STATS[2].fetch_add(1, Ordering::Relaxed);
            }
            (None, Some((from, offset, _))) => {
                self.buffer_write(&to.buffer, D3D12_RESOURCE_STATE_COPY_DEST);
                // SAFETY: open list; live buffers; ranges inside them.
                unsafe {
                    self.list.CopyBufferRegion(&to.buffer, at, from, *offset, len as u64);
                    if let Some(b) = self.buffer_common_barrier(&to.buffer) {
                        self.list.ResourceBarrier(&[b]);
                    }
                }
            }
            (None, None) => return false,
        }
        unsafe { wait_for_all(host, &self.list) };
        STATS[0].fetch_add(1, Ordering::Relaxed);
        STATS[1].fetch_add(len as u64, Ordering::Relaxed);
        to.gpu_writes(self, dst as u64, len as u64);
        let mut deferred = self.deferred();
        deferred.copies.push(Step::Placed(Placed {
            dst,
            src,
            len,
            shadow: to,
            from: scratch.map(|s| s.2 as usize),
        }));
        deferred.title_written();
        self.changed_something();
        true
    }

    /// For a copy of `[src, src + len)` into a texture: a buffer (readable by
    /// copies) that holds what shaders wrote there, at this place of the list.
    /// `None` where shaders do not write it.
    pub(crate) fn place_source(&self, src: usize, len: usize) -> Option<ID3D12Resource> {
        if !on() {
            return None;
        }
        let host = host()?;
        let from = shaders_own(src, len)?;
        let between = self.between(host, len as u64)?;
        from.refresh(self, src as u64, len as u64);
        STATS[3].fetch_add(1, Ordering::Relaxed);
        // SAFETY: an open list; live buffers, ranges inside them.
        unsafe { wait_for_all(host, &self.list) };
        self.buffer_read(&from.buffer, D3D12_RESOURCE_STATE_COPY_SOURCE);
        // SAFETY: an open list; live buffers, ranges inside them.
        unsafe {
            self.list.CopyBufferRegion(&between, 0, &from.buffer, from.offset(src as u64), len as u64);
            // (And the source back to the common state, in the same call.)
            let mut post = vec![transition(&between, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE)];
            post.extend(self.buffer_common_barrier(&from.buffer));
            self.list.ResourceBarrier(&post);
            wait_for_all(host, &self.list);
        }
        Some(between)
    }

    /// A buffer on the card for this recording alone.
    fn between(&self, host: &Host, len: u64) -> Option<ID3D12Resource> {
        let buffer = host.uav_buffer(len.next_multiple_of(1 << 16))?;
        name(&buffer, || "bytes between copies".into());
        self.keep(&buffer);
        Some(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::buffers;

    /// Fresh title memory of `LEN` bytes with a host copy of its own (not in
    /// place), and memory to copy from. `None` without a host GPU.
    fn copy_between() -> Option<(&'static mut [u8], &'static mut [u8], usize, usize, std::sync::Arc<Shadow>)> {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        const LEN: usize = 64 << 10;
        host()?;
        // SAFETY: fresh private memory for this test.
        let dst_ptr = unsafe { VirtualAlloc(std::ptr::null(), LEN, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE).cast::<u8>() };
        // SAFETY: as above.
        let src_ptr = unsafe { VirtualAlloc(std::ptr::null(), LEN, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE).cast::<u8>() };
        assert!(!dst_ptr.is_null() && !src_ptr.is_null());
        // SAFETY: `LEN` bytes just committed.
        let (dst_mem, src_mem) = unsafe {
            (std::slice::from_raw_parts_mut(dst_ptr, LEN), std::slice::from_raw_parts_mut(src_ptr, LEN))
        };
        for (i, b) in src_mem[..256].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3).wrapping_add(7);
        }
        crate::shadow::register(0x350_0001 + dst_ptr as usize, dst_ptr as u64, LEN as u64);
        let shadow = crate::shadow::find(dst_ptr as u64, 64)?;
        (!shadow.in_place()).then_some((dst_mem, src_mem, dst_ptr as usize, src_ptr as usize, shadow))
    }

    /// Issue #350 as the code has it without `VICTORY_BUFFER_STATES` (what
    /// Belobug/VIctory-Belobug-AMD #424 found): a copy into a host copy that
    /// lives in the card's memory is a `CopyBufferRegion`, which promotes the
    /// buffer from the common state to the copy state, and nothing the list
    /// does after it takes the buffer out of that state: no state is tracked
    /// and no barrier records the way back, so the root shader resource of
    /// the compare that follows the lists in the same submission
    /// (`plan.check`, id 538 of the debug layer) reads a buffer that D3D12
    /// still has as `COPY_DEST` until that `ExecuteCommandLists` ends.
    /// (Tracked, the same copy ends in the common state.)
    #[test]
    fn a_copy_into_a_host_copy_is_left_promoted_unless_the_states_are_tracked() {
        for tracked in [false, true] {
            buffers::FORCED.with(|f| f.set(Some(tracked)));
            let Some((_, _, dst, src, shadow)) = copy_between() else { return };
            let list = host().unwrap().new_list().expect("host list");
            assert!(list.place_copy(dst, src, 256));
            let key = shadow.buffer.as_raw() as usize;
            let states = list.buffer_states.lock().unwrap_or_else(|e| e.into_inner());
            if tracked {
                let entry = states.get(&key).expect("the destination is tracked");
                assert_eq!(entry.track.current, buffers::COMMON, "back in the common state, for the root view after it");
                assert!(entry.track.needs_common);
            } else {
                assert!(states.is_empty(), "as before: nothing tracked, nothing recorded to take it out of COPY_DEST");
            }
            drop(states);
            list.close();
            buffers::FORCED.with(|f| f.set(None));
        }
    }

    /// `place_copy` (from upload scratch, after an earlier read, and within
    /// the same shadow buffer) must leave the destination shadow buffer in
    /// `COMMON` so later root views and `plan.check` in the same
    /// `ExecuteCommandLists` call can promote it without D3D12 ID 538.
    /// (Belobug/VIctory-Belobug-AMD #424's test; tracked, as that PR has it.)
    #[test]
    fn place_copy_restores_buffer_to_common_and_runs_cleanly() {
        buffers::FORCED.with(|f| f.set(Some(true)));
        let Some((dst_mem, src_mem, dst, src, shadow)) = copy_between() else {
            buffers::FORCED.with(|f| f.set(None));
            return;
        };
        let host = host().unwrap();
        let key = shadow.buffer.as_raw() as usize;
        let current = |list: &HostList| {
            let states = list.buffer_states.lock().unwrap_or_else(|e| e.into_inner());
            let entry = states.get(&key).expect("tracked destination buffer");
            (entry.track.current, entry.track.needs_common, entry.track.promoted_read)
        };
        let list = host.new_list().expect("host list");
        // 1. Fresh upload-scratch -> shadow copy leaves buffer in COMMON.
        assert!(list.place_copy(dst, src, 256));
        assert_eq!(current(&list), (buffers::COMMON, true, false));
        // 2. Subsequent compute root SRV read promotes COMMON -> NON_PIXEL_SRV.
        list.buffer_read(&shadow.buffer, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        assert_eq!(current(&list), (buffers::NON_PIXEL_SRV, true, true));
        // 3. Second `place_copy` after the read transitions NON_PIXEL_SRV -> COPY_DEST -> COMMON.
        assert!(list.place_copy(dst + 512, src, 256));
        assert_eq!(current(&list).0, buffers::COMMON);
        // 4. Same-buffer copy (`from == to`) transitions COPY_SOURCE -> COPY_DEST -> COMMON.
        assert!(shadow.written());
        assert!(list.place_copy(dst + 1024, dst, 256));
        assert_eq!(current(&list).0, buffers::COMMON);
        list.close();
        host.execute_own(&list);
        assert_eq!(&dst_mem[..256], &src_mem[..256]);
        assert_eq!(&dst_mem[512..768], &src_mem[..256]);
        assert_eq!(&dst_mem[1024..1280], &src_mem[..256]);

        // 5. Cross-list handoff in one submission: list_a leaves `shadow.buffer`
        // read-promoted (`NON_PIXEL_SRV`), and list_b runs `place_copy` into it.
        // `prelude_buffer_barriers` must bridge `NON_PIXEL_SRV -> COMMON` before list_b.
        let list_a = host.new_list().expect("host list a");
        list_a.buffer_read(&shadow.buffer, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        let list_b = host.new_list().expect("host list b");
        assert!(list_b.place_copy(dst + 2048, src, 256));
        let mut live = buffers::LiveBuffers::new();
        assert!(list_a.prelude_buffer_barriers(&mut live).is_empty());
        assert_eq!(
            live.get(&key).copied(),
            Some(buffers::LiveState { state: buffers::NON_PIXEL_SRV, promoted_read: true })
        );
        let bridge = list_b.prelude_buffer_barriers(&mut live);
        assert_eq!(bridge.len(), 1);
        assert!(!live.contains_key(&key));
        list_a.close();
        list_b.close();
        host.execute(&[&list_a, &list_b]);
        buffers::FORCED.with(|f| f.set(None));
        assert_eq!(&dst_mem[2048..2304], &src_mem[..256]);
        if let Some(msg) = host.log_messages() {
            panic!("D3D12 debug layer reported an error: {msg}");
        }
    }

    /// What copies with two sides cost to make (ignored: a measurement for
    /// `VICTORY_CARD_ALL`): buffers of 64 KiB on the card and in the PC's
    /// memory.
    #[test]
    #[ignore]
    fn small_buffers_cost() {
        let Some(host) = super::host() else {
            return;
        };
        const N: usize = 1000;
        let used = || host.video_memory().map_or(0, |m| m[1] >> 20);
        let before = used();
        let t = std::time::Instant::now();
        let card: Vec<_> = (0..N).map(|_| host.uav_buffer(1 << 16)).collect();
        let card_us = t.elapsed().as_micros() as f64 / N as f64;
        let after = used();
        let t = std::time::Instant::now();
        let pc: Vec<_> = (0..N).map(|_| host.shared_buffer(1 << 16)).collect();
        let pc_us = t.elapsed().as_micros() as f64 / N as f64;
        println!(
            "{N} buffers of 64 KiB: on the card {card_us:.0} us each, {} MB of its memory; in the PC's memory {pc_us:.0} us each",
            after.saturating_sub(before)
        );
        drop((card, pc));
    }
}
