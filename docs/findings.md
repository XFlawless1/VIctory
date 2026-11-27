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

## Where a list call's time goes (RTX 5080, prison cell, 2026-11-27)

Timed with the probes of `prep/submit-path` (`VICTORY_SITES=1`; not in this branch): an `ExecuteIndirect` is 15-16 us of the
recording thread, the driver's own call 0.4-0.5 of it (`Dispatch` 0.6, `SetPipelineState` 0.7-1.0, root constants 0.1: the
calls to the host list together are about 7 % of the time in list calls). The rest is `State::bind`, and in it
`State::tables` is 63 % of all the time in list calls: the tables that hold a constant buffer are made afresh for each
draw (8,700 a frame, 7.7 entries each; every entry looked up, copied, kept and its resource handed to the global queue of
retired objects, 55,000 times a frame), and the title's own tables went through `mirror`, `readable` and `slot_tabled` at
every draw.

What was changed (all on; `=0` is the old behaviour):

- `VICTORY_READY_ENTRIES`: an entry of the title's own tables that a draw brought up to date is left as it was for
  later draws of the list that read it the same way while the list's epoch is unchanged (as `VICTORY_READY_ONCE` does for
  tables made afresh); not what shaders write through, not a texture that stands for a render target.
  `VICTORY_READY_ENTRIES=verify` reads such an entry all the same and counts the readings that added something to the
  list (diagnostics). A first form that left aliases out too: 2,332 of 67 million readings added a copy and a kept
  texture each, all views of a texture placed in a render target's memory (`texture::alias`: its source is found by the
  memory's address and can be another render target by the next draw); with aliases read every time, more than 134
  million readings in a run of 240 s, none of which added anything.
- `VICTORY_RETIRE_BATCH`: what the slots of a table made afresh let go of goes to the queue of retired objects once for
  the draw, not once each (numbered as of the end of the draw, so dropped no earlier than before).
- Not switchable, same work: `make_listed` looks into the thread's table once, not twice; `AddressHash` takes a word at a
  time (a three-word record was twelve rounds); the list's states, twins, texture set and alias table use it.
- `VICTORY_CAPTURE_COMPOSED=1` (diagnostics): the captured frame is the one shown, the interface laid over the game.

Measured inside one run with the two switches on and off in turn (`VICTORY_AB=4,VICTORY_READY_ENTRIES,VICTORY_RETIRE_BATCH`,
world phases of 4,300-4,700 draws a frame, a cycle is four phases):

| Run (the same code with the probes, branch `rec/list-call-cost`; this branch itself was built, tested and run for 100 s, not paired) | READY_ENTRIES on minus off | RETIRE_BATCH on minus off | both off | both on |
|---|---|---|---|---|
| 6 cycles (sd 0.1-0.2 ms, every cycle below 0) | **-1.44 ms** | **-1.18 ms** | 41.0 ms | 38.6 ms |

Together 2.4 ms of a 41 ms frame (6 %). An earlier run, 7 cycles, before aliases were left out of the memo and before the
constant buffer and plain slots joined the batch: -1.63 (sd 0.4) and -0.87 (sd 0.4). The changes that cannot be switched
off are in both columns. Tests: 391 in `victory-gpu` (389 and the two of the hash), 295 in `victory-shader`.

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

## kelvaroq's patch series, ported and measured here (2026-11-27)

`stellarr-studio/kelvaroq` publishes a patch series (`native/patches`, GPL-3.0-or-later) written against
fxceless/victory-fork f85101b, not against this tree: the hunks mostly miss, so what is taken was ported by hand, and each
commit names its patch. The patches are read, never run.

