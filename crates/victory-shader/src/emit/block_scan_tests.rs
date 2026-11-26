//! A scan over a thread group of several console waves, as the console's ray-tracing structure
//! builder writes them (its prefix sums over 128 to 512 threads): each wave scans its 64
//! threads, its last lane puts the wave's total in LDS, and after the group's barrier every wave
//! of index `w` adds the totals of the waves below it one at a time, in a loop of `w` rounds
//! that hands the running sum from round to round through a first-lane read. On a host of
//! 32-lane waves each console wave is two host waves, which cannot meet inside the loop (only
//! the waves past the first run it): the first-lane read has to be exact without the other half
//! ([`super::wave_alike`]).
//!
//! The programs here are written out as the compiler writes the idiom, not copied from the title.

use std::collections::HashSet;

use super::host::{Dispatch, Host, Root, View};
use super::wave64_tests::{lowered, native_of};
use super::{validator, wave_alike};
use crate::native::{NativeProgram, StageIo};
use crate::{ShaderError, ir, structure};

/// The lane's own count of lanes below it (`v_mbcnt_lo`/`hi`), the lane masks the scan starts
/// from, then `term`: the words that make `v5`, the thread's term.
fn prologue(term: &[u32]) -> Vec<u32> {
    let mut words = vec![
        0xD765_000D,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v13, -1, 0
        0xD766_000D,
        0x0002_1AC1, // v_mbcnt_hi_u32_b32 v13, -1, v13
        0xBE81_03FF,
        0xAAAA_AAAA, // s_mov_b32 s1, 0xaaaaaaaa
        0xBE80_03FF,
        0xAAAA_AAAA, // s_mov_b32 s0, 0xaaaaaaaa
    ];
    words.extend_from_slice(term);
    words
}

/// The wave's inclusive scan of v5 into v10 (`ds_swizzle` and masked adds, the lanes' halves
/// joined by `v_readlane` of lane 31), its total into LDS by lane 63, the group's barrier, and
/// the loop over the waves below: after it v10 holds the group's inclusive scan.
fn scan(address: u32, barrier: u32, in_loop: &[u32]) -> Vec<u32> {
    let mut words = vec![
        0xD8D4_001E, 0x0A00_0005, // ds_swizzle_b32 v10, v5 offset:30
        0xD501_000A, 0x0002_1480, // v_cndmask_b32 v10, 0, v10, s[0:1]
        0xBE80_03FF, 0xCCCC_CCCC, // s_mov_b32 s0, 0xcccccccc
        0xBE81_03FF, 0xCCCC_CCCC, // s_mov_b32 s1, 0xcccccccc
        0x4A14_1505, // v_add_nc_u32 v10, v5, v10
        0xD8D4_003C, 0x0C00_000A, // ds_swizzle_b32 v12, v10 offset:60
        0xD501_000C, 0x0002_1880, // v_cndmask_b32 v12, 0, v12, s[0:1]
        0xBE80_03FF, 0xF0F0_F0F0, // s_mov_b32 s0, 0xf0f0f0f0
        0xBE81_03FF, 0xF0F0_F0F0, // s_mov_b32 s1, 0xf0f0f0f0
        0x4A14_150C, // v_add_nc_u32 v10, v12, v10
        0xD8D4_0078, 0x0C00_000A, // ds_swizzle_b32 v12, v10 offset:120
        0xD501_000C, 0x0002_1880, // v_cndmask_b32 v12, 0, v12, s[0:1]
        0xBE80_03FF, 0xFF00_FF00, // s_mov_b32 s0, 0xff00ff00
        0xBE81_03FF, 0xFF00_FF00, // s_mov_b32 s1, 0xff00ff00
        0x4A14_150C, // v_add_nc_u32 v10, v12, v10
        0xD8D4_00F0, 0x0C00_000A, // ds_swizzle_b32 v12, v10 offset:240
        0xD501_000C, 0x0002_1880, // v_cndmask_b32 v12, 0, v12, s[0:1]
        0xBE80_03FF, 0xFFFF_0000, // s_mov_b32 s0, 0xffff0000
        0xBE81_03FF, 0xFFFF_0000, // s_mov_b32 s1, 0xffff0000
        0x4A14_150C, // v_add_nc_u32 v10, v12, v10
        0xD8D4_01E0, 0x0C00_000A, // ds_swizzle_b32 v12, v10 offset:480
        0xD501_000C, 0x0002_1880, // v_cndmask_b32 v12, 0, v12, s[0:1]
        0xBE80_0380, // s_mov_b32 s0, 0
        0xBE81_03C1, // s_mov_b32 s1, -1
        0x4A14_150C, // v_add_nc_u32 v10, v12, v10
        0xD760_0005, 0x0001_3F0A, // v_readlane_b32 s5, v10, 31
        0xD501_000C, 0x0000_0A80, // v_cndmask_b32 v12, 0, s5, s[0:1]
        0xBE80_047E, // s_mov_b64 s[0:1], exec
        0x4A14_150C, // v_add_nc_u32 v10, v12, v10
        0x2C18_0086, // v_lshrrev_b32 v12, 6, v0
        0x7DA4_1ABF, // v_cmpx_eq_u32 exec, 63, v13
        0x341A_1882, // v_lshlrev_b32 v13, 2, v12
        0xD834_0000, 0x0000_0A0D, // ds_write_b32 v13, v10
        0xBEFE_0400, // s_mov_b64 exec, s[0:1]
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        barrier,     // s_barrier
        0x7E1A_0280, // v_mov_b32 v13, 0
        0xBE80_047E, // s_mov_b64 s[0:1], exec
        0x7DAA_1880, // v_cmpx_ne_u32 exec, 0, v12
        0xBF88_0013 + in_loop.len() as u32, // s_cbranch_execz to the restore of EXEC
        0xBE82_047E, // s_mov_b64 s[2:3], exec
        0xBE86_047E, // s_mov_b64 s[6:7], exec
        0xBE92_0380, // s_mov_b32 s18, 0
        0xBE91_0380, // s_mov_b32 s17, 0
        0xBF82_0002, // s_branch to the loop's body
        0x87FE_0610, // s_and_b64 exec, s[16:17], s[6:7]
        0x7E22_050D, // v_readfirstlane_b32 s17, v13
        0x8F10_8212, // s_lshl_b32 s16, s18, 2
        0x8112_8112, // s_add_i32 s18, s18, 1
        address,     // v_mov_b32 v13, s16
        0xD8D8_0000, 0x0D00_000D, // ds_read_b32 v13, v13
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x4A1A_1A11, // v_add_nc_u32 v13, s17, v13
    ];
    words.extend_from_slice(in_loop);
    words.extend_from_slice(&[
        0xBE90_047E, // s_mov_b64 s[16:17], exec
        0x7DA4_1812, // v_cmpx_eq_u32 exec, s18, v12
        0xBF80_0000, // s_nop 0
        0x8A86_7E06, // s_andn2_b64 s[6:7], s[6:7], exec
        0xBF85_FFF2 - (in_loop.len() as u32), // s_cbranch_scc1 to the loop's start
        0xBEFE_0400, // s_mov_b64 exec, s[0:1]
        0x4A14_150D, // v_add_nc_u32 v10, v13, v10
    ]);
    words
}

