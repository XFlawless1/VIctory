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
- `VICTORY_CAPTURE_COMPOSED=1` (diagnostics): the captured frame is the one shown, the interface laid over the game. After the merge with the ambient branch this is the default (`VICTORY_CAPTURE_GAME_PLANE=1` gives the game's plane alone, as before); the switch is no longer read.

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

## The light stand-in settles by the clock (2026-11-27, ported from Belobug/VIctory-Belobug-AMD)

`VICTORY_AMBIENT_SETTLE=<time constant in ms>[,<jump factor>[,<fresh seconds>]]` (unset: `250,3`; `0`: the old 0.04 of the
way a frame). The slow mean of the light around (the stand-in's memory of the room, 20 % of every pixel's estimate) used
to follow by 0.04 a frame: 25 frames, 0.8 s at the console's 30 frames a second but 2 to 4 s at 7 to 13, so walking into a
room left it too bright or too dark for seconds. Now it follows by a share of the time (the same a second whatever the
frame rate), and a change of the mean by more than the jump factor (3, either way: a door, a lamp) is taken at once. The
optional third number bounds the title's leftover by the estimate for that many seconds after such a jump (default 0: not
done). Taken from b4ba4bbb and 9576c45f of that fork (GPL-3.0-or-later; its default is the same, from its
`reports/LIGHT-POPIN.md`), without its light probe (`lightprobe.rs`, the sums in a fourth buffer) and without its filter
of magenta probes (which it says AMD's uninitialised memory makes). This supersedes the entry above that left the series
out for its conflicts: only the part in `ambient.rs` was needed.

Tested: the settings parse (`settle_settings`), the share is the same a second at any frame rate, and the pass runs on
the card (`the_slow_mean_follows_jumps_and_ages`: half the way, a jump up and down taken at once with the age restarted,
no jump factor, the first frame). NOT shown: that rooms look better after walking in on this PC; no pictures compared.

## The frame capture saves the picture that is shown

`VICTORY_CAPTURE` read back the first plane (the game's) only, so every loading screen, title card and intro, which the
Series S title draws on the second plane (the interface, laid over the game by `compose.rs`), was saved black. It now
reads back the composed picture when the planes were laid over each other (`VICTORY_CAPTURE_GAME_PLANE=1`: the game's
plane alone, as before).

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

## Belobug #424: the states of the buffers lists use directly (2026-11-27)

`VICTORY_BUFFER_STATES` (off; `=1` on; `=0`: as before, no state tracked, no barrier recorded, no allocation: the lists are
what they were). Ported from Belobug/VIctory-Belobug-AMD #424 (by its author; its issue #350), GPL-3.0-or-later;
`host/buffers.rs` is its module, the rest by hand (`host.rs`, `draw.rs`, `indirect.rs`, `host/place.rs`). Each host list
tracks the state of the card-side buffers it binds directly (root views of a draw or dispatch, vertex and index buffers,
the source of an indirect clamp) and of those it copies into or out of. `place_copy` and `place_source` bring their
buffers back to the common state after the copy: a `CopyBufferRegion` promotes its destination from the common state to
`COPY_DEST`, nothing took it out, and the compare list `plan.check` of the same submission read it as a root shader
resource (id 538 of the debug layer, "COPY_DEST promoted from COMMON is invalid for use as a root shader resource",
#350). A list's prelude brings a buffer an earlier list of the same `ExecuteCommandLists` left in another state back to
common, and the compare list moves what is left in a write state to a read state. Not taken: the PR's debug-layer test
(`copies_in_place_leave_their_buffers_to_root_views`, ignored; it needs the layer).

**The D3D12 debug layer is not on this PC** (no `d3d12SDKLayers.dll` in `System32`, `SysWOW64` or the driver store: the
Windows optional feature "Graphics Tools" is not installed, and installing it was not asked for), so error 538 cannot be
counted before and after here. What is checked instead is what the host list records: `a_copy_into_a_host_copy_is_left_promoted_unless_the_states_are_tracked`
(`host::place::tests`) shows that without the switch a `place_copy` into a host copy in the card's memory tracks nothing
and records nothing that takes the buffer out of `COPY_DEST`, and with it the buffer ends in the common state;
`place_copy_restores_buffer_to_common_and_runs_cleanly` and the five state-machine tests of `host::buffers` are the PR's.
Whether #350's device loss has anything to do with it is not known (its author says it was not this).

**Frame time.** `tx_all1` (this switch on, with the three other ports) 44.5 ms median in the world, against 47.9 ms for
`tx_cache1` and 47.3 for `tx_old1` (neither tracks buffer states): one run each, whose spread is as large as the
differences, so no cost shows and no gain; the pairs of runs each side that were asked for were not made (the machine lock
was not free, and the time box). The recording threads do the extra work only where a draw binds a root view, a vertex or
index buffer of a host copy directly, and for `place_copy` and indirect clamps (a hash lookup and a lock a call); the log of
`tx_all1` has no device removal. Left off by default: nothing here shows the switch to help (error 538 could not be
counted), and a wrong state in a barrier is a way to break a picture; what would justify turning it on is a run with
`VICTORY_D3D12_DEBUG=1` on a PC with Graphics Tools that shows 538 gone and the picture unchanged.

## Constant buffers of tables read from a base of their own (2026-11-27)

`VICTORY_CB_BASE` (translation switch, **off by default**: `=1` on; unset or `=0` the translation is the one before, byte for
byte) and `VICTORY_CB_SPLIT` (host side, on; `=0` makes every such table whole as before; toggled, so `VICTORY_AB=<s>,VICTORY_CB_SPLIT`
runs both in one run), `VICTORY_CBSTAT=1` (diagnostics: counts and times the parts of `State::tables`). The shader cache keys on
`VICTORY_CB_BASE=1` only when it is on, so the translations cached without it keep their key; `TRANSLATOR_VERSION` stays 4
because nothing a default build translates changes (the census below), and goes to 5 in the commit that turns the switch on.

**Measured first** (prison cell, world seconds of one 240 s run, `VICTORY_CBSTAT=1`, switch off; frame 45.0 ms with the probes on,
the probes cost something): 38,460 list calls and 113 ms a frame in `with_state` over the five recording threads. `State::tables`
is 76 ms of it in the 9,300 stage calls a frame that hold a table with a constant buffer (8.1 us a call): 8,200 tables made afresh
a frame (6.6 us each, 7.9 entries each, of which 1.4 constant buffers, 3.9 other entries the stage reads, 2.6 it does not) and
4,800 found again in the list (2.2 us); the title's own tables, with none, are 1 ms. Per entry (one table in eight timed) 0.71 us
for an entry that is not a constant buffer, 0.61 for a constant buffer. So the entries that are not constant buffers of such
tables took 38 ms of the 113 (34 %) and the part the base can take away, 3.9 + 2.6 of 7.9 entries a table less what the title's own
tables cost, was estimated at 25-30 ms a frame over the threads: over the 2 ms asked for, so it was built. Two things found on
the way: the title builds a table for each draw in its heap and **most of them (about three in four of those with a constant buffer) lie
past the 1,000,000 views of the host's heap** (`host view heap capped at 1000000`, the title's holds 1,245,184), where the
title's own slots have no host slot, so "the title's own table stays the title's" cannot hold for them; and only 19 % of the
other entries of the tables that do lie in the heap had their slot made already (`mirror_current`): a draw that reads them as
the title's own table makes most of them, 1.0 us a `mirror`.

**What it does.** (Translator, `emit/lower.rs`, `emit/shader.rs`.) With the switch a load of a descriptor that a table's used list
says is a constant buffer, at a static offset (`Lower::cb_delta`), adds to its heap address the host-computed user-data dword of
kind `USER_CB_BASE` (0x107) less the table's own; every other load of the table is as before. The dword is placed right after the
highest user-data dword the program reads (not after all the layout's, as other host words are: the root signature holds 64
dwords). A table that some load of the program reads by a computed index, per lane, or in a waterfall loop is read as the title's
whole (a second round of lowering with that table left out, `Stop::CbTaint`), as is one whose slot is 64 or more. (Host,
`draw.rs`.) A table whose shader has the dword is not made afresh whole:
- its constant buffer views are made in a **block** of dynamic slots (the first slot less the first constant buffer's index, in the
  dword, wrapping, so the block holds only their span), keyed by what they hold and where they lie (and by the table's address
  where the title's own slots of it have a host slot), so draws of a list that name the same constant buffer share one block and one
  copy of its bytes at submission;
- the other entries it reads: where the table lies in the host heap, the title's own ([`State::table_entry`], as for tables with no
  constant buffer); past the host heap, a block made afresh of **only the entries the
  shader reads** (from the first of them), keyed by their records and places only (no table address), so every table of the list
  that holds the same entries shares it.
With `VICTORY_CB_SPLIT=0` such a table is whole as before and the dword is the table's own place (the shader adds zero).

**Exactness.**
- Census of the translator (`victory translate shaders.rpf --list --validate --jobs 8`, `VICTORY_HOST_WAVE=32`, 25,689 programs; each
  program's line is a hash of its DXIL and of what the host binds for it): a tree built from 62d1f226 itself and this build with
  `VICTORY_CB_BASE=0` agree on **all 25,689 (0 differ)**; with `=1` **21,848 differ** (85 %: nearly every program reads a table's constant
  buffer) and 25,570 validate in all three (the same ones; 319 fail in all three, the baseline's).
- `emit::tests::table_constant_buffers_are_read_from_their_own_base` (a card test with read-backs: the program loads a table's
  descriptor of the constant buffer, reads its last four dwords at a constant offset and its last dword at a computed one, and writes
  them to a UAV; entry 0 and entry 2 of the table, 64 B, 240 B (not a multiple of 256), 16 KiB and 64 KiB, each on WARP and on the RTX
  5080, with the switch off and on; with it on the title's own slot of the constant buffer holds other bytes and the block lies after
  the table, the base below the block's first slot for entry 2): the same dwords every time, and the translations differ and have the
  dword only with the switch. Its program words are those of Belobug's `table_constant_buffers_read_as_raw_bytes_through_their_twins`
  (VIctory-Belobug-AMD 3920dc26, renderer port 5, GPL-3.0-or-later); nothing else of that port was taken. Not covered by a test of
  its own: a root constant buffer, one picked from the heap and a constant buffer not on 256 bytes in title memory: none of the
  three is read through a table's entry, so the switch leaves the translation of those loads as it was (the census shows a program
  that has only those unchanged), and the bytes of a constant buffer are copied to 256-byte aligned memory by the code that was
  there. `draw::block_tests` checks the arithmetic of the block's base. Tests: 410 in `victory-gpu` (one new), 317 in
  `victory-shader` (one new), 151 in `victory-hle`: all pass; the counts before were not run on 62d1f226 itself (the merge run
  before the texture fixes had 391 and 295).

**Frame time and list-call time** (prison cell, world seconds, frame lines of 4,400-4,500 draws).
- In one run, the switch on and the host side toggled every 4 s (`VICTORY_CB_BASE=1 VICTORY_AB=4,VICTORY_CB_SPLIT`, 420 s; 36 world
  phases each way, the same translated shaders, the tables whole as before in the off phases): median of the phases' medians **36.85 ms
  with the host side on, 38.95 ms off**; the difference of each neighbouring pair of phases, on minus off: median **-2.20 ms** (mean
  -1.82, sd 1.72, 69 pairs, below zero in 63). That is the host side alone. What the base costs the shaders (an add and a subtract of
  user-data dwords for each constant buffer load) is not in it.
- Three single runs of 240 s with `VICTORY_CBSTAT=1` (the probes cost something, more with the switch off: one new table in eight has
  its entries made through `mirror` as well): off 45.0 ms, 38,460 list calls and 113.2 ms in list calls a frame over the threads;
  base with the host side limited to the tables in the heap (the title's own entries, no blocks shared past it) 46.0 ms, 112.6 ms (the
  title's own table for the other entries is no cheaper than copying them: 285 ns an entry in 90,000 calls a frame, against 710 ns for
  an entry copied; `tables()` 75.9 -> 72.4 ms); base as committed 37.5 ms, **90.1 ms** in list calls, `tables()` for the stages with a
  table of constant buffers 75.9 -> 55.7 ms (8.1 -> 6.1 us a call), tables made afresh 8,207 -> 4,530 new and 4,814 -> 2,656 met again, the
  submitting thread's "copies" 16.1 -> 12.6 ms (the constant buffers' bytes are copied once for every block, not once for every draw).
  Single runs: the 7.5 ms between 45.0 and 37.5 is more than the in-run pairs show (2.2), and part of it is the probes.

**Other forks' leads, read for this work.** fxceless/victory-fork #1414 (`VICTORY_TABLED_ALL=1`, 9a18dd08 and four commits after it,
KyleS98): the title's own slots of a table a draw binds count as a table's whatever they hold, so the pass over records the title
copies in as bytes leaves them to their draws. Ours has that on already: `HostHeap::slot_tabled` stamps every slot of a table a draw reads
or has made afresh (not only textures' ones), and `copied_tabled` leaves the records copied into such slots to the draws for
`COPIED_FRAMES` (8) frames; the new blocks stamp the title's slots as the tables made afresh did (all slots up to the last the stage
reads, or the ones it reads for a table of more than 64), so nothing is lost there. Its later commits are about the 240-frame window of
its own stamps (a slot that stops being read stays with its draws for 240 frames); ours is 8. Upstream #3379 (de-duplicated
`HostList::keep`, a stack array for user data, one `shadow::find`) and #3405 (a list lends the host copy it read) are not about tables
and were not taken; our `keep` is called once per heap or pipeline change, not per table entry. Vilocity's 7.9 -> 5.4 us a list call is the
batching of retired objects, which `VICTORY_RETIRE_BATCH` has.

**Left off by default.** The host side shows 2.2 ms (6 %) in one run of paired phases, and 113 -> 90 ms of list-call time, but: (1) the
picture was not compared on one frame (below), (2) the pairs of whole runs (translation off against on, 240 s each) were not made
because the machine lock was not free: only the in-run toggling of the host side was, so what the shaders' extra arithmetic costs
and what a cold shader cache costs (the first run with the switch on translates every shader it meets: 6.5 s frames in the first
world seconds of the first run) are not measured, (3) the base changes the translation of 85 % of the programs. Turning it on would
be: `CB_BASE_DEFAULT` in `emit/shader.rs`, `TRANSLATOR_VERSION` 5.

**The picture was not compared on one frame.** A frame of the prison cell was recorded with the switch on and the host side off
(`VICTORY_CB_BASE=1 VICTORY_CB_SPLIT=0 VICTORY_RECORD=<folder> VICTORY_RECORD_AT=f4200`: the tables whole as before and the base the
table's own place, so the recorded heap and root arguments serve the shaders of both translations): 45 submissions, 308 lists, 93,359
resources, 1,267 pipelines, 6.8 GB of the card's memory. It was to be replayed with the shaders of the translation off and of the one on
(`victory replay <folder> --retranslate <dir>` for each, then `--shaders <dir> --images <dir> --raw`, the textures compared texel by
texel). That did not work on this card: the replay could not make again the 2,191 buffers the frame had placed in heaps opened from title
memory (`CreatePlacedResource` refused them with E_INVALIDARG: they carry the cross-adapter flag and the heap the replay makes does not);
with the flag dropped for placed resources (tried, not kept) the frame was made and the replay then hung the card (`0x887A0006`) before
it drew anything; and `--retranslate` needs the game's directory configured (`VICTORY_GAME_DIR`), which the job did not give it. What was
kept of it: `--retranslate` takes `VICTORY_HOST_WAVE` and `VICTORY_HOST_RDNA` (default as before: 64 lanes, RDNA). So the exactness of the
host side (blocks, shared blocks, the title's own entries) is shown by the runs only: no device removal and no `ERROR` line in the six
runs made (`probe1` off; `t1`, `t2`, `t3`, `ab1` and the recorded one with the switch on), the same kinds of warnings (the one most
often logged, "a texture picked by index changed after its upload", 90 times in the switch-off run and 150-200 in the others, the
recorded run, whose tables were whole, among them: not looked into), and the frame time; no picture was compared. Captures of the same
frame number of two runs differ in 99 % of their pixels whatever the switch (the camera and the time are not the same).