**0009, the cut of a parallel scan (`shadow.rs`; a bug fix, so no switch).** `Shadow::differing` compares a range of a host
copy with its snapshot on up to eight threads (one per 16 MiB, so from 32 MiB), each share starting at a cut backwards to a
piece boundary of title memory, `at - (start + at) % PIECE`. For the first share `at` is the range's own start, and when
that is below its offset into the piece (a copy whose title address is not a multiple of 64 KiB, a range at the copy's
start) the subtraction goes below zero: a panic where overflow checks are on, and in the diag and release builds a wrapped
cut that `.max(from)` leaves huge, from which nothing is scanned: the first share, 16 MiB or more, was never compared and
what shaders wrote there was not copied back. The scan is now a function over plain memory (`differing_pieces`, cut by
`scan_boundary`, saturating as kelvaroq's patch has it), and `scan_cut_tests` runs the cut on its own and the whole scan
against a serial one, over unaligned title addresses and starts: with the old formula both panic, `attempt to subtract
with overflow`; with the fix they pass. The other two cuts of this kind in the file (`find_pieces`, `copy_wide`) cannot go
below their start. Not shown: that the title has such a range (an unaligned copy start with a range of 32 MiB or more from
the copy's start); the world's big heap copy is 100-170 MB, and its start is not known to be unaligned.

**0061, `VICTORY_NO_DRAWS=1` (a measuring switch: unset, or any other value, is as before).** No host pipeline is made and no
shader is translated: `pipeline::host_pipeline` gives none, a pipeline known from earlier runs is not started when the title
creates it, and the structure builder stays off. Every draw, dispatch and indirect execution of the title is dropped and the
picture stays black; what a frame costs is the title's own code and the part of our work that does not wait for a draw (its
calls into our lists, their walk, fences, presents). Taken as kelvaroq's patch has it (a switch of the same name in Cold
Project VI); added here: the creation-time start of known pipelines is left out too, and a dropped draw is not counted by
reason (diagnostics builds took a lock and made a string for each, on the threads that replay the lists).
Measured (RTX 5080, Series S, 240 s, scripted keys to the prison, the frame lines that had 3,500+ draws and dropped
commands a frame, the first 6 of them left out): **33.3 ms (30.0 fps) in every line from the 7th on (17 of 17), one run**
(`kq_nodraws1`); 3,600-3,800 draws and dispatches and about 4,600 indirect executions dropped a frame, the submission
preparing 0.7-0.9 ms (copies 0.1 ms; no texture slot placed: nothing is made that a draw does not bind), `Present` 0.30 ms, no
device loss, no warning but the switch's own, the black frames and the present-plane notes. The title's cutscenes show 33.3
ms in the same build with draws too, so 33.3 ms is the title's own limiter for 30 fps (the host presents with sync interval
0): with every draw dropped the title and our walk of its lists fit inside it, and the frame time cannot show how far below.
The same keys without the switch, same build (the runs section below has them all): the cell's frame settles at 39.2 ms
(base2, the last 8 quiet-cell lines, 4,440 draws a frame; the whole cell's median 40.5 ms), 44-46 ms before it does so (and
in two of five quiet runs only at the very end or not at all within 240 s), and in one run (base1) the scene went on to a
state with 6,200 draws and 59 ms.
What the gap says: the title's code is not the wall: it and our walk fit inside 33.3 ms with every command of a frame
recorded and dropped (3,700 draws and dispatches and 4,600 indirect executions a frame); of the cell's 39 ms at least 5.7 ms
(15 %) is drawing, of the 44-46 ms before the settle at least 11-12 ms: what our host does for draws (translation's pipelines,
copies, uploads, descriptors, the replay of the lists) and the card. Not shown: the real floor under the cap (burning CPU in
the list calls until the frame leaves 33.3 ms would give it; `VICTORY_BURN_CPU_US` burns on the submitting thread only; one
`VICTORY_NO_DRAWS=1` run, a second was not made), and whether the title takes other paths when no GPU result comes back (it
did not stall or crash in this run).