const S_BARRIER: u32 = 0xBF8A_0000;
const V_MOV_V13_S16: u32 = 0x7E1A_0210;

/// Every thread stores its result `v10` at its own dword.
const STORE: [u32; 3] = [
    0x3402_0082,              // v_lshlrev_b32 v1, 2, v0
    0xE070_1000,              // buffer_store_dword v10, v1, s[8:11], 0 offen
    0x8002_0A01,
];

/// The scan program for `threads` threads of one group.
fn program(threads: u32, words: &[u32]) -> NativeProgram {
    let mut native = native_of(words, true, threads);
    // 512 bytes of LDS: a total for each of up to 128 waves
    let StageIo::Cs(cs) = &mut native.io else { unreachable!() };
    cs.rsrc2 |= 1 << 15;
    native
}

fn whole(threads: u32, term: &[u32], address: u32, barrier: u32, in_loop: &[u32]) -> NativeProgram {
    let mut words = prologue(term);
    words.extend(scan(address, barrier, in_loop));
    words.extend_from_slice(&STORE);
    program(threads, &words)
}

/// The thread's term t * t + 5 in v5.
fn term() -> Vec<u32> {
    vec![
        0xD569_0005,
        0x0002_0100, // v_mul_lo_u32 v5, v0, v0
        0x4A0A_0A85, // v_add_nc_u32 v5, 5, v5
    ]
}

fn built(native: &NativeProgram) -> ir::Program {
    ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build")
}

/// What a first-lane read that is proven the same in every lane is made of: the loop's own.
#[test]
fn the_running_sum_of_the_waves_below_is_proven_the_same_in_every_lane() {
    let native = whole(512, &term(), V_MOV_V13_S16, S_BARRIER, &[]);
    let p = built(&native);
    let st = structure::structure(&p).expect("structure");
    let proven = wave_alike::find(&p, &st, false);
    let reads = p.blocks.iter().flat_map(|b| &b.insts).filter(|i| i.name() == "v_readfirstlane_b32").count();
    assert_eq!((reads, proven.len()), (1, 1));
}

