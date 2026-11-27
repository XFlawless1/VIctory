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

## The Rockstar-logo zoom was shown in sixes, three pictures twice (2026-11-27)

`VICTORY_TEXTURE_DIRTY_FRONT=0` gives the old behaviour. Reported as a flickering loading screen: the zoom of the
Rockstar logo intro (the radial blur out of a star, then the artwork zooming in) "goes twice". Measured: the composed
pictures were A B C A B C D E F D E F (frame n, n+1, n+2 equal n+3, n+4, n+5, pixel for pixel), 51 % of the logo frames
equal to the one three before (42 of 82 in a run that saved every frame of 270 to 470; 0 of 84 and 0 of 83 for the one or
two before).

**It was not the title, and not the pacing.** `VICTORY_INTRO_LOG=<from>,<to>` (diagnostics; written to the log after the
last present) records, per present, the title's Present call and ours, the frame waits, the checks of texture memory,
the texture uploads recorded, and a hash sampled from the memory of the movie's three luma planes (3840x2160, 8 bits,
in a ring of three, each frame of the movie written into the next of them). In the run without any capture
(`run_li_b_nocap`, presents 345 to 468): the title's Presents were 33.2 ms apart (median; 124 gaps, two hitches of one
late and one early present each), the swap chain's the same, the flip wait blocked 29 ms every present and the origin
wait never. And at every one of those presents exactly one of the three movie memories held a new picture (119 of 119):
the title's movie advances by a frame every present, as on the console. What was wrong was what we did with it: of the
three textures, each was uploaded to the host copy every second time the title drew it: uploads and keeps in turns of
three, `UkkkUUUkkkUUU...` over the three luma planes (and the same for their chroma planes).

**Cause.** A texture a draw reads through a table is checked at the submission and detiled when its memory changed; the
check sets `Staging::dirty`, and the next list that samples it records an upload. The step of a list that recorded an
upload cleared `dirty` ("noted before the check below, which sets it again if it detiles"). That is the order of the
steps when the check comes after them. With `VICTORY_JOIN_PARTS` above 1 (`scripts/play-nvidia.ps1` sets 8) the
submission's refreshes are done in front of all its steps (`refresh_front`, `VICTORY_WIDE_TEXTURES`), so the check
detiled the new frame first and the step then cleared the note: the next sampling of that texture, three presents later,
found `dirty` false and recorded no upload (a keep), although the title had written another picture since; the one
after that found it set again (the check of the keep's submission detiled and nothing cleared it), uploaded, and the
step cleared it again. A texture the title changes at every draw was uploaded every second time: a picture twice,
and the one between skipped.

**Fix.** The step leaves `dirty` set if the staging buffer was detiled in the same submission, before it (a counter of
submissions, `texture::next_submission`, kept in `Staging::detiled_in`); a buffer that was not detiled in it (the
texture did not change) is cleared as before, and one detiled after the step (the order the note was written for) is
set by that detiling as before. `VICTORY_TEXTURE_DIRTY_FRONT=0`: cleared whatever came before, as it was. A texture
that changes once is uploaded once more than needed (as it was in the other order); one that changes at every draw
every time.

**Measured** (`li_d_fixcap`, `li_e_fixnocap` against `li_a_cap`, `li_b_nocap`, same build, 40 s from the start, Enter
at 12 s): over the logo frames saved every present, equal to the one three before: 51 % without the fix, 0 % with it
(0 of 83); no two logo frames of the run equal. The luma planes are uploaded at every present (124 of 124 presents
from 345, none kept). The zoom's step from one saved frame to the next (mean absolute difference of 96x54
thumbnails) grows smoothly, 8.2, 8.9, 9.7, 10.2, ... 24.4 for frames 393 to 411, where before the fix it went 14, 13,
-20, 14, 13, +26 (back to the first of the three, and on by two steps). The title's Presents with the fix are 33.33 ms
apart (sd 0.21 ms, 124 gaps, none off by 3 ms), the upload costs nothing that shows there.