**0042's counters of the clamps (diagnostics: the frame line's neighbour "indirect clamps per frame").** kelvaroq's 0042
counts the clamps of one execution or window by the kind of their records; here those are counted too, and so are our
batches (`indirect::batch`, 256 jobs a pass): the passes, the executions that read them by kind, and what a batch held when
it ended (mesh dispatches only, others only, both). The patch's other half (why a merge under flights' shader writes gave
up, `VICTORY_LATE_WAIT=2` only) is not taken. In the cell (RTX 5080), 4,460 draws a frame: 155 passes a frame, none alone
or in windows, 3,260-3,280 mesh dispatches, 900-910 dispatches, 260 draws and 83 indexed draws reading their records from
them, in 25 batches of mesh dispatches only, 124 of dispatches and draws only and 6 of both; in the state with 6,200 draws:
161-166 passes, 2,300-2,480 mesh dispatches, 940-960 dispatches, 18-20 batches of mesh only, 128-130 of the others, 15 of
both. Two full waits for every earlier shader write per pass: about 310 a frame, against the title's own barrier groups' 690
(the cell) to 780 (the other state).

**0041, the mesh launch (not ported: nothing to gain that the numbers show).** The patch's hash half is here already
(`CARD_COMPARE` is 64 threads a piece). Its launch half puts an amplification stage of its own in front of every mesh
shader, which reads the title's record where the title wrote it, bounds and charges it as the clamp does, and launches the
groups, so that an indirect mesh draw needs no clamp pass before it; kelvaroq measured it against an unbatched clamp (two
full waits for every record: 3,200 of 3,950 full waits in a frame). What was measured here, in order:
- The bench this tree has (`indirect::bench`, `cargo test -p victory-gpu --features diagnostics --lib indirect::bench --
  --ignored --nocapture`; RTX 5080, 3,200 one-record draws a frame, medians of 9): windows of 32 as `main` clamped them
  30.40 ms, as this tree does 0.64 ms (-97.9 %); one at a time 14.19 and 13.97 ms. That is kelvaroq's issue 22
  ("3,200 draws 30.54 -> 0.64 ms"): the cost of an unbatched clamp that the patch removes is gone here since the
  barriers and the one charge per group, and the batched pass (`VICTORY_INDIRECT_BATCH`) after them.
- What is left is the batched passes: 155 a frame in the cell (counters above), each with two full waits, 310 against the
  title's 690. The batch is shared by all kinds, and the patch can take out only the mesh dispatches' clamps: 25 of the 155
  batches hold mesh dispatches only (and 6 more some), 124 hold only dispatches and draws, which the launch does not touch.
  Even if all 155 passes cost what the bench's cost (0.64 ms for 100 passes and 3,200 draws, so at most 6.4 us a pass on an
  idle card), the 25 are at most 0.16 ms of a frame of 40-57 ms, and nothing in the 130 others' way changes.
- The clamp passes' waits (all 155 passes' two each, not only the mesh ones') taken out in a world run
  (`VICTORY_BARRIER_TEST=indirect`, for timing only: nothing then orders a clamp after the title's writes), which is more
  than a launch could save: the cell's frame 39.2 ms with them, 39.0 ms without (last 8 quiet-cell lines of base2 and bt2;
  the runs section below); bt1, the other run, was 43.5 at the end, and only because it had not settled yet.
- Cannot be done as asked here either: the amplification stage is shader model 6.6, and the host shaders of this tree are
  compiled by `D3DCompile` (FXC, `cs_5_1` and the like); `dxc` is found only by the tests, in the Windows SDK. The patch's
  stage is a compiled DXIL blob (`launch_code.rs`), not taken. Our DXIL emitter (`victory_shader::emit`) has no
  amplification stage, no `DispatchMesh` and no payload; writing them is new surface, and the translated mesh shaders would
  all declare a payload (`MESH_PAYLOAD_BYTES`), so `TRANSLATOR_VERSION` would go up and every cache start cold.
So the measurement says the clamp cost left is small and mostly not in mesh dispatches' share; nothing is changed.

