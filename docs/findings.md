# What was learned on the RX 7900 XTX (2026-11-23 to 2026-11-25)

One PC: i9-12900K (8 fast cores, 8 slow ones, 24 threads), 64 GB, Radeon RX 7900 XTX
24 GB, Windows 11, 1440p, Series X profile. Figures are of diagnostics builds with a
screenshot a second. Scenes are not the same from run to run, so read differences
under about 5 % as nothing. What a switch does is in the comment where it is read.

## Where a frame goes

Median of whole runs, same part of the prologue: 280 ms, then 124, 103, 75, now 46-65.
On the water (first boat mission) 46-53 ms.

The title needs 33 ms. Of a 55 ms frame on the water:

- **The thread that prepares submissions: 48 ms.** It is the wall. Steps of the lists
  18 (refreshes of host copies 10), steps done on the card 7, waiting for descriptor
  records 6, asking the write watch 5, copies in the card's memory 4, the title's bytes
  under shader writes 4.
- **The card: about 41 ms busy**, the next wall.
- **List calls: 36,000 a frame, 230 ms of work** spread over eight threads. In a street
  chase 67,000 calls and 430 ms.
- Descriptor records copied as plain bytes: 400,000 slots looked at, 40,000 mirrored a
  frame, most of them empty.
- Landing what shaders wrote: 16 MB a frame put as dwords, on a thread of its own.

## The one idea behind most of it

On the console the CPU and the graphics processor share memory. Here they do not, so
everything one writes and the other reads must be noticed and moved. Every large cost
is one of four: **noticing** writes of the title (write watch, guards), **noticing**
writes of shaders (hashes on the card, compares on the CPU), **moving** them, and
**waiting** until an order is safe. Gains came from making each of them narrower
(dwords, not pages), wider (threads, AVX2) or later (on the card, behind the frame),
in that order of size:

| What | Frame before | after |
|---|---|---|
| Descriptor records of draws' tables left to the draws | 280 ms | about 124 |
| Our hash passes over card copies, 64 threads a piece | card 79 ms | 66 |
| Preparing on a thread of its own, refreshes and records spread | 103 | 75 |
| List calls written down and translated by eight threads (`VICTORY_THIN`) | 75 | 68 |
| Submissions joined, card steps, textures sent ahead | 68 | about 63 |
| Title bytes under shader writes compared with AVX2 on six threads | 14 ms of it | 4-7 |
| Stores into guarded memory looked at first, fewer guards reset | 69 ms of threads' time | about 6 |

(Each figure is from the run in which it was measured; they overlap and do not add up.)

## Faults found, and what they were

- **Boats sank, the sea plane went under.** The ocean's height and displacement maps are
  textures that compute passes write and the title's physics sample on the CPU. They were
  never brought back to title memory. Fixed by Geckomacabre's read-back as merged in
  fxceless/victory-fork f5c03f3b; on by default here (`VICTORY_TEXTURE_BACK=0`: off).
  With it the boat stays up through a whole run; without it sank in every one.
- **A device loss about every 16 minutes**: a page fault inside a texture made in the same
  tenth of a second. A slot showed it before its upload had run.
  `VICTORY_SHOW_AFTER=1`; none in the sessions since (likely fixed, not proven).
- **A rare lost write** (6 of 200 runs of a stress test): a view was mapped between our
  look at its guards and our lock, and views were opened one after another. None in 603
  runs after.
- **Blocky patches**: indirect dispatches cut at 65,535 groups.
- **Square holes and dashed edges in shadows**: pixel shaders that read their quad's
  neighbours with wave reads; on AMD helper lanes take no part in them.
- **Cube reflections too blurred, wrong materials**: DXIL's `Log` is base 2, and
  normalized buffer codes were one step off.
- **Every ray missed**: the dispatch flag that says counts are threads, not groups.

## Pixelated lit characters and shadows (RTX 5080, Series S profile, 2026-11-26)

Looked at with `VICTORY_DUMP_TARGETS` (every target a frame writes, 1280x720 under a 1080p output):

- **Not the upscaler.** `TAA2U:CS_TAA2_Resolve` (22f4971293b8fdcc) translates exactly (no wave exchange, no
  retry form); the title ping-pongs its two 1920x1080 history pairs and our host keeps each between frames;
  the stipple it takes in (hair is a visibility buffer, `hair_resolve:CS_ResolveToGBufferTiled_*`, resolved
  with a different dither every frame) comes out frame to frame stable in the cell: |frame N - frame N+1| of
  the lit hair 2.4 against 1.4 for film grain alone.
