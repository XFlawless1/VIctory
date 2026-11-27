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

## Ports from Belobug's pull requests and from kelvaroq (2026-11-27)

Five changes ported by hand from third-party code (Belobug/VIctory-Belobug-AMD pull requests #442, #345 and #366;
stellarr-studio/kelvaroq patches 0033 with the mask pieces of 0047, and 0052; all GPL-3.0-or-later), each its own commit with
a switch, a card test and a census. The census is `VICTORY_HOST_WAVE=32 victory translate <shaders.rpf> --list --validate
--jobs 8` (26,084 programs, 25,689 distinct translations, about 100 s) before and after, compared by the emitted hash of
each distinct translation. 319 of the distinct programs do not come out whole, before and after every commit (195 "resources:
an address in an SGPR that is no root parameter", 119 emitted but refused by the validator, 5 "a descriptor read only as
data"); the 25,570 that validate still do after each. Tests: `cargo test -p victory-shader --lib` was 295 passed, 4
ignored at a3892b0f.

**Console threads of 2-D and 3-D groups from the wave counter (Belobug #442; `VICTORY_THREAD_TICKETS=0` is the old
translation; `TRANSLATOR_VERSION` 3).** A console-lane program in a group of more than one host wave took its console thread
from its host wave's first flat id rounded down to the wave size, exact only while each host wave holds an aligned run of
flat ids. It now takes a block of the group's LDS counter (`wave_base`, which only console wave64 programs on waves of 32
used), and a partial wave takes the last block, not any (the PR's second fix; groups of one host wave are left as they were).
The test (`console_threads_of_multidimensional_groups_are_one_each`: each thread adds 1 + its console wave's first id at its
own id) shows the bug on this RTX 5080: the old rounding is wrong for 7 of 12 shapes, 32 x 2 (64 of 64 dwords), 32 x 2 x 2
(128 of 128), 32 x 4 x 2 (256 of 256), 10 x 10 (64 of 100), 12 x 12 (96 of 144), 24 x 8 (128 of 192) and 6 x 6 (8 of 36), and
exact for 8 x 8, 4 x 4 x 4, 16 x 16, 8 x 8 x 2 and 16 x 2 x 4. With the counter all 26 runs on the card (13 shapes, console
wave32 and wave64 on waves of 32) are exact, and with `VICTORY_THREAD_TICKETS=0` the test fails (first at 32 x 2).
Tests 295 -> 298, `victory-gpu` 389 -> 389. **Census: 4 programs change**, `terrainshadows:CS_ComputeTerrainVisibility`,
`scattering_transmittance_system:CS_GatherRaymarchedLocalLights`, `TAA2:CS_TAA2_Resolve` and `TAA2U:CS_TAA2_Resolve`
(`VICTORY_WAVE_WHY=1` prints `THREAD-TICKETS` for the programs that take the counter: three 8 x 8 groups of console wave32
programs, which the old rounding numbered right on this card, and one group of 36 threads whose partial wave takes the last
block). All four validate and make their host pipelines on the card before and after. The PR's census (88 console wave32 8 x 8
programs, among them `cascadeshadows_processing`, `rtdirectionalshadows` and `shadowmask`) was of a tree without a counter;
here 8 x 8 programs of console wave64 already took it, none of those three families changes (6, 40 and 32 distinct programs),
and so nothing here links this change to the blocky shadows. What is not shown: that any of the four was wrong before (the
shapes the card gets wrong, 32-wide rows, are not among the title's programs here). World runs of this build are with those of
the last port, below.

**A skip that only ORs an empty compare into a saved mask may go (Belobug #345; `VICTORY_EMPTY_MASK_OR=0` is the old
behaviour; no measured effect, `TRANSLATOR_VERSION` stays 3).** `ir::without_skips` (the deep form of a console wave64
program on waves of 32) now also takes an `s_cbranch_execz` skip whose region is one `v_cmp` and an `s_or_b64` of VCC into a
saved mask: with no lane left both VCC dwords are zero, so the OR changes nothing, and the program stops being refused with
"a lane mask holding lanes elsewhere read as data". The PR's ir.rs hunk is taken as it is; its test hunk does not apply here
and is written again, and a wave64 program with card readbacks of both mask words at four thresholds replaces the PR's
(refused without the change, translates with it; all four thresholds right on the card). **On the title's pack it does
nothing:** `VICTORY_WAVE_WHY=1` over the whole pack (69,355 lines, every form each program is tried in) never prints that
refusal text (0 times); the nearest, "a lane mask exchanged where not every lane runs", is 138 refusals of forms, none of
which leaves a program untranslated (the 319 failures above are of other kinds). The census of the build with it (together with the next port) is byte for byte the one
before: 0 of 25,689 programs change. The four programs the title still drops each frame (`GpuParticleUpdate:CS_Visibility`,
`alphaResolve:CS_AlphTransmissionPrefilterMip`, `vfx_triangleClip:CS_DefragHeap_ComputeRelocations`,
`sr_tracing:CS_Tiling_Generate_Opaque`) end in a 64-lane form with other reasons ("LDS in a group over 32, an LDS read and
write in one lane-branch region", `v_readlane`, a mask read as data), and stay refused by the card (`0x80070057`).

**Divergent loops that leave a scalar take synchronized latches (Belobug #366, draft; `VICTORY_LOOP_SCALARS=0` is the old
behaviour; no measured effect on the title, `TRANSLATOR_VERSION` stays 3).** A console wave64 program of one console wave (a
group of exactly 64 threads, no LDS) on a host whose waves are at most 32 lanes, with EXEC loops that leave a scalar read per
lane after them, counted that scalar once in each host wave, so every lane read the count of its own host wave's iterations (or
of its own, with per-lane loop exits). `lower::scalar_loop_latches` (the PR's analysis, as it is) picks such programs and the
latches of their loops are then decided by the whole wave (`structure_wide`: both host waves vote, two group barriers a
latch); `emit` takes that form first and does not retry the program in the per-half forms. Taken without the PR's test that
starts its own test binary again to read `VICTORY_GROUP_SYNC=0`: that refusal is kept and untested. Eight card tests: the PR's
fixtures and two of ours with the longest loop in the second or in the first host wave, the program, translated with the
switch off, giving per-lane counts and with it the wave's: a divergent loop of t + 1 iterations 1..64 -> 64 in every lane;
nested loops 1, 2, 3, 4, ... -> 16; two loops in a row through a phi, 17, 23, 29, ... -> 95; a shared scalar beside a lane-local
count 11, 15, 19, ... -> 200, 201, ...; ours, 3, 6, 9, 12, ... -> 27 and 18, 21, 24, 27, ... -> 27. Controls with the same
translation either way: a uniform exit, a loop with only per-lane results, a console wave32, a group of two console waves, a
loop behind a skip. **Census: 0 of 25,689 programs change** (this port and the one before it, against the first commit's
build): the PR's conditions are narrow (one console wave, no LDS, nothing but plain scalar and lane-local vector
instructions, nested loops with one entry and one exit), and no program of the pack meets them here.

**An empty lane mask reads as 0, and a copy of an entry mask is ballotted at its source (kelvaroq 0033 and the mask pieces of
0047; `VICTORY_WAVE_ZERO_MASK=1` turns it on, it is OFF by default after the merge (9 programs change, among them the cascade shadow rasterizers, with no benefit shown and no reference check); `TRANSLATOR_VERSION` 4).** In a console wave64 program run as two host
waves, a mask read as a number needs both waves to ballot it where every lane runs. A mask that is identically empty
(`s_xor_b64 s[14:15], exec, exec`) now reads as 0 in both dwords whatever the other wave does (0033; `Bool::FALSE`), and a
mask a lane branch copies (`s_mov_b64`, or `s_and_b64` / `s_or_b64` of one value with itself) from one written in the entry
block is ballotted there (0047's `entry_mask_copied_by`). Not taken from 0047: the signature order, the 0x218 stub and the
diagnostics. Changed from the patches: the empty-mask shortcut also asks for `Structured::active` (a complement of a mask
has a FALSE lane bit where it is written and is not zero), and both pieces are behind the one switch, on by default (0033 has
its piece off by default, 0047 on). The test (a mask read in a lane branch, both dwords stored per lane): the empty mask and the
copy of EXEC are refused with the switch off and translate with it, a mask only some lanes hold is still refused, and both
give the right word on the card for 1, 32, 40 and 64 lanes. **Census: 9 programs change**, eight tiled cascade shadow
rasterizers, `cascadeshadows_rendering:CS_ShadowRenderTiledMisc`, `...MiscCB`, `...MiscCBPPPC`, `...MiscAlpha`,
`...MiscAlphaPPPC`, `...MiscHair`, `...MiscHairPPPC`, `...MiscPPPC`, and `sr_tracing:CS_RayPrePass_Alpha`. All validate and
make their host pipelines on the card (9 of 9 before and after). They are not rescued: with the switch off and on each is
translated in the same last form (the one without skips, scalars followed, waves of 32; the two before it are refused with the
same texts at the same places), so the change is inside that form, in how a mask read as a number is made where it is
provably empty or copied. That the nine come out the same on the card was not checked program by program (nothing here runs them
against a reference), and the pictures of two runs are not comparable pixel by pixel.

**Two guest lanes in each host lane (kelvaroq 0052; `VICTORY_WAVE64_PAIRED=1` turns it on, off by default; with it off the
output is the previous commit's, so no `TRANSLATOR_VERSION` change).** A last retry for a console wave64 compute program on a
host of at most 32-lane waves that no other form of this tree gives 32 lanes (or whose form needs lanes of the other host
wave): `bitcode/paired.rs` rewrites the finished module so that host lane 7 carries guest lanes 7 and 39 (every value twice),
threads / 2, ballots, lane reads, any/all, prefix and reduction taken over both copies, the guest threads numbered by a counter
at entry, an LDS fence before each LDS access, and every branch that closes an IR block taken by the whole guest wave
(`emit_paired` makes them `structure_wide`); it needs complete guest waves (a group of a multiple of 64, and with several waves
`VICTORY_GROUP_SYNC` not 0) and refuses anything else (`VICTORY_WAVE_WHY=1` prints why). Taken as it is (`paired.rs`,
`paired_tests.rs`, the hooks), hand-merged into `run()` and `emit`; `thread_tickets` is off inside it. Tests: the patch's five on
the card (an OR over the wave for 0, 1, 31, 32, 33, 63 and 64 live lanes; LDS shared between lanes and between two guest waves
(64 and 128 threads) with masked stores at four limits; a lane gather and a first active lane at four thresholds; the retry
taking only the refused width; the group shapes admitted) and ours: lane ranks of `lane & 3 == 1` by `v_mbcnt_lo` and
`v_mbcnt_hi` of the compare's mask (slot r holds lane 4 r + 1), which crosses both halves, right in the paired form and in the
exchanged form this tree has for it. `victory-shader` 295 -> 315 passed in the full run through this port (4 ignored), the
lane-rank test run alone after it. **Census with the switch off: 0 of 25,689 programs change. With it on: 28 compute programs
change** (all validate): `GpuParticleEmit` 7 (`CS_Emit_Dynamic_Model*`, `CS_EmitCounts_Dynamic_Edge`),
`GpuParticleUpdate:CS_Visibility`, `shadowmask` 2 (`CS_GenerateShadowMasks_SampleHairScreenSpace`, `...HairOnly...`),
`sr_tracing:CS_Tiling_Generate_Opaque`, `vfx_proj_decal_trail` 9, `vfx_triangleClip` 3 (`CS_DefragHeap_ComputeRelocations`,
`CS_CollectTriangles`, `CS_CollectTriangles_Meshlets`) and `water_shading` 5 (`CS_TransmissionVolumetrics`, `...Submerged`,
`..._Tiled`, `..._Tiled_LocalLights`, `..._VolumePortalsOnly`). Their host pipelines (`victory translate --pipelines`): 0 of 28
without the switch (every one `The parameter is incorrect`, 64 lanes the card does not run), 28 of 28 with it.

**What the switch does in the title's world (RTX 5080, Series S, 240 s to the prison cell; `run_game_repo.ps1`).** Default
builds log the card's refusal of 7 compute programs and drop 1.0 dispatch a frame of each of four, `GpuParticleUpdate:CS_Visibility`
(369cbe90), `vfx_triangleClip:CS_DefragHeap_ComputeRelocations` (02384a60), `sr_tracing:CS_Tiling_Generate_Opaque` (2cc4c197) and
`alphaResolve:CS_AlphTransmissionPrefilterMip` (2da4beb3); the other three refused (`CS_CollectTriangles_Meshlets`, the two
water `..._Tiled*`) are made rarely. With the switch on, **three of the four run** (no refusal, no drop logged for them, nor for
the other three), and `alphaResolve:CS_AlphTransmissionPrefilterMip` is still refused (1.0 a frame in each of about 30 log lines of each run),
as the patch's author saw on another card (that program is not among the 28). The frame lines' own "dropped" count (about 265
in the world: 263 against 265 with and without it) does not change. World frame, the
median of the world lines (3,500+ draws a frame) after the first 12 of them, in ms: with the switch on 45.9, 41.8 and 42.0 (three
runs, 19 to 25 world lines each, ranges 44.9-46.5, 41.1-46.2, 40.8-44.2); default, final build 47.4 (one run); the first-commit
build 49.7 (a first run of that build, 10 world lines) and 46.5. The same recipe gave 41.0 to 49.0 ms on this PC for the base
tree earlier today (41.2, 45.1 and 49.0 in `prep_r1-3`; 41.0 in `belo_r3` and `belo_r4` after the first 12 lines), so these
numbers show no slowdown but no speed-up either: no run of the base build was made beside them (the machine's queue was long),
and a run is warmer than the one before (shader and driver caches), the first of a build the coldest. No run ended before its time, none
logs a device removal, a panic or an access violation (the patch's author reported one crash of two paired runs, at 216 s, on
a different card and system). The frames captured at 4,800 show the same scene with and without the switch (camera and
animation differ between runs; no black-and-red blocks on the jumpsuit in either, which that author saw by eye without it);
nothing here shows a picture difference, and no frame cost of the paired form shows in these runs.