/// Each of these keeps the read from being proven: the proof is of the whole chain of values.
#[test]
fn a_running_sum_that_differs_by_lane_or_by_time_is_not_proven() {
    let none = |name: &str, native: NativeProgram| {
        let p = built(&native);
        let st = structure::structure(&p).expect("structure");
        let reads = p.blocks.iter().flat_map(|b| &b.insts).filter(|i| i.name() == "v_readfirstlane_b32").count();
        assert_eq!(reads, 1, "{name}: the read is there");
        let proven = wave_alike::find(&p, &st, true);
        assert!(proven.is_empty(), "{name}: proven");
    };
    // the address of the read is the thread's own: each lane reads another dword
    none("address by thread", whole(512, &term(), 0x7E1A_0300, S_BARRIER, &[]));
    // no barrier before the reads: the other waves' totals may not be there yet
    none("no barrier", whole(512, &term(), V_MOV_V13_S16, 0xBF80_0000, &[]));
    // a write to LDS in the loop: the dword read may change between the two halves' reads
    none("a write in the loop", whole(512, &term(), V_MOV_V13_S16, S_BARRIER, &[0xD834_0000, 0x0000_0D0D]));
    // (a write after the loop, in a program with the barrier before it, is no help either)
}

/// The same read where the value is made in a loop and read after it: lanes that left the loop
/// in other rounds hold other values.
#[test]
fn a_value_made_in_a_loop_and_read_after_it_is_not_proven() {
    let loop_then_read = |read_inside: bool| {
        let mut words = vec![0xBE86_0380]; // s_mov_b32 s6, 0
        // loop: v_mov_b32 v2, s6; [v_readfirstlane_b32 s7, v2]; s_add_i32 s6, s6, 1; s_cmp_lt_u32 s6, 4; s_cbranch_scc1 loop
        words.push(0x7E04_0206); // v_mov_b32 v2, s6
        if read_inside {
            words.push(0x7E0E_0502); // v_readfirstlane_b32 s7, v2
        }
        words.extend_from_slice(&[
            0x8106_8106, // s_add_i32 s6, s6, 1
            0xBF0A_8406, // s_cmp_lt_u32 s6, 4
        ]);
        let back = 4 + u32::from(read_inside);
        words.push(0xBF85_0000 | (0x10000 - back) & 0xFFFF); // s_cbranch_scc1 to the loop
        if !read_inside {
            words.push(0x7E0E_0502); // v_readfirstlane_b32 s7, v2
        }
        words.extend_from_slice(&[0x7E08_0207, 0xE070_1000, 0x8002_0400]); // v_mov_b32 v4, s7; buffer_store_dword v4, v0 offen
        built(&native_of(&words, true, 64))
    };
    for (inside, proven) in [(true, 1), (false, 0)] {
        let p = loop_then_read(inside);
        let st = structure::structure(&p).expect("structure");
        assert_eq!(wave_alike::find(&p, &st, true).len(), proven, "the read inside the loop: {inside}");
    }
}

/// The scan on the hardware device, with the first-lane read exact: every thread's result is
/// the group's inclusive prefix sum, wherever the waves of the group are.
#[test]
fn a_scan_over_several_console_waves_is_the_prefix_sum_in_waves_of_32() {
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut ran = false;
    for threads in [128u32, 256, 512] {
        let native = whole(threads, &term(), V_MOV_V13_S16, S_BARRIER, &[]);
        let p = built(&native);
        let shader = lowered(&native, &p, &HashSet::new()).unwrap_or_else(|e| panic!("{threads} threads: {e}"));
        if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
            let mut copy = shader.dxil.clone();
            v.validate(&mut copy).expect("DXIL validation failed");
        }
        let Ok(host) = Host::new(false) else {
            eprintln!("no SM 6.6 hardware device; not run");
            return;
        };
        let (root, pso) = match host.compute_pipeline(&shader) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("the hardware device does not take this program (waves of 32?): {e}; not run");
                return;
            }
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(threads)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
        let mut sum = 0u32;
        for t in 0..threads {
            sum = sum.wrapping_add(t.wrapping_mul(t) + 5);
            assert_eq!(out[0][t as usize], sum, "{threads} threads: thread {t}");
        }
        ran = true;
    }
    if ran {
        eprintln!("ran on the hardware device: the group's prefix sums, 128, 256 and 512 threads");
    }
}

/// With `VICTORY_FIRSTLANE_ALIKE=0` the program is refused as it was.
#[test]
fn without_the_proof_the_scan_is_refused_as_it_was() {
    let native = whole(512, &term(), V_MOV_V13_S16, S_BARRIER, &[]);
    let p = built(&native);
    let was = wave_alike::TEST_ALIKE.replace(Some(false));
    let refused = lowered(&native, &p, &HashSet::new());
    wave_alike::TEST_ALIKE.set(was);
    assert!(
        matches!(&refused, Err(ShaderError::Unsupported { what, .. }) if *what == "a first-lane read where not every lane runs"),
        "{:?}",
        refused.err()
    );
}