The `flick_batch` metric (every third frame of 280 to 520 saved, logo frames over 400,000 bytes, share equal in size to
the one three frames before), one 40 s run each: old 51 % (23 of 45), fix 0 % (0 of 44), old with `VICTORY_JOIN_PARTS=1`
(the refreshes behind the steps again: it gives up what `VICTORY_JOIN_PARTS=8` was made for, 0.6 to 3 ms a frame in its own measurements) 0 % (0 of 44): the repeat goes
with the refreshes in front, which is what the cause above says. The pictures of the fix and of that control are the
same ones (equal PNG sizes frame for frame).

Also by that metric, the old behaviour with `VICTORY_TEXTURE_GUARD=0` (every check hashes): 51 % (29 of 57); with
`VICTORY_TEXTURE_NO_WAIT=0`: 51 % (22 of 43). The log agrees: of the 128 checks of the three luma memories in presents
345 to 468, 119 detiled a changed picture, 7 read it through the kernel, 2 found it unchanged, and none was passed over by
the guard, so the guard and the hashing were not what held a picture back. The frame waits of the title all came from one
thread (each kind), one flip wait before each present but two (the early present after a late one, the hitch above); two
threads never shared a tick there (upstream 810f4422 is a real race of `frame::wait` but not what is seen here; not ported).