**0046 and 0048, presents behind the lists, two frames ahead (not ported: this tree has it, and the cost is not here).** The
patch makes the title's `Present` return at once: it takes one of `VICTORY_FRAMES_AHEAD` slots (2 by default), puts the
present into the queue's line behind the lists and signals before it (`Held::Present`), and the copy to the back buffer runs
when the line reaches it, from a texture the present holds; the slot is given back when the card has run that copy. A
title's blocking fence wait (no event) waits on an event of its own that the relay publishes; the worker looks at a blocked
head every 4 ms; `VICTORY_EARLY_BARRIERS=1` (off) records the preludes while the relay lands bytes; `frame_wait.rs` times
both sides of the handoff. 0048 makes it the default. In this tree the title's `Present` already does not wait for the card
with the relay (`VICTORY_RELAY=2` is in the play set: `late_free_present`, the copy is a flight of its own, `Present` is told
behind it), and with `VICTORY_ASYNC_SUBMIT=1` all queue calls, presents included, go through one line served by a thread of
ours (`queue/behind.rs`: `VICTORY_SUBMIT_FRAMES` presents ahead, 1 by default; the title drains only before a read-back or a
blocking fence wait). Measured: the host's `Present` call blocks 0.30-0.45 ms a frame in every world run of this session and
in a whole 81-line run of 2026-11-26 (including a 184 ms frame: 0.5 ms), and the flights are settled 0.0 times a frame in all
of them: the 0.1 ms and the 10-12 ms of settles that the patch removes are not in these frames. What would be new: the slot
that stays until the card has run the present copy, the event-based fence wait, the 4 ms look, the early preludes; none
addresses a cost this tree shows. Our own equivalent (`VICTORY_ASYNC_SUBMIT=1`, `VICTORY_SUBMIT_FRAMES=2`) against the
default: 39.2 ms (base2) and 38.9 (async2) once settled, async1 had not settled at the end (44.0); the thread of ours worked
35-39 ms of every frame of 40-44, the title waited 0.0 ms at presents and for room in both modes: no gain to take (the
runs section below).

**Runs of this section (RTX 5080, Series S, 240 s each, `run_game_repo.ps1`, the keys that reach the prison in about 4 minutes,
the diag build of the branch's tip; no device loss in any; `Present` blocked 0.30-0.45 ms a frame in all).** The cell
differs between runs: the log's draws a frame say 4,430-4,530 in five runs and 6,200 in one (base1: the scene went on to the
guard at the door), and the cell's frame is 44-46 ms for the first minute or two and 39 ms after a step down, which came
well within the 240 s in three of the five quiet runs (base2, bt2, async2), in the last three lines of a fourth (bt1) and not
at all in async1. So the median of all world lines (the first 6 left out) is shown beside the median of the last 8 lines of
the quiet cell (4,300-4,800 draws a frame):

| Run | What | Draws a frame | All world lines | Last 8 quiet lines |
|---|---|---|---|---|
| base1 | as built | 6,206 | 57.2 ms (27 lines) | none (the scene had moved) |
| base2 | as built | 4,443 | 40.5 ms (26) | 39.2 ms |
| bt1 | `VICTORY_BARRIER_TEST=indirect` | 4,459 | 44.8 ms (29) | 43.5 ms |
| bt2 | the same | 4,427 | 39.8 ms (23) | 39.0 ms |
| async1 | `VICTORY_ASYNC_SUBMIT=1`, `VICTORY_SUBMIT_FRAMES=2` | 4,531 | 44.9 ms (20) | 44.0 ms |
| async2 | the same | 4,465 | 40.0 ms (29) | 38.9 ms |
| nodraws1 | `VICTORY_NO_DRAWS=1` | 0 (3,700 + 4,600 dropped) | 33.3 ms (22) | 33.3 ms |

The same configuration differs by up to 5 ms between runs (bt1 against bt2) when one has not stepped down, so differences
under that are not shown by one pair: base2 against bt2 -0.2 ms and base2 against async2 -0.3 ms are nothing, and no claim
is made from them, except that the clamp waits and the async line do not hold the frame at 39 ms either. Tests of this
tree: 393 in `victory-gpu` (391 at 3f687f24 and the two of 0009), 295 in `victory-shader`.