- **Not the ambient stand-in.** `VICTORY_AMBIENT_SHADE=0` against the default: the same noise in matching
  scenes (walls 1.68 and 1.68, shirt 1.68 and 1.68, 0..255 luma, neighbour difference).
- **Shadows are the title's shadow maps, and small ones.** In the frame with a light on the character the
  atlas of its local lights had a 128x64 and a 64x64 corner of its 1024x1024 used (probably two paraboloid
  halves of 64x64 a side); the sun's cascades about 1220x215 and 1080x200 texels of 2304x576. The character's
  shadow on a wall in that frame is a blob with steps of some 20 to 50 output pixels. The title also traces a
  half-resolution shadow mask (`sr_tracing:CS_RaytraceShadows_Mixed`, 640x360), whose rays all miss here
  (`VICTORY_BVH_MISS=1`); the structure builder that would give them something to hit could not run on 32-lane
  waves: 3 of its 53 programs (39321a95223001ed, 77360df21e12ea43, 9fc111d3be79364f) did not translate ("a
  first-lane read where not every lane runs"). The programs of `XboxBvhBuilder.dll` that stopped there are three
  block scans over 512, 512 and 256 threads (translate hashes #dc78c5de69c7c155, #322ed4ad832bdea6,
  #d441b097704ab616; the pipelines' hashes in a run's log are of the title's own copies); their loop over the
  waves below now translates exactly (`emit/wave_alike.rs`, docs/fork-ray-tracing.md), the builder
  runs on an RTX 5080 (53 of 53 pipelines), and what that does is measured in the same document. Whether the
  title makes these maps bigger on Series X is not known (it does not run here).
- **Six lighting programs translated only in an approximate form** (`VICTORY_WAVE_WHY=1`, host wave 32):
  `shadowmask:CS_GenerateShadowMasks_HairOnly`, `tiled_lighting:CS_ClusterLightsWorldSpace` (both local
  waves, any mask: each host wave gives its own half of a console wave's result) and four
  `cascadeshadows_rendering:CS_ShadowRenderTiled*PPPC` rasterizers. The first two have a thread group of one
  console wave and translate exactly with the wave-wide form, which is now on for them by default
  (`VICTORY_WAVE_WIDE=0`: as before). `VICTORY_WAVE_WIDE=1` for every program lost the card about a minute into
  a run (device removed; which program was not found): not safe for the others. Whether the approximate forms were the
  cause of anything seen is not shown.
- A seventh, found while checking what `emit/wave_alike.rs` changes in the title's 26,084 programs:
  `water_interaction_foam:CS_DisturbersContribution_d51b035d_Wrapped` (1a61a99e8379f7d6) also translates only in
  the approximate form of local waves (first-lane reads of float values where not every lane runs). A first,
  broader form of that proof (any vector ALU instruction without a cross-lane step) made it translate in the
  original form, but also accepted accumulating instructions (`v_fmac`) whose old value the IR does not list as
  an operand; the proof that is in is integer-only, so this program is as it was (approximate), and no other
  program of the title changes its translation.
- Not looked at here: NVIDIA Smooth Motion (the launcher's default) interpolates frames the capture does not
  see; `-NoSmoothMotion` is the check.

## Measured and not worth it

- Unordered early pipelines for everything (AMD's compiler crashed).
- Series S profile, smaller textures, a second drawing queue, `ASYNC_SUBMIT` off.
- Mapping pages on sixteen threads: no more pages mapped, frames longer.
- Scalar buffer loads four dwords a call (`VICTORY_SHADER_OPT=127`): no gain on this card.
- Wave sizes, fused multiply-adds, fast reciprocals, raw picked buffers, leaving out
  barriers: nothing to 0.1 ms each. Exact RDNA float rules (`VICTORY_RDNA_FLOAT`): 0.8 ms.
- Running the console's shader code natively on the card: the Windows driver takes no raw
  code, the two generations differ, and a Vulkan host layer ends at the same compiler. What
  is left to gain from shader code was guessed at under 1 ms.
- An on-screen panel of numbers (`VICTORY_PANEL`): it helps finding where a frame goes but
  costs some, so it is off and F3 shows it.

## What is next

1. The preparing thread: split what one submission does over two threads in a row; ask
   for the next submission while this one is sent; view the card's own buffers where fills
   copy them now.
2. The card: small buffers' copies in its own memory (`VICTORY_CARD_BUFFERS_MIN_KB=64`,
   after fxceless c36971b2). Measured on an RTX 5080 in the prison cell (the NVIDIA fork's
   branch, not this card): the card's time for the same commands 36.8 -> 17.3 ms a frame,
   the frame 51.8 / 50.5 -> 40.8 / 42.0 ms; the title's counters, work lists and outputs
   live in buffers of 88 bytes to 1 MiB, and the card read them over the bus. And the
   passes that run for nothing without rays.
3. Stalls: 270-660 s of a session were full of frames over 200 ms, almost all while new
   pipelines were made. `victory warm`, and a quarter of the workers for the slow ones.
4. Outdoor flicker: suspected to be one bad indirect record that uses up its list's
   budget, so that every later draw of it is empty. The log's frame line counts them;
   `VICTORY_INDIRECT_REST=0` is the experiment.
5. Memory, 40-46 GB private: lists' allocators 9 GB, windows 7, uploads 4
   (`VICTORY_MEMORY_MAP=1` lists it).

## The other repositories, compared

Read up to 2026-11-25: Marvisak/VIctory, fxceless/victory-fork, DexNom/VIctory,
Belobug/VIctory-Belobug-AMD, FQY55kai/ZJOU-VICTORY. What was taken is in the history,
each commit naming its source. In short:

- Marvisak's commits hardly apply any more; nearly all of what they fix is here, often
  another way. Taken: nearest normalized codes, d16 views, shader model flags.
- fxceless: the base. Newest taken: texture read-back, small buffers on the card, hashes
  over a buffer's own bytes.
- DexNom: thread counts of ray dispatches, cube gradients, joined sparse ranges.
- Belobug: pipeline workers for screen passes, the ambient stand-in's idea, pipeline and
  constant buffer bookkeeping (romainhedouin).
- ZJOU: render targets not set again; some shader corrections are being looked at.

## Belobug's fork, a second look (2026-11-27)

`belobug/rx9070xt`, 118 commits not in this tree (vendor-neutral fixes, ports from other forks, AMD-only work, opt-in
experiments). What was ported is in the history (`git cherry-pick -x`); what follows is what was measured. Shader
changes are checked by translating the whole of `shaders.rpf` for a host of waves of 32
(`VICTORY_HOST_WAVE=32 victory translate <shaders.rpf> --list`: 26,084 programs, 25,689 distinct translations, about
65 s on this PC) and comparing the emitted hash of each, so that any change of any program shows.

**The tiled cascade shadows (520776a0 against 6df73fdc).** Before, four of the eighteen `CS_ShadowRenderTiled*`
rasterizers translated only in the approximate form of local waves: `...CascadesPPPC`, `...CascadesCBPPPC`,
`...CascadesAlphaCBPPPC`, `...MiscAlphaPPPC`. Now all eighteen translate exactly (the form without skips, scalars
followed). `indirect_diffuse_lighting:CS_ResolveVBuffer_Voxels` went from approximate to exact with them. Those five
are the only programs of the pack whose translation changed.

**The lane-uniformity proof (Belobug 431cab5d, abbbd6b0, 7bf91de4, 7348b80a, 368293ec).** Five programs translate
differently, no other of the 25,689: `water_interaction_foam:CS_DisturbersContribution` (approximate -> the original
exact form), `deferred_tiled:CS_DepthProcessingHBO` (a first-lane read the old proof took as alike and the new one
does not: now the form without skips, 2,389 -> 2,891 host instructions), and
`indirect_light_probes:CS_Primary_RegisterAllocationRequests`, `tiled_lighting:CS_ClusterLightsWorldSpace` and
`...IntensityBased` (two group barriers and one LDS access fewer each). `VICTORY_LANE_UNIFORM=0` gives the old proof:
every program then translates as at 520776a0 (checked). The scan test needed both `VICTORY_FIRSTLANE_ALIKE=0` and this
to refuse the builder's block scans as before. `TRANSLATOR_VERSION` is 2 for it. Not ported: ad0f9dda (a lane-branch
region that only raises an LDS flag, `sr_tracing:CS_Tiling_Generate_Opaque` and nothing else of the pack: ray
tracing only, and it would make that dispatch run each frame, which is refused now), 06fe22a7 (counted wave64
builder programs: no program of the pack changes).

**Dropped or approximate programs the title runs now (520776a0, RTX 5080).** The log's "dropped N/frame without a host
pipeline: refused [compute ...]" names programs whose translation is still one of 64 lanes, which the card refuses
(`0x80070057`), each frame: `GpuParticleUpdate:CS_Visibility` (369cbe906ea72764), `alphaResolve:CS_AlphTransmissionPrefilterMip`
(2da4beb386509b77), `vfx_triangleClip:CS_DefragHeap_ComputeRelocations` (02384a60fda6f2d0) and
`sr_tracing:CS_Tiling_Generate_Opaque` (2cc4c197ee29e291); also refused when made, dispatched rarely in the cell:
`vfx_triangleClip:CS_CollectTriangles_Meshlets` (6b82bc7d22016cb5) and the two
`water_shading:CS_TransmissionVolumetrics_Tiled*` (approximate and 64 lanes). All are "LDS in a group over 32, an LDS read and write in one lane-branch region", with
`v_readlane`, a mask read as data or `v_mbcnt` besides. These are the next shader work that could show in the picture.

**Shader cache keyed by a version (1162d38f).** The key was the translator's source (including its test files), so
every commit that touched `crates/victory-shader` started cold. Checked here: `victory shaders` into an empty cache
(25,689 shaders, 70 s, 0.8 GB), then a comment added to `victory-shader`'s `lib.rs` and a rebuild: the second run found
all 25,689 translations of the first build in the cache and translating them again gave 0 different. A rebuild that
changes what the translator makes must bump `victory_shader::TRANSLATOR_VERSION` (`VICTORY_SHADER_KEY=source` keys by
the source as before); the first run of this tree translates everything once.

**What cost a lot: b68e65e0 (structured buffer views off the raw alignment, "hosts without descriptor twins").**
Here (no twins on NVIDIA's 1,000,000-slot heaps) 2,500 buffer views a frame became "off their copy's elements" (1.5
before), 350 copies of their own bytes were made a frame by the title's thread, and the world went from 41-44 ms to
175-180 ms a frame (runs belo_r1 and belo_r2: the card 12% busy, the title not submitting for 135 ms of each frame).
Dropped (kept on branch `scratch/with-b68e65e0`). Whether those views read wrong bytes without it is not known; the
picture at 41 ms looks as before.

**Runs (Series S, 240 s, scripted to the prison cell, world = lines of 3,500+ draws a frame).** Before the drop of
b68e65e0: belo_r1 174 ms median, belo_r2 (`VICTORY_SHOW_NEW_LATER=0`) 179 ms. After: belo_r3 (defaults) 43.8 ms over all
world lines, 41.0 ms after the first 12; belo_r4 (`VICTORY_SHOW_NEW_LATER=0`) 42.2 and 41.0 ms. Other runs of 520776a0's
tree today: 41.2, 45.1, 49.0 ms (prep_r1-3). No device removal in any. The texture slots shown after their upload:
0.21 frames on average and 3 at most with `VICTORY_SHOW_NEW_LATER` on (4,706 slots held until earlier lists ran),
0.19 and 2 with it off; 0.15 to 0.22 and 2 to 3 in four runs before. One pair of runs shows no difference, and nothing
here shows that the hold prevents a device loss (there was none to prevent).

**Not ported, and why.** 5d6bd841 (title textures uploaded in the submission prelude, "a frame late" is what the
comment in `texture.rs` says of the present way): conflicts in `host.rs`, `resource.rs`, `texture.rs` of 100 to 170
lines each, in the refresh path another piece of work is changing. ffc2cf6f (idle windows capped under RAM pressure):
conflicts in `shadow.rs`, off unless `VICTORY_WINDOW_BUDGET_MB` is set. c278f0d5 and 85d56416 (pooled textures at 64 KiB
multiples): a theory for device losses this PC does not have (none in these runs), and more card memory. 8699d96a
(frames counted after the swap chain accepts them): conflicts in `host.rs`, only the frame meter. 888bf1e2, 33439c8d,
1438f09d, c6ff045f: a slot scan of the heap buffer pass, and the pipeline book's writer (off in play). 0dc5d1be and
41d3184e (one interpolation mode per signature row: 48 pixel and mesh programs the validator rejects; no host pipeline of
them was refused in the logs): worth checking with `victory translate --validate`. 027e367a and d1f883ec (the
ray-traced shadow tracers `CS_TraceShadowRays_*`, 5 programs that do not translate: not run by the title here).
b4ba4bbb, bf7833a9, 9576c45f (the ambient stand-in settling by the clock, "rooms stop being too bright or dark for
seconds after walking in"): conflicts in `descriptor/host.rs` and `host.rs`. The host-copy refresh, heap-record, guard
and renderer-port commits are another piece of work's; FidelityFX, RX 9070 XT, output and scale switches, the frame
rate switch and the cache-core affinity are not wanted.

## Belobug #454: upload ahead, a copy that did not run, and a texture being taken (2026-11-27)

`VICTORY_AHEAD_SAFE` (on; `=0`: as before; A/B-able in diagnostics builds). Ported by hand from Belobug/VIctory-Belobug-AMD
#454 (by its author, GPL-3.0-or-later; its diff did not apply, this tree having `drain`, `IN_FLIGHT` and the order that
removes a job from those on their way after its `then` calls, which are kept). In `texture/ahead.rs`, and a few lines of
`descriptor/host.rs`:

- A batch whose copy did not run ("uploads ahead did not run") was finished all the same: its textures counted as
  uploaded (`there()`), their slots were shown, and lists did not upload them. Now they are the lists' (`LISTED`), what
  waited for them is not called, and no flip is made.
- `claim` made a texture `PENDING` and put it among those on their way only after its staging buffer was made. A list
  about to record its own upload (`listed`) in that while found nothing to wait for and went on: two uploads of one
  texture, and the one ahead later set `uploaded` and cleared `dirty` without looking at what the list had done. Now the
  entry is made with the taking, under the lock the list's wait takes, and whoever finds it waits (a second asking
  waits for a second at most, as lists do).
- The slot flips of an upload ahead had `after: 0`, ignoring `VICTORY_SHOW_NEW_LATER`: lists sent before the texture was
  made could read its slot by index. They now wait for the submissions begun when the texture was made, as the slots of
  the texture pass do.

**Does the bug exist here.** The code does what is said, shown by two tests (`texture::ahead::tests`), each run both ways:
`a_copy_that_did_not_run_does_not_count_as_uploaded` (without the switch the texture is `there()` and uploaded, its
`then` calls are called; with it none of that) and `a_list_waits_for_a_texture_that_is_being_taken` (a test-only gate holds
`claim` between taking the texture and making the staging buffer: without the switch `listed` returns at once, with it
the list waits until the upload has landed). It has not happened in a world run on this PC: no "uploads ahead did not run"
and no "texture not detiled" in any of the 36 run logs of the scratchpad on 2026-11-27 11:30 (21 were left at noon; with
`VICTORY_UPLOAD_AHEAD=1` all, at least 905,000 textures uploaded ahead in those 21: the failure path is not taken).
The race of the second point does happen: see the runs below.

**Runs** (Series S, 240 s, scripted to the prison cell; world = lines of 3,500+ draws a frame; one run each, on a PC whose
memory was nearly all taken, 0.5 to 1.0 GB available at the lowest; `tx_old1` all four switches of the three ports 0,
`tx_cache1` only `VICTORY_FILE_CACHE=1`, so `VICTORY_AHEAD_SAFE=0` too, `tx_all1` all four on). The statistics line of the
uploads ahead now counts the lists' waits that found a texture taken and its upload not made yet: that is the race. With
`VICTORY_AHEAD_SAFE=0` it was 92 of 1,768 waits in `tx_old1` and 99 of 1,710 in `tx_cache1` (each of them a list that went
on to record its own upload beside the one ahead); with it, 14 of 1,774 in `tx_all1`, which waited instead. (The counts of the two kinds
are not of the same events: a list that goes on may ask again for its other levels.) No "uploads ahead did
not run", no "timed out", no device removal in any of the three. World frame median 47.3 ms (`tx_old1`), 47.9 (`tx_cache1`),
44.5 (`tx_all1`); earlier runs of the base tree today were 41.0 to 47.6, so that is the PC's spread from run to run: no sign
of a cost, and none of a gain. Uploads ahead 55,000 / 65,000 / 60,000 (the line is logged every 5,000), "slots shown after
their upload ... not shown any more by then" 371 / 402 / 428. What was not seen: a copy that did not run, in the game.

## Belobug #470: read-only opens through the file cache, and a failed detiling retried (2026-11-27)

Ported from Belobug/VIctory-Belobug-AMD #470 (by its author; it takes its two halves from avatarmanz/VIctory-7900XTX
#423 and #419), GPL-3.0-or-later, by hand where the diff did not apply (`texture.rs`, `ahead.rs`).

**`VICTORY_FILE_CACHE`** (off; `=1` on; `=0` keeps the title's `FILE_FLAG_NO_BUFFERING`, as before). In
`victory-hle`'s `files.rs`: an open whose access is exactly `GENERIC_READ` and is not overlapped loses the flag on the
way to the host's `CreateFileW`/`CreateFileA`, so that reads go through the system file cache (a buffered handle also takes
reads that are not aligned); overlapped opens, opens with write access and `CreateFile2` (Bink) are as before. The log
names the first 8 unbuffered opens and every 256th, with `cached=true` if the flag was dropped. The cache is PC memory
(standby pages, given back when something asks for them); the PR has it on, this tree off until it is shown to help here.

**Measured here** (RTX 5080, Samsung 990 PRO NVMe, 31 GB of RAM of which the title and the PC leave 0.5 to 1.5 GB;
`tx_old1` against `tx_cache1`, one run each, not the two each that were asked for: the machine lock was not free): the log
says the switch was active (`cached=true` on the title's unbuffered opens, `access=0x80000000 flags=0x20000000`; only 8 opens
were named: the log names the first 8 and every 256th, so fewer than 256 were unbuffered). First frame-rate line with 1,000+ draws: 86.2 s after the
first log line without, 83.7 s with it (eight earlier runs of the base tree today, without it: 77.9 to 88.8 s). Five-second lines before
the world under 15 fps: 1 and 1, the worst 376 and 411 ms a frame, as in every earlier run. The PC's available memory (free
plus standby; sampled every 10 s) at its lowest 982 and 502 MB, at the end of the run 1,301 and 1,360 MB, the zero list 56 and
56 MB at the end; the standby list's largest 11.5 and 6.0 GB, which probably says more of what ran on the PC before each (the first sample,
before the title: 18.3 and 17.7 GB available). `tx_all1` (the cache with the other ports) 78.4 s. So no gain that is
outside the spread of the runs, and no sign of the memory cost either, on this drive; the original report's
gain (RX 7800 XT, 5 s sooner) and the PR's own NVMe measurement (unchanged, 6.1 to 6.3 s to the first frame) are the other two data. Left off
until a run on a slow drive or with a cold cache shows more. Not measured: a cold cache (reboot), the load stutter beyond
the five-second lines.