Ruled out on the way, with the log of `VICTORY_INTRO_LOG` (the same run as above): the frame pacing (the title's
Presents 33.3 ms apart with and without the fix; the flip wait blocks about 29 ms before each, the origin wait never,
and a present is not a burst of three after a stall: the six presents of a cycle are 33.3 ms apart each); a slow movie
(a new picture in the movie's memory at every present); the clock, the update thread's waits, the loading work of that
moment (one of the three luma memories changed at each of 119 presents of 119, both with and without the fix: nothing
holds the title's update back). The title's frame interval is 33.333 ms, as it asks; there is no `VICTORY_FRAME_INTERVAL_MS`
here and none was needed.

Not shown: how this looks with Smooth Motion on (the frame generation was off in every run, `-NoSmoothMotion`); that
other textures the title rewrites at every draw look better (the same step is taken for all of them).

The world frame, 240 s runs (Enter every 8 s from 45 s, the prison at about 200 s), the medians of the last ten
five-second lines of each: old 42.9 and 39.2 ms, fix 38.5 and 45.2 ms (two runs each, in the order old, fix, fix, old):
the runs differ from each other by more than the two builds do, and the fix is neither shown faster nor slower. All four
reached the world with no device removal in the log; each had one or two windows of the entry into the world of 160 to
4,300 ms a frame (the old and the fixed alike). What the fix adds is the card's copy of one movie frame (12.6 MB: the luma plane and two chroma
planes) at each of the 30 presents a second, where it was every second one; the detiling was done at every present
before.

## Device-removed report: "nothing of ours was running" only when every submission had finished (Belobug PR #412, 2026-11-27)

Ported from Belobug/VIctory-Belobug-AMD PR #412 (b8d0dd96, b90f6b6b), applied as it is. Diagnostics only: no behaviour
changes and no switch. `host::removal_context` (what the log says when the host device is removed) called the submitting
thread "between submissions (nothing of ours was running on the card)" whenever it was between two submissions. With
`VICTORY_RELAY` the thread goes on while earlier submissions still run, so that line could be false. It now says "nothing
of ours was running" only when started and finished submissions are equal (`removal_phase`; otherwise "N submissions of ours
not finished: their lists may have been running on the card"), and adds the fences of the three queues that can hold work of
ours: the drawing queue's (completed of signalled), the keep-awake queue's (`VICTORY_CARD_AWAKE`) and the relay's copy
queue's (`VICTORY_RELAY_COPY`). Locks that are held elsewhere are tried, never waited for, since the report is written by
whoever finds the device removed.

Tests: `cargo test -p victory-gpu --features diagnostics --lib` 389 -> 390 (the new one is
`the_removal_phase_counts_unfinished_submissions`). **Not exercised in a run**: no device was removed in any run here, so
the new report text has only been seen through its unit test.

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

## Physical pages in sections of 2 MiB (upstream VIctory #3177, 2026-11-27)

`VICTORY_SECTION_MB=<n>` (default 2 on Windows and 0 under Wine; `0`: one section per allocation, as always). Ported by hand
from Marvisak/VIctory pull request #3177 by KainanP (the idea and first patch are RyanCase94's, fxceless/victory-fork#1089; the
PR's `physical_remap_bench` is kept, ignored), GPL-3.0-or-later. The claim: Windows' large-page rebuild worker trades a
section's physical pages one at a time and walks the section's views while it holds the section's lock, and the title asks
for its pages in one call and maps them 64 KiB at a time, so one section carries views by the hundred thousand and every map
and unmap of the title's pages waits behind that lock. In 2 MiB sections a trade walks a few dozen views. Here
`XMemAllocatePhysicalPages` makes the sections (`make_sections`), `number_pages` numbers the pages on as before, and a run of
pages (a view, a window of ours, a DirectStorage write in place, `inplace::beside`) ends at a section's end. Kept from this
tree: `title_memory_section` once for each section with its own size, the page numbers written after the page-table lock is
let go, `Section`'s window and committed bitmap per section. Not in the PR: more than 65,536 sections are never made; on the
GPU side `inplace.rs` holds sections and our whole-section views in maps (a list searched at every write was fine for four
sections, not for thousands), forgets a section when the runtime lets it go (it was kept: a later `beside` on a view of it
would have mapped a closed handle), and prints one census line a size. The `M` lines of `VICTORY_DSTORAGE_LOG` now also count
the title's mapping calls of 1 ms or more and of 10 ms or more, and the DirectStorage writes that needed more than one run of
pages (count, bytes).

**Does the stall exist here: yes, and the sections remove it.** The standard scripted run (240 s of Enters, the cell from
about 85 s), alternating old, new, old, new (`xm_o1 xm_n1 xm_o2 xm_n2`, 2026-11-27 16:25 to 18:28; the same binary both
ways, only `VICTORY_SECTION_MB` differs; frame capture off, because its saves are 16 hitches a run; `VICTORY_LOG=info,victory_gpu::frame=debug`
for a line a frame, `VICTORY_DSTORAGE_LOG` for the `M` lines). Two runs each way, not three: the machine lock's queue was an hour
long. "world" is from the first line of 1,000 draws to the end (155 s, loading the cell and then standing in it); "last 60 s" is
the standing-still part. Old = `VICTORY_SECTION_MB=0`, new = 2; each cell is the two runs, in order.

| | old | new |
|---|---|---|
| first line of 1,000 draws, s | 84.6, 86.9 | 84.1, 83.0 |
| title's mapping calls, ms per second: opening | 55.9, 80.7 | 25.2, 21.9 |
| ... world | 206.3, 119.6 | 33.5, 30.5 |
| ... last 60 s | 10.8, 13.6 | 12.7, 10.3 |
| mapping calls of 1 ms or more: opening | 464, 688 | 103, 95 |
| ... world | 4,276, 2,671 | 552, 377 |
| ... of 10 ms or more, world | 394, 212 | 4, 10 |
| longest mapping call, world, ms | 439, 189 | 26, 24 |
| GPU page-mapping copies, ms per second, world | 161.6, 103.3 | 30.8, 28.8 |
| mapping lock wait, ms per second, world | 92.4, 42.9 | 2.4, 2.0 |
| frames over 100 ms: opening | 26, 22 | 20, 19 |
| ... world | 84, 50 | 29, 17 |
| worst world frame, ms | 875, 542 | 140, 142 |
| world frame p99, ms | 179, 118 | 98, 86 |
| frames over 50 ms: opening | 62, 73 | 69, 66 |
| ... world (of 3,038 to 3,501) | 832, 779 | 1,164, 716 |
| world median, ms (3,500+ draws lines / present gaps) | 44.0, 43.6 / 44.9, 43.3 | 46.3, 42.7 / 47.7, 42.2 |
| last 60 s median, ms (present gaps) | 43.4, 41.3 | 46.2, 39.6 |
| DirectStorage writes in several runs (staged, copied): count, GB | 6,294, 6,637; 2.65, 2.78 | 7,374, 7,319; 3.06, 3.05 |
| launcher private bytes (commit) highest, MB | 23,765, 24,322 | 23,835, 23,913 |
| launcher working set highest, MB | 20,952, 21,777 | 19,430, 20,988 |
| system available memory at the end / lowest, MB | 2,849, 1,772 / 676, 1,406 | 1,927, 1,817 / 570, 911 |
| device removals | 0, 0 | 0, 0 |

What the two ways differ in: the title's mapping calls cost 4 to 6 times less in the world part, the longest took 26 ms or less (old
189 and 439), the lock the GPU's page-mapping copies share with them is free (2 ms a second against 43 and 92), and the long
frames are fewer and shorter (frames over 100 ms 84 and 50 against 29 and 17, worst frame 542 and 875 against 140 and 142, p99
118 and 179 against 98 and 86): the PR's own finding, here on an RTX 5080. What they do not differ in: the time to the
first world line (the opening's frames over 100 ms are 20 against 24 on average, its over 50 ms 68 against 68), the
standing-still part (mapping 11 to 14 ms a second either way: "standing still hid it"), and the world frame's median: the
last 60 s was 43.4 and 41.3 ms old and 46.2 and 39.6 ms new, so the spread of one way (up to 6.6 ms) is larger than the
difference between the ways; the frames over 50 ms in the world part (832 and 779 old, 1,164 and 716 new) follow that spread and
not the sections. The first new run was the slow one (host copies 15.7 ms against 14.9 and 14.5 old, landing merge 5.06 against
4.75 and 4.49, everything else the same) and the second the fastest of the four (13.7 ms, 3.88): no change of the frame is
shown. DirectStorage writes that needed more than one run of pages were already 6,300 to 6,600 (they cross allocations); the
sections add about 900 (about 340 MB more staged and copied, over the whole run, not seen in a frame line).

**Memory.** None of the figures is outside the range the old way showed between its two runs, except the lowest available
memory (570 and 911 against 676 and 1,406 MB), which follows what the PC held at the start of each run (3.9 and 6.0 GB
available against 6.4 and 19.3 GB). The title's commit is the same to 0.1 GB; about 3,100 section handles for a 6.1 GiB request
(in run n1 four requests of 110, 149, 93 and 1,989 sections, made in 0.1 to 2.9 ms each) cost nothing measurable. No device removal:
the only log lines with "device-removed" or "timeout detection" are the start-up lines that name those settings.

**Not measured.** A walk. A scripted walk from the cell (`VICTORY_KEYS=<from>-<to>:W` holds a key) was not run: no slot of
the machine lock in the time box. So the case the PR's authors measured, walking and driving, where the title maps all the
time, is not covered: the cell's world part here is loading the cell and then standing still. The 340 MB more of staged
writes is a cost the loading of the world pays and a walk may pay more of. Not run: the PR's benchmark (it maps 4 GiB: more
than the PC has free while agents work); a section size between (4 to 64 MiB); a third pair of runs; a cold cache.

**Decision.** On by default on Windows (`DEFAULT_SECTION_MB = 2`; `VICTORY_SECTION_MB=0` is the old way, and the default
under Wine), in a commit of its own, from two runs each way that agree on every number the PR's authors used (stalls, long
frames) and show no cost in the frame's median, in memory or in device losses. What would argue for turning it off: a walk or
a drive that shows the staged DirectStorage writes costing more than the stalls saved. The tests (`modules::xmem::tests`, 8 new
and the PR's ignored benchmark; `inplace::tests::many_small_sections_each_have_a_view_of_their_own`) pass with `=0` and `=2`.