**`VICTORY_DETILE_RETRY`** (on; `=0`: as before; A/B-able). `Staging::detile` says whether it detiled, after checking every
part of the texture against the bounds `Layout::detile` checks (`Layout::check_detile`, in `victory-formats`) before it
writes any, and `Last::convert` notes the memory's digest (or the write-watch's asking) only when it did. Before, the digest
was noted before the conversion: a texture whose detiling failed ("texture not detiled") was never tried again while its
memory stayed as it was, and kept the parts the failure had written, which went up with the next upload. A job of the upload
ahead whose staging buffer is incomplete is left out of its batch and is the lists' (`refresh_all`, the others go on).
Not taken from the PR: its change to the partly mapped path that stopped noting the digest of memory that reads as
all zeros (kept as it always was); its prelude claim (this tree has no prelude upload).

**Does the bug exist here.** In the data structure, yes: `unchanged_input_retries_a_failed_detile_then_skips_a_completed_one`
(`texture::detile_retry`) runs the same refresh both ways, and as before a failed conversion is counted done and the same
memory is never converted again; with the switch it is converted again and then skipped. In a world run it has not
happened: "texture not detiled" appears in none of the 36 logs of the scratchpad (and the PR's own table of warnings
shows none either, so what it measured between its two runs is not this fix).

**Runs.** "texture not detiled" in `tx_old1` (`VICTORY_DETILE_RETRY=0`), `tx_cache1` (0) and `tx_all1` (on): 0, 0, 0, and no
device removal; the retry never ran in the game here, so what a failed detiling does to the picture was not seen, and the
only evidence of the fix is the tests (`texture::detile_retry`, `texture::ahead::tests`, `Layout::check_detile`'s). With it
on the world frame median of `tx_all1` was 44.5 ms against 47.3 and 47.9 (see #454: one run each).
