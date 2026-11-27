//! A console wave64 compute program on a host of 32-lane waves: a wave reduction inside a
//! per-lane branch, with that branch taken wave-wide ([`crate::structure::structure_wide`])
//! so that the two host waves can exchange their halves inside it. Lowered, validated by
//! `dxil.dll` when present, and run on the host GPU when it takes waves of 32.

use std::collections::HashSet;

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

/// 64 threads in one console wave. Each thread t holds the bit `1 << (t >> 1)`; the threads
/// below s0 (a root constant) OR theirs over the wave inside `if (t < s0) { ... }` and store
/// the result to dword 0 of the raw UAV.
/// `skip`: the branch past the body when no thread is below s0: `s_cbranch_execz`, or
/// `s_cbranch_scc0` on the SCC `s_and_saveexec` sets ("some lane is left").
fn program(skip: u32) -> NativeProgram {
    let mut words = vec![
        0x7E0A_0281, // v_mov_b32 v5, 1
        0x2C0C_0081, // v_lshrrev_b32 v6, 1, v0
        0x340E_0B06, // v_lshlrev_b32 v7, v6, v5
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        skip | 23,   // the branch to the restore of EXEC
    ];
    words.extend_from_slice(&super::wave::WAVE_OR_V7_TO_S7);
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    ]);
    native(&words)
}

/// A wave64 compute program of 64 threads (one console wave; the thread index in v0) with 4
/// root constants in s0..s3 and a raw UAV's descriptor in s[8:11], then `body`.
fn native(body: &[u32]) -> NativeProgram {
    native_of(body, true, 64)
}

/// The same for a console of 32-lane waves when `wave64` is not set, with `threads` threads.
pub(super) fn native_of(body: &[u32], wave64: bool, threads: u32) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `program()` for a host of 32-lane waves, with the branches closing `wide` taken wave-wide.
pub(super) fn lowered(native: &NativeProgram, p: &ir::Program, wide: &HashSet<usize>) -> Result<HostShader, ShaderError> {
    lowered_on(native, p, wide, 32)
}

/// [`lowered`] for a host whose waves have at most `host_wave` lanes.
fn lowered_on(
    native: &NativeProgram,
    p: &ir::Program,
    wide: &HashSet<usize>,
    host_wave: u32,
) -> Result<HostShader, ShaderError> {
    lowered_counted(native, p, wide, host_wave, false)
}

fn lowered_counted(
    native: &NativeProgram,
    p: &ir::Program,
    wide: &HashSet<usize>,
    host_wave: u32,
    counted: bool,
) -> Result<HostShader, ShaderError> {
    let st = structure::structure_wide(p, wide).expect("structure");
    let res = resource::track(p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(p, native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(host_wave));
    let lowered = lower::lower_counted(&lower::Parts {
        native,
        p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    }, counted);
    lower::TEST_HOST_WAVE.set(was);
    let lowered = lowered?;
    Ok(HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        root_indexed_views: res.root_indexed_views,
    })
}

#[test]
fn a_sum_of_halves_over_the_wave_is_the_consoles_tree_bit_for_bit() {
    // thread t holds the 16-bit float with bits 0x3C00 + 37 t (1.0 and up, with odd
    // mantissas, so that additions round); all of them store the wave's sum
    let mut words = vec![
        0x340C_0085, // v_lshlrev_b32 v6, 5, v0
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0x4A0E_0F06, // v_add_nc_u32 v7, v6, v7
        0x4A0E_0F00, // v_add_nc_u32 v7, v0, v7
        0x4A0E_0EFF,
        0x0000_3C00, // v_add_nc_u32 v7, 0x3c00, v7
    ];
    // (the wave OR's words with v_add_f16 for v_or_b32: opcode 50 for 28)
    words.extend(super::wave::WAVE_OR_V7_TO_S7.iter().map(|&w| if w >> 25 == 28 { w & 0x01FF_FFFF | 50 << 25 } else { w }));
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ]);
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert_eq!(p.blocks.iter().flat_map(|b| &b.insts).filter(|i| i.name() == "v_add_f16").count(), 6);
    let shader = lowered(&native, &p, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    // the console's sum: lanes 2k and 2k+1, then pairs, ... each addition rounded to 16 bits
    // (to nearest, ties to even); values here are normal numbers
    let wide = |h: u16| f32::from_bits((u32::from(h >> 10) + 112) << 23 | u32::from(h & 0x3FF) << 13);
    let narrow = |f: f32| {
        let b = f.to_bits();
        let h = ((b >> 23) - 112) << 10 | (b >> 13 & 0x3FF);
        let rest = b & 0x1FFF;
        (h + u32::from(rest > 0x1000 || rest == 0x1000 && h & 1 == 1)) as u16
    };
    let add = |a: u16, b: u16| narrow(wide(a) + wide(b));
    let terms: Vec<u16> = (0..64u16).map(|t| 0x3C00 + 37 * t).collect();
    let mut tree = terms.clone();
    while tree.len() > 1 {
        tree = tree.chunks(2).map(|c| add(c[0], c[1])).collect();
    }
    let in_a_row = terms.iter().copied().reduce(add).unwrap();
    assert_ne!(tree[0], in_a_row, "these terms tell the tree from another order");
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
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
                views: &[View::RawUav(1)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    // (the high half is lane 0's term's: zero)
    assert_eq!(out[0][0], u32::from(tree[0]), "tree {:#x}, in a row {in_a_row:#x}", tree[0]);
    eprintln!("ran on the hardware device: the sum {:#x} of the console's tree", tree[0]);
}

/// "Some lane is left" of a mask operation as a number: an upscaler's programs wait for
/// other groups in a loop that ends when every lane saw its flag, or after enough rounds:
/// both tests become -1 or 0 and SCC of `timeout | ~waiting` ends it.
#[test]
fn some_lane_of_a_mask_is_a_number_the_whole_wave_agrees_on() {
    let native = native(&[
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0001,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v1, -1, v1
        0x7D84_0200, // v_cmp_eq_u32 vcc, s0, v1
        0x8792_7E6A, // s_and_b64 s[18:19], vcc, exec
        0x8592_80C1, // s_cselect_b64 s[18:19], -1, 0
        0xBF09_8501, // s_cmp_ge_u32 s1, 5
        0x8594_80C1, // s_cselect_b64 s[20:21], -1, 0
        0x8B92_1214, // s_orn2_b64 s[18:19], s[20:21], s[18:19]
        0x8506_8081, // s_cselect_b32 s6, 1, 0
        0x7E04_0206, // v_mov_b32 v2, s6
        0xE070_0000,
        0x8002_0200, // buffer_store_dword v2, off, s[8:11], 0
    ]);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let shader = on_64_lanes(&native, &p);
    // (lane s0 waits, if there is one; s1 rounds)
    for (wait, rounds, done) in [(0, 0, 0), (37, 4, 0), (63, 4, 0), (64, 0, 1), (37, 5, 1), (64, 9, 1)] {
        let Some(out) = dispatched(&shader, &[wait, rounds]) else {
            return;
        };
        assert_eq!(out, done, "lane {wait} after {rounds} rounds");
    }
}

/// `native` for a host of 64-lane waves.
fn on_64_lanes(native: &NativeProgram, p: &ir::Program) -> HostShader {
    let st = structure::structure(p).expect("structure");
    let res = resource::track(p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(p, native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(64));
    let lowered = lower::lower(&lower::Parts {
        native,
        p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    });
    lower::TEST_HOST_WAVE.set(was);
    let lowered = lowered.expect("lower");
    assert_eq!(lowered.wave_size, Some(64));
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        root_indexed_views: res.root_indexed_views,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    shader
}

/// Dword 0 of the UAV after `shader` ran with root constants `constants`; none without a
/// device that takes it.
fn dispatched(shader: &HostShader, constants: &[u32]) -> Option<u32> {
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return None;
    };
    let (root, pso) = match host.compute_pipeline(shader) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("the hardware device does not take this program (waves of 64?): {e}; not run");
            return None;
        }
    };
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        if u.root == RootType::Constants {
            ud[u.index as usize] = constants.get(u.dword as usize).copied().unwrap_or(0);
        }
    }
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(1)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    Some(out[0][0])
}

/// The same sum inside `if (t >= s0) { ... }` on a host of 64-lane waves: the lanes below s0
/// are elsewhere, and count as +0 in the console's tree.
#[test]
fn a_sum_of_halves_where_not_every_lane_runs_on_a_wave_of_64() {
    let mut words = vec![
        0x340C_0085, // v_lshlrev_b32 v6, 5, v0
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0x4A0E_0F06, // v_add_nc_u32 v7, v6, v7
        0x4A0E_0F00, // v_add_nc_u32 v7, v0, v7
        0x4A0E_0EFF,
        0x0000_3C00, // v_add_nc_u32 v7, 0x3c00, v7
        0x7D86_0000, // v_cmp_le_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0000 | 23, // s_cbranch_execz to the restore of EXEC
    ];
    words.extend(super::wave::WAVE_OR_V7_TO_S7.iter().map(|&w| if w >> 25 == 28 { w & 0x01FF_FFFF | 50 << 25 } else { w }));
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    ]);
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let shader = on_64_lanes(&native, &p);
    let wide = |h: u16| match h {
        0 => 0.0,
        h => f32::from_bits((u32::from(h >> 10) + 112) << 23 | u32::from(h & 0x3FF) << 13),
    };
    let narrow = |f: f32| {
        if f == 0.0 {
            return 0;
        }
        let b = f.to_bits();
        let h = ((b >> 23) - 112) << 10 | (b >> 13 & 0x3FF);
        let rest = b & 0x1FFF;
        (h + u32::from(rest > 0x1000 || rest == 0x1000 && h & 1 == 1)) as u16
    };
    for from in [0u32, 1, 5, 31, 32, 37, 63] {
        let mut tree: Vec<u16> = (0..64u16).map(|t| if u32::from(t) < from { 0 } else { 0x3C00 + 37 * t }).collect();
        while tree.len() > 1 {
            tree = tree.chunks(2).map(|c| narrow(wide(c[0]) + wide(c[1]))).collect();
        }
        let Some(out) = dispatched(&shader, &[from]) else {
            return;
        };
        assert_eq!(out, u32::from(tree[0]), "lanes from {from}");
    }
}

/// The console's 16-bit wave sum where the lanes at or past s0 have left the program (a
/// thread past the end of the data): on the console they add +0, and after each step of the
/// tree they hold their run's partial sum all the same. On a host of 64-lane waves (AMD) the
/// lanes that are here read each partner run's sum from a lane of it that is here.
#[test]
fn a_sum_of_halves_where_lanes_have_left_is_the_consoles_tree_bit_for_bit() {
    let mut words = vec![
        0x7DA8_0000, // v_cmpx_gt_u32 exec, s0, v0: the lanes below s0 stay
        0, // s_cbranch_execz to the end (filled in below)
        0x340C_0085, // v_lshlrev_b32 v6, 5, v0
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0x4A0E_0F06, // v_add_nc_u32 v7, v6, v7
        0x4A0E_0F00, // v_add_nc_u32 v7, v0, v7
        0x4A0E_0EFF,
        0x0000_3C00, // v_add_nc_u32 v7, 0x3c00, v7
    ];
    words.extend(super::wave::WAVE_OR_V7_TO_S7.iter().map(|&w| if w >> 25 == 28 { w & 0x01FF_FFFF | 50 << 25 } else { w }));
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ]);
    // past every word after it, to the program's end
    words[1] = 0xBF88_0000 | (words.len() as u32 - 2);
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let shader = lowered_on(&native, &p, &HashSet::new(), 64).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let wide = |h: u16| {
        if h == 0 {
            0.0
        } else {
            f32::from_bits((u32::from(h >> 10) + 112) << 23 | u32::from(h & 0x3FF) << 13)
        }
    };
    let narrow = |f: f32| {
        if f == 0.0 {
            return 0;
        }
        let b = f.to_bits();
        let h = ((b >> 23) - 112) << 10 | (b >> 13 & 0x3FF);
        let rest = b & 0x1FFF;
        (h + u32::from(rest > 0x1000 || rest == 0x1000 && h & 1 == 1)) as u16
    };
    let add = |a: u16, b: u16| narrow(wide(a) + wide(b));
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut ran = false;
    for stay in [64u32, 63, 40, 33, 32, 17, 5, 1] {
        let terms: Vec<u16> = (0..64u32)
            .map(|t| if t < stay { (0x3C00 + 37 * t) as u16 } else { 0 })
            .collect();
        let mut tree = terms.clone();
        while tree.len() > 1 {
            tree = tree.chunks(2).map(|c| add(c[0], c[1])).collect();
        }
        let Some(got) = on_the_card(&shader, [stay, 0]) else {
            return;
        };
        ran = true;
        // (the high half is lane 0's term's: zero)
        assert_eq!(got, u32::from(tree[0]), "{stay} lanes stay: got {got:#x}, the console's tree {:#x}", tree[0]);
    }
    if ran {
        eprintln!("ran on the hardware device: the console's tree with lanes gone, in every case");
    }
}

/// One `v_add_f16` whose exact result lies between two halves: 1 + 3/1024 (0x3C03) plus 2
/// (0x4000) is 3 + 1.5 steps of that range, which the console rounds up to 0x4202. The
/// host's `f32tof16` may give 0x4201 (toward zero); with `VICTORY_F16_NEAREST=1` the
/// translation gives the console's whatever the host does.
#[test]
fn a_16_bit_sum_rounds_to_nearest_when_asked_to() {
    let words = [
        0x7E0E_02FF,
        0x0000_3C03, // v_mov_b32 v7, 0x3c03
        0x7E0C_02FF,
        0x0000_4000, // v_mov_b32 v6, 0x4000
        0x6400_0D07, // v_add_f16 v0, v7, v6
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ];
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert!(p.blocks.iter().flat_map(|b| &b.insts).any(|i| i.name() == "v_add_f16"));
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for nearest in [false, true] {
        let was = lower::TEST_F16_NEAREST.replace(Some(nearest));
        let shader = lowered(&native, &p, &HashSet::new());
        lower::TEST_F16_NEAREST.set(was);
        let shader = shader.expect("lower");
        if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
            let mut copy = shader.dxil.clone();
            v.validate(&mut copy).expect("DXIL validation failed");
        }
        let Ok(host) = Host::new(false) else {
            eprintln!("no SM 6.6 hardware device; not run");
            return;
        };
        let Ok((root, pso)) = host.compute_pipeline(&shader) else {
            eprintln!("the hardware device does not take this program; not run");
            return;
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
        let got = out[0][0] & 0xFFFF;
        eprintln!("v_add_f16 0x3c03 + 0x4000 on the hardware device, nearest {nearest}: {got:#x}");
        if nearest {
            assert_eq!(got, 0x4202);
        } else {
            assert!(got == 0x4201 || got == 0x4202, "{got:#x}");
        }
    }
}

#[test]
fn a_wave_or_inside_a_lane_branch_needs_the_branch_wave_wide() {
    for (skip, name) in [(0xBF88_0000, "s_cbranch_execz"), (0xBF84_0000, "s_cbranch_scc0")] {
        a_wave_or_inside(skip, name);
    }
}

fn a_wave_or_inside(skip: u32, name: &str) {
    let native = program(skip);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let branch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == name))
        .expect("the branch");
    // per lane, the two host waves cannot meet inside the branch
    lower::take_needs();
    let per_lane = lowered(&native, &p, &HashSet::new());
    assert!(
        matches!(&per_lane, Err(ShaderError::Unsupported { what, .. }) if *what == "a wave reduction where not every lane runs"),
        "{:?}",
        per_lane.err()
    );
    // ... and the lowering names the block, around which that branch is the one to widen
    let needs = lower::take_needs();
    let st = structure::structure(&p).expect("structure");
    assert_eq!(lower::branches_around(&st, &needs), Some(vec![branch]));
    // (taken, past the body, when every lane's bit is clear)
    assert_eq!(st.wave_any[branch], Some(false));
    // wave-wide, every lane runs the reduction (EXEC stays a guard on what it does there)
    let wide = HashSet::from([branch]);
    let st = structure::structure_wide(&p, &wide).expect("structure");
    let inside = match p.blocks[branch].exit {
        ir::Exit::Branch { not_taken, .. } => not_taken,
        _ => unreachable!(),
    };
    assert!(st.guards[inside].iter().any(Option::is_some));
    let shader = lowered(&native, &p, &wide).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    // On a GPU that takes waves of 32 (two host waves for the console's one): the OR is of
    // both halves, whichever half's lanes take the branch.
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
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
    // (threads below s0 take the branch; thread t gives bit t >> 1)
    for (below, expected) in [(64u32, u32::MAX), (40, 0x000F_FFFF), (33, 0x0001_FFFF), (10, 0x1F), (1, 1)] {
        let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            if let (RootType::Constants, 0, 0) = (u.root, u.slot, u.dword) {
                ud[u.index as usize] = below;
            }
        }
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
        assert_eq!(out[0][0], expected, "{name}: {below} threads in the branch");
    }
    eprintln!("ran on the hardware device ({name}): the OR of both halves in every case");
}

/// Runs `shader` on the hardware device with the root constants `s0`, `s1` and gives dword 0
/// of the UAV; None (and a line) without a device that takes it.
fn on_the_card(shader: &HostShader, constants: [u32; 2]) -> Option<u32> {
    whole_uav(shader, constants, 1).map(|out| out[0])
}

/// As [`on_the_card`], and gives the first `dwords` dwords of the UAV.
fn whole_uav(shader: &HostShader, constants: [u32; 2], dwords: u32) -> Option<Vec<u32>> {
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return None;
    };
    let Ok((root, pso)) = host.compute_pipeline(shader) else {
        eprintln!("the hardware device does not take this program; not run");
        return None;
    };
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        if let (RootType::Constants, 0, d @ 0..=1) = (u.root, u.slot, u.dword) {
            ud[u.index as usize] = constants[d as usize];
        }
    }
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(dwords)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    Some(out[0].clone())
}

/// A 32-lane console wave. A register holds a lane mask on one path (the lanes below s0)
/// and a number on the other, and is read as a number after the join, as the trace programs
/// keep their lanes' masks and their node numbers on one stack
/// (`deferred_tiled:CS_LaserLightShadowRaytracing`, s46 at 0x104, 0x75c and 0x188). The
/// mask arrives as the ballot of its lanes.
#[test]
fn a_lane_mask_joined_with_a_number_is_read_as_the_number_it_is() {
    let words = [
        0x7D88_0000, // v_cmp_gt_u32 vcc_lo, s0, v0
        0xBE94_036A, // s_mov_b32 s20, vcc_lo
        0xBF06_8001, // s_cmp_eq_u32 s1, 0
        0xBF85_0001, // s_cbranch_scc1 over the next
        0xBE94_0385, // s_mov_b32 s20, 5
        0x7E00_0214, // v_mov_b32 v0, s20
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ];
    let native = native_of(&words, false, 32);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let shader = lowered(&native, &p, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (below, other, expected) in [(32u32, 0u32, u32::MAX), (10, 0, 0x3FF), (0, 0, 0), (10, 1, 5)] {
        let Some(got) = on_the_card(&shader, [below, other]) else { return };
        assert_eq!(got, expected, "{below} lanes in the mask, s1 = {other}");
    }
    eprintln!("ran on the hardware device: the mask's bits, or the number");
}

/// A 32-lane console wave. `if (t < s0) { ... SCC = (s1 == 7) }` and a branch on SCC after
/// the join: the skip brings the SCC of `s_and_saveexec` ("some lane is left") along
/// (`rtdirectionalshadows:CS_RTShadowTrace32HQ_TiledFarOnly`, 0x360-0x39c). On the console
/// the skip is taken by the wave, so with some lane inside every lane sees the inner test.
/// A lane that skipped on its own would see "some lane is left" instead: refused per lane,
/// right with the branch taken wave-wide. Lane 31 stores what it saw.
#[test]
fn scc_of_a_mask_operation_joined_with_a_scalar_test_needs_the_skip_wave_wide() {
    let words = [
        0x7D88_0000, // v_cmp_gt_u32 vcc_lo, s0, v0
        0xBE8D_3C6A, // s_and_saveexec_b32 s13, vcc_lo
        0xBF88_0001, // s_cbranch_execz over the next
        0xBF06_8701, // s_cmp_eq_u32 s1, 7
        0xBEFE_030D, // s_mov_b32 exec_lo, s13
        0xBF85_0002, // s_cbranch_scc1 to "seven"
        0x7E0A_0281, // v_mov_b32 v5, 1
        0xBF82_0001, // s_branch over the next
        0x7E0A_0282, // seven: v_mov_b32 v5, 2
        0x7DA4_009F, // v_cmpx_eq_u32 exec_lo, 31, v0
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16 | 5 << 8, // buffer_store_dword v5, off, s[8:11], 0
    ];
    let native = native_of(&words, false, 32);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let branch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == "s_cbranch_execz"))
        .expect("the skip");
    lower::take_needs();
    let per_lane = lowered(&native, &p, &HashSet::new());
    assert!(
        matches!(&per_lane, Err(ShaderError::Unsupported { what, .. }) if *what == "a lane mask read as data"),
        "{:?}",
        per_lane.err()
    );
    let st = structure::structure(&p).expect("structure");
    assert_eq!(lower::branches_around(&st, &lower::take_needs()), Some(vec![branch]));
    let shader = lowered(&native, &p, &HashSet::from([branch])).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (lanes below s0 inside, s1) -> what lane 31 stores: 2 if SCC was set at the branch
    for (below, s1, expected) in [(0u32, 7u32, 1u32), (10, 7, 2), (10, 3, 1), (32, 3, 1), (32, 7, 2)] {
        let Some(got) = on_the_card(&shader, [below, s1]) else { return };
        assert_eq!(got, expected, "{below} lanes inside, s1 = {s1}");
    }
    eprintln!("ran on the hardware device: the inner test's SCC for every lane once some lane is inside");
}

/// A dispatch counted in threads: of the last group only the lanes below the
/// count store (after DexNom/VIctory bc3cd4b0).
#[test]
fn the_lanes_past_a_count_of_threads_do_not_run() {
    // Each lane that runs adds 1 to counter[local x].
    let native = native(&[
        0x3402_0082, // v_lshlrev_b32 v1, 2, v0
        0x7e04_0281, // v_mov_b32 v2, 1
        0xe0c8_1000, 0x8002_0201, // buffer_atomic_add v2, v1, s[8:11], 0 offen
    ]);
    let p = ir::build(&native.code, native.wave64, native.vgprs).unwrap();
    let shader = lowered_counted(&native, &p, &HashSet::new(), 64, true).unwrap();
    let limit = shader
        .user_data
        .iter()
        .find(|u| u.root == RootType::Other(super::USER_DISPATCH_THREADS_X))
        .unwrap();
    assert_eq!((limit.slot, limit.dword), (0, 64));
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(true) else { return };
    let Ok((root, pso)) = host.compute_pipeline(&shader) else { return };
    for count in [0u32, 1, 7, 63, 64, 65, 74, 77, 127, 128] {
        let mut constants = vec![0; shader.user_data_dwords() as usize];
        constants[limit.index as usize] = count;
        let result = host
            .dispatch(&root, &pso, &Dispatch {
                roots: vec![(0, Root::Constants(constants))],
                views: &[View::RawUav(68)],
                samplers: &[],
                groups: [2, 1, 1],
            })
            .unwrap();
        let active = if count == 0 { 128 } else { count };
        for (i, &word) in result[0].iter().enumerate().take(64) {
            let i = i as u32;
            assert_eq!(word, u32::from(i < active) + u32::from(i + 64 < active), "count {count}, word {i}");
        }
    }
}

/// A wave64 program of 128 threads (two console waves) that leaves early when no lane of its
/// wave is below s0, as the console's compiler does when the end of the program is too far for
/// a branch: `s_cbranch_execnz` over a far jump to the end. `extra` are words that go into the
/// far jump's block before it, and `work_at_end` puts an instruction between the jump's target
/// and the end of the program. Then the gate: every thread stores 1, or 2 when some lane of its
/// console wave that is still running has the thread number s1 (the lanes that fail a test).
fn far_early_out(extra: &[u32], work_at_end: bool) -> NativeProgram {
    let gate = [
        0x7D84_0001, // v_cmp_eq_u32 vcc, s1, v0
        0xBE90_106A, // s_bcnt1_i32_b64 s16, vcc
        0xBF06_8010, // s_cmp_eq_u32 s16, 0
        0x8511_8281, // s_cselect_b32 s17, 1, 2
        0x7E02_0211, // v_mov_b32 v1, s17
        0x3404_0082, // v_lshlrev_b32 v2, 2, v0
        0x38 << 26 | 0x1C << 18 | 1 << 12,
        0x80 << 24 | 2 << 16 | 1 << 8 | 2, // buffer_store_dword v1, v2, s[8:11], 0 offen
    ];
    let jump = [
        0xBE8E_1F00, // s_getpc_b64 s[14:15]
        0x800E_FF0E,
        4 * (4 + gate.len() as u32), // s_add_u32 s14, s14, the way to the end
        0x820F_800F, // s_addc_u32 s15, s15, 0
        0xBE80_200E, // s_setpc_b64 s[14:15]
    ];
    let mut words = vec![
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF89_0000 | (extra.len() + jump.len()) as u32, // s_cbranch_execnz over the jump
    ];
    words.extend_from_slice(extra);
    words.extend_from_slice(&jump);
    words.extend_from_slice(&gate);
    if work_at_end {
        words.push(0x7E06_0281); // v_mov_b32 v3, 1
    }
    native_of(&words, true, 128)
}

fn branches_named(p: &ir::Program, name: &str) -> usize {
    p.blocks.iter().filter(|b| b.insts.last().is_some_and(|i| i.name() == name)).count()
}

/// What the gate of [`far_early_out`] stores in each of 128 threads.
fn gate_expected(below: u32, failing: u32) -> Vec<u32> {
    (0..128u32)
        .map(|t| {
            let some_fail = failing < below && failing / 64 == t / 64;
            match (t < below, some_fail) {
                (false, _) => 0,
                (true, false) => 1,
                (true, true) => 2,
            }
        })
        .collect()
}

#[test]
fn a_far_early_out_is_left_out_and_the_gate_counts_the_lanes_of_both_halves() {
    let native = far_early_out(&[], false);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert_eq!(branches_named(&p, "s_cbranch_execnz"), 1);
    lower::take_needs();
    let kept = lowered(&native, &p, &HashSet::new());
    assert!(
        matches!(&kept, Err(ShaderError::Unsupported { what, .. }) if what.ends_with("where not every lane runs")),
        "{:?}",
        kept.err()
    );
    for (name, form) in [("early-outs", ir::without_early_outs(&p)), ("skips", ir::without_skips(&p, false))] {
        let form = form.unwrap_or_else(|| panic!("without {name}: the far early-out stays"));
        assert_eq!(branches_named(&form, "s_cbranch_execnz"), 0, "without {name}");
    }
    let form = ir::without_early_outs(&p).expect("left out");
    let shader = lowered(&native, &form, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (threads below s0 are in bounds; the one thread s1 fails the test) A failing thread of the
    // upper half gives 2 to the lower half's threads as well, and a failing thread out of
    // bounds counts for nothing
    for (below, failing) in [(128u32, 40u32), (128, 100), (128, 63), (40, 50), (40, 10), (33, 32), (1, 0), (0, 5)] {
        let Some(got) = whole_uav(&shader, [below, failing], 128) else { return };
        assert_eq!(got, gate_expected(below, failing), "{below} threads in bounds, thread {failing} fails");
    }
    eprintln!("ran on the hardware device: the gate's count over both halves of each console wave");
}

#[test]
fn a_far_jump_that_does_work_is_not_an_early_out() {
    // work in the jump's block, and work between the jump's target and the end
    for (name, native) in [
        ("work in the jump", far_early_out(&[0xBE94_0385], false)), // s_mov_b32 s20, 5
        ("work at the target", far_early_out(&[], true)),
    ] {
        let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
        assert_eq!(branches_named(&p, "s_cbranch_execnz"), 1, "{name}");
        assert!(ir::without_early_outs(&p).is_none(), "{name}");
        assert!(ir::without_skips(&p, false).is_none(), "{name}");
        assert!(ir::without_skips(&p, true).is_none(), "{name}");
    }
}

/// `if (t < s0) { park s[20:21]; if (s1 == 0) {..} else {..}; take it back }` and a read of
/// s[20:21] after the join, as the console's compiler leaves a parked scalar across an inner
/// branch. `then` is the inner branch's first arm; the second one leaves the parked scalar alone.
fn parked_across_an_inner_branch(then: u32) -> ir::Program {
    let body = [
        0xBE96_0414, // s_mov_b64 s[22:23], s[20:21]
        0xBF06_8001, // s_cmp_eq_u32 s1, 0
        0xBF85_0002, // s_cbranch_scc1 to the second arm
        then,
        0xBF82_0001, // s_branch to the join
        0xBE98_0382, // s_mov_b32 s24, 2
        0xBE94_0416, // s_mov_b64 s[20:21], s[22:23]
    ];
    let mut words = vec![
        0xBE94_0406, // s_mov_b64 s[20:21], s[6:7]
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0000 | body.len() as u32, // s_cbranch_execz past the body
    ];
    words.extend_from_slice(&body);
    words.extend_from_slice(&[
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
        0x7E02_0214, // v_mov_b32 v1, s20
    ]);
    let native = native(&words);
    ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build")
}

#[test]
fn a_skip_whose_parked_scalars_pass_an_inner_branch_unchanged_is_left_out() {
    let p = parked_across_an_inner_branch(0xBE96_0314); // s_mov_b32 s22, s20
    assert_eq!(branches_named(&p, "s_cbranch_execz"), 1);
    // (s22 comes back through a phi of the inner join whose two arguments are copies of one
    // value: only `deep` follows it)
    assert!(ir::without_skips(&p, false).is_none());
    let deep = ir::without_skips(&p, true).expect("left out");
    assert_eq!(branches_named(&deep, "s_cbranch_execz"), 0);
}

#[test]
fn a_skip_whose_parked_scalar_an_inner_branch_changes_is_kept() {
    let p = parked_across_an_inner_branch(0xBE96_0387); // s_mov_b32 s22, 7
    assert_eq!(branches_named(&p, "s_cbranch_execz"), 1);
    assert!(ir::without_skips(&p, false).is_none());
    assert!(ir::without_skips(&p, true).is_none());
}

/// `v_writelane_b32 vN, sX, K`.
fn writelane(n: u32, x: u32, lane: u32) -> Vec<u32> {
    vec![0xD761_0000 | n, (128 + lane) << 9 | x]
}

/// `v_readlane_b32 sX, vN, K`.
fn readlane(x: u32, n: u32, lane: u32) -> Vec<u32> {
    vec![0xD760_0000 | x, (128 + lane) << 9 | (256 + n)]
}

fn parked(words: &[Vec<u32>]) -> ir::Program {
    let native = native(&words.concat());
    ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build")
}

fn instructions<'a>(p: &'a ir::Program, name: &str) -> Vec<&'a ir::Inst> {
    p.blocks.iter().flat_map(|b| &b.insts).filter(|i| i.name() == name).collect()
}

fn regs<T>(of: &[(ir::Reg, T)]) -> Vec<ir::Reg> {
    of.iter().map(|r| r.0).collect()
}

#[test]
fn a_pair_parked_in_lanes_that_hold_other_scalars_elsewhere_moves_as_a_pair() {
    // s[20:21] goes into lanes 4 and 5 of v10 and comes back into s[22:23]; later lane 4
    // holds s30 for a while: no lane of the two is the pair's alone
    let p = parked(&[
        writelane(10, 20, 4),
        writelane(10, 21, 5),
        vec![0x7E04_0281], // v_mov_b32 v2, 1
        readlane(22, 10, 4),
        readlane(23, 10, 5),
        writelane(10, 30, 4),
        readlane(31, 10, 4),
    ]);
    let wide = instructions(&p, "s_mov_b64");
    let [save, restore] = wide[..] else { panic!("one pair save and one pair restore: {} moves", wide.len()) };
    assert_eq!(regs(&save.reads), [ir::Reg::S(20), ir::Reg::S(21)]);
    assert_eq!(regs(&restore.writes), [ir::Reg::S(22), ir::Reg::S(23)]);
    assert_eq!(regs(&restore.reads), regs(&save.writes));
    // the single scalar has its own lane register, not the pair's
    let narrow = instructions(&p, "s_mov_b32");
    let lone_save = narrow.iter().find(|i| regs(&i.reads) == [ir::Reg::S(30)]).expect("the save of s30");
    let lone_restore = narrow.iter().find(|i| regs(&i.writes) == [ir::Reg::S(31)]).expect("the restore into s31");
    assert_eq!(regs(&lone_restore.reads), regs(&lone_save.writes));
    assert!(regs(&lone_save.writes).iter().all(|r| !regs(&save.writes).contains(r)));
}

#[test]
fn a_parked_pair_another_save_or_restore_shares_stays_two_scalars() {
    let cases = [
        (
            "a lone restore of the lane",
            vec![
                writelane(10, 20, 4),
                writelane(10, 21, 5),
                readlane(22, 10, 4),
                readlane(23, 10, 5),
                readlane(24, 10, 4),
            ],
        ),
        (
            "another save of the lane on one path to the restore",
            vec![
                writelane(10, 20, 4),
                writelane(10, 21, 5),
                vec![0xBF06_8001, 0xBF85_0002], // s_cmp_eq_u32 s1, 0 and s_cbranch_scc1 over the save
                writelane(10, 30, 4),
                readlane(22, 10, 4),
                readlane(23, 10, 5),
            ],
        ),
    ];
    for (name, words) in cases {
        let p = parked(&words);
        assert!(instructions(&p, "s_mov_b64").is_empty(), "{name}");
        assert!(!instructions(&p, "s_mov_b32").is_empty(), "{name}");
    }
}

/// The prefix of wave totals the console's structure builder computes: each console
/// wave of a 256-thread group writes its total (10 + its index) to LDS, then, under
/// `if (id >> 6 != 0)`, loops over the totals of the waves before it with lanes leaving at their
/// own wave's index, reading the running sum's first lane (`v_readfirstlane`) every iteration.
/// (The total is read from the wave's last lane first, as the builder's scan does: a read
/// across the two host waves, which makes them exchange.)
/// The condition and the sum are the same in every lane of a console wave, so each host wave
/// takes its own first lane; before, the read was refused ("a first-lane read where not every
/// lane runs"), which kept the builder off on 32-lane GPUs. Every thread stores the sum of the
/// waves before its own: 0, 10, 10 + 11, 10 + 11 + 12.
fn wave_prefix_program(lane_dependent: bool) -> NativeProgram {
    let lds_address = if lane_dependent {
        0x3410_0082 // v_lshlrev_b32 v8, 2, v0
    } else {
        0x7E10_0210 // v_mov_b32 v8, s16
    };
    let words = [
        0x2C0E_0086, // v_lshrrev_b32 v7, 6, v0             (the console wave's index)
        0x3410_0E82, // v_lshlrev_b32 v8, 2, v7
        0x4A12_0E8A, // v_add_nc_u32 v9, 10, v7
        0xD760_0013,
        0x0201_7F09, // v_readlane_b32 s19, v9, 63   (the wave's last lane: the other host wave)
        0x7E12_0213, // v_mov_b32 v9, s19
        0xD834_0000,
        0x0000_0908, // ds_write_b32 v8, v9
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xBF8A_0000, // s_barrier
        0x7E10_0280, // v_mov_b32 v8, 0
        0xBE80_047E, // s_mov_b64 s[0:1], exec
        0x7DAA_0E80, // v_cmpx_ne_u32 0, v7
        0xBF88_0011, // s_cbranch_execz END
        0xBE86_047E, // s_mov_b64 s[6:7], exec
        0xBE92_0380, // s_mov_b32 s18, 0
        0xBE91_0380, // s_mov_b32 s17, 0
        0xBF82_0002, // s_branch BODY
        0x87FE_0614, // LOOPTOP: s_and_b64 exec, s[20:21], s[6:7]
        0x7E22_0508, // v_readfirstlane_b32 s17, v8
        0x8F10_8212, // BODY: s_lshl_b32 s16, s18, 2
        0x8112_8112, // s_add_i32 s18, s18, 1
        lds_address, // LDS address for the next subtotal
        0xD8D8_0000,
        0x0800_0008, // ds_read_b32 v8, v8
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x4A10_1011, // v_add_nc_u32 v8, s17, v8
        0xBE94_047E, // s_mov_b64 s[20:21], exec
        0x7DA4_0E12, // v_cmpx_eq_u32 s18, v7
        0x8A86_7E06, // s_andn2_b64 s[6:7], s[6:7], exec
        0xBF85_FFF3, // s_cbranch_scc1 LOOPTOP
        0xBEFE_0400, // END: s_mov_b64 exec, s[0:1]
        0x3402_0082, // v_lshlrev_b32 v1, 2, v0
        0xE070_1000,
        0x8002_0801, // buffer_store_dword v8, v1, s[8:11], 0 offen
    ];
    let mut native = native_of(&words, true, 256);
    if let StageIo::Cs(cs) = &mut native.io {
        cs.rsrc2 |= 2 << 15; // 1 KiB covers the lane-varying address as well.
    }
    native
}

#[test]
fn a_first_lane_read_of_a_wave_alike_sum_under_a_wave_alike_loop() {
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_FEATURE_D3D12_OPTIONS1, D3D12_FEATURE_DATA_D3D12_OPTIONS1,
    };

    let native = wave_prefix_program(false);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert!(
        p.blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|i| i.name() == "v_readfirstlane_b32")
    );
    let shader = lowered(&native, &p, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
        eprintln!("DXIL validator accepted the wave32 prefix shader");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return;
    };
    let mut waves = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: the output has the layout and size required by OPTIONS1.
    unsafe {
        host.device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS1,
            (&raw mut waves).cast(),
            size_of_val(&waves) as u32,
        )
    }.expect("query wave capabilities");
    if !(waves.WaveLaneCountMin..=waves.WaveLaneCountMax).contains(&32) {
        eprintln!("the device does not support wave32; hardware dispatch skipped");
        return;
    }
    let (root, pso) = host.compute_pipeline(&shader).expect("wave32 prefix pipeline");
    let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(256)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    let expected = |t: u32| (0..(t >> 6)).map(|w| 10 + w).sum::<u32>();
    let got: Vec<u32> = (0..256).map(|t| out[0][t]).collect();
    for t in 0..256u32 {
        assert_eq!(got[t as usize], expected(t), "thread {t}: {got:?}");
    }
    eprintln!("ran on the hardware device: every console wave got the sum of the waves before it");
}

#[test]
fn a_lane_dependent_prefix_cannot_take_its_own_first_lane() {
    let native = wave_prefix_program(true);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let Err(error) = lowered(&native, &p, &HashSet::new()) else {
        panic!("the two host waves may read different subtotals");
    };
    assert!(error.to_string().contains("a first-lane read where not every lane runs"), "{error}");
}

/// Console lanes in a 2-D or 3-D group of more than one host wave (Belobug/VIctory-Belobug-AMD
/// #216, PR #442). Each thread computes its flat id f from v0..v2 (`TIDIG_COMP_CNT` 2), reads lane
/// 0's (`v_readlane`, so the program gets console lanes) and adds that + 1 to dword f. As on the
/// console, every dword gets exactly one add, of 1 + its console wave's first id. A GPU need not
/// fill such a group in aligned runs of flat ids (NVIDIA's 8 x 8 groups, 2 x 2 quads on AMD), and
/// rounding a host wave's first flat id down to the wave size then gave two waves the same ids.
fn flat_id_adds(threads: [u32; 3], wave64: bool) -> NativeProgram {
    let [x, y, _] = threads;
    let body = [
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xD569_0005,
        0x0001_FF01,
        x, // v_mul_lo_u32 v5, v1, X
        0xD569_0006,
        0x0001_FF02,
        x * y,       // v_mul_lo_u32 v6, v2, XY
        0x4A08_0B00, // v_add_nc_u32 v4, v0, v5
        0x4A08_0D04, // v_add_nc_u32 v4, v4, v6
        0xD760_000C,
        0x0001_0104, // v_readlane_b32 s12, v4, 0
        0x7E04_020C, // v_mov_b32 v2, s12
        0x4A04_0481, // v_add_nc_u32 v2, 1, v2
        0x3406_0882, // v_lshlrev_b32 v3, 2, v4
        0xE0C8_1000,
        0x8002_0203, // buffer_atomic_add v2, v3, s[8:11], 0 offen
    ];
    let mut native = native_of(&body, wave64, 1);
    // (thread ids x, y and z in v0..v2)
    native.io = StageIo::Cs(ComputeIo {
        rsrc2: 5 << 1 | 2 << 11,
        threads,
    });
    native
}

/// [`flat_id_adds`] lowered for host waves of `host_wave` lanes, with `tickets` as the test's
/// choice for `VICTORY_THREAD_TICKETS` (None: the process's), and validated if `dxil.dll` is there.
fn flat_id_adds_lowered(
    threads: [u32; 3],
    wave64: bool,
    host_wave: u32,
    tickets: Option<bool>,
) -> Result<HostShader, String> {
    let native = flat_id_adds(threads, wave64);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let before = lower::TEST_TICKETS.replace(tickets);
    let shader = lowered_on(&native, &p, &HashSet::new(), host_wave);
    lower::TEST_TICKETS.set(before);
    let shader = shader.map_err(|e| format!("{e:?}"))?;
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
    Ok(shader)
}

/// The adds of [`flat_id_adds`] on the hardware device (None without one that takes the
/// program), and what each dword must hold.
fn flat_id_adds_on_the_card(
    shader: &HostShader,
    threads: [u32; 3],
    wave64: bool,
) -> Option<(Vec<u32>, Vec<u32>)> {
    let w: u32 = if wave64 { 64 } else { 32 };
    let n: u32 = threads.iter().product();
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let out = whole_uav(shader, [0, 0], n)?;
    Some((out, (0..n).map(|t| 1 + (t & !(w - 1))).collect()))
}

/// Group shapes of more than one host wave: the 2-D and 3-D ones of the title's console wave32
/// programs (8 x 8 and 4 x 4 x 4 at 32 lanes are 88 + 2 programs of its archive), and shapes
/// whose last host wave is partial.
const SHAPES_32: [[u32; 3]; 12] = [
    [32, 2, 1],
    [8, 8, 1],
    [4, 4, 4],
    [32, 2, 2],
    [16, 16, 1],
    [8, 8, 2],
    [16, 2, 4],
    [10, 10, 1],
    [12, 12, 1],
    [24, 8, 1],
    [6, 6, 1],
    [32, 4, 2],
];

#[test]
fn console_threads_of_multidimensional_groups_are_one_each() {
    let mut ran = Vec::new();
    for (wave64, host_wave) in [(false, 32), (true, 64), (true, 32)] {
        let w: u32 = if wave64 { 64 } else { 32 };
        let mut shapes = SHAPES_32.to_vec();
        shapes.push([w, 2, 1]);
        for threads in shapes {
            let what = format!("console wave{w}, host waves of {host_wave}, {threads:?}");
            let shader = flat_id_adds_lowered(threads, wave64, host_wave, None)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            if let Some((out, want)) = flat_id_adds_on_the_card(&shader, threads, wave64) {
                assert_eq!(out, want, "{what}");
                ran.push(what);
            }
        }
    }
    eprintln!("ran on the hardware device: {ran:#?}");
}

/// `VICTORY_THREAD_TICKETS=0` (here: the test's own choice) is the translation this tree had:
/// the console-wave32 programs of groups of 2-D or 3-D shapes and more than one wave differ from
/// the ones with the counter, those of 1-D groups and of one host wave do not.
#[test]
fn the_ticket_switch_changes_only_multidimensional_groups_of_several_waves() {
    let make = |threads, tickets| {
        flat_id_adds_lowered(threads, false, 32, Some(tickets))
            .expect("lowered")
            .dxil
    };
    for threads in SHAPES_32 {
        assert_ne!(make(threads, true), make(threads, false), "{threads:?}");
    }
    for threads in [[64, 1, 1], [32, 1, 1], [4, 4, 2], [8, 4, 1]] {
        assert_eq!(make(threads, true), make(threads, false), "{threads:?}");
    }
}

/// What the old rounding gives on this card, one line per shape (no assertion: which shapes
/// collide depends on the card and the driver; the run on an RTX 5080 is in docs/findings.md).
/// Run with `-- --nocapture`.
#[test]
fn the_old_rounding_of_console_threads_on_this_card() {
    let mut bad = Vec::new();
    for threads in SHAPES_32 {
        let shader = flat_id_adds_lowered(threads, false, 32, Some(false)).expect("lowered");
        for run in 0..4 {
            let Some((out, want)) = flat_id_adds_on_the_card(&shader, threads, false) else {
                return;
            };
            let wrong = out.iter().zip(&want).filter(|(a, b)| a != b).count();
            if wrong > 0 {
                bad.push(format!("{threads:?} run {run}: {wrong} of {} dwords wrong", want.len()));
                break;
            }
        }
    }
    eprintln!("rounding: {} of {} shapes wrong on the card: {bad:#?}", bad.len(), SHAPES_32.len());
}

/// A saved mask updated inside a divergent branch and read, both words, after the join (Belobug
/// PR #345): `s[2:3] = lanes below 5; if (t >= s1) { s[2:3] |= lanes below 55 }`, then both
/// dwords are stored per lane. With no lane left for the branch the OR changes nothing (the
/// comparison clears both dwords of VCC), so the skip may go (`ir::without_skips`, deep) and
/// the program translates in an exact form; without it the program is refused ("a lane mask
/// holding lanes elsewhere read as data"). `VICTORY_EMPTY_MASK_OR=0`, here the test's choice
/// of `ir::TEST_EMPTY_MASK_OR`, is the refusal.
fn branch_mask_program() -> NativeProgram {
    native(&[
        0x7D88_0085, // v_cmp_gt_u32 vcc, 5, v0: bits 0..4
        0xBE82_046A, // s_mov_b64 s[2:3], vcc
        0x7D86_0001, // v_cmp_le_u32 vcc, s1, v0: lanes at or above the threshold
        0xBE84_246A, // s_and_saveexec_b64 s[4:5], vcc
        0xBF88_0002, // s_cbranch_execz -> join
        0x7D88_00B7, // v_cmp_gt_u32 vcc, 55, v0: add active lanes below 55
        0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
        0x88FE_047E, // join: s_or_b64 exec, exec, s[4:5]
        0x7E08_0202, // v_mov_b32 v4, s2
        0x7E0A_0203, // v_mov_b32 v5, s3
        0x3412_0083, // v_lshlrev_b32 v9, 3, v0
        0xE070_1000,
        0x8002_0409, // buffer_store_dword v4, v9, s[8:11], 0 offen
        0xE070_1004,
        0x8002_0509, // buffer_store_dword v5, v9, s[8:11], 0 offen offset:4
    ])
}

/// [`branch_mask_program`] through the whole of `emit` (its retries included) for a host of
/// 32-lane waves, with `empty_mask_or` as the test's choice for `VICTORY_EMPTY_MASK_OR`.
fn branch_mask_emitted(empty_mask_or: bool) -> Result<HostShader, ShaderError> {
    let native = branch_mask_program();
    let p = crate::spill::build(&native).expect("IR");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resources");
    let io = io::raise(&p, &native).expect("IO");
    let bytes = crate::dxbc::build(&[]);
    let container = crate::Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let before = ir::TEST_EMPTY_MASK_OR.replace(Some(empty_mask_or));
    let result = super::emit(&container, &native, &p, &st, &io, &res);
    ir::TEST_EMPTY_MASK_OR.set(before);
    lower::TEST_HOST_WAVE.set(was);
    result
}

#[test]
fn a_branch_mask_is_refused_without_the_empty_mask_or_and_translates_with_it() {
    let Err(refused) = branch_mask_emitted(false) else {
        panic!("translated without the empty mask OR");
    };
    assert!(refused.to_string().contains("holding lanes elsewhere"), "{refused}");
    let shader = branch_mask_emitted(true).expect("translation with the empty mask OR");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
}

#[test]
fn a_branch_mask_keeps_both_words_on_the_card() {
    let shader = branch_mask_emitted(true).expect("translation");
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (threshold, expected) in [
        (0, [u32::MAX, 0x007f_ffff]),     // all lanes enter
        (32, [0x0000_001f, 0x007f_ffff]), // only the upper half enters
        (48, [0x0000_001f, 0x007f_0000]), // only the top 16 enter
        (64, [0x0000_001f, 0]),           // no lane enters; the old mask survives
    ] {
        let Some(out) = whole_uav(&shader, [0, threshold], 128) else {
            return;
        };
        for (lane, pair) in out.chunks_exact(2).enumerate() {
            assert_eq!(pair, expected, "threshold {threshold}, lane {lane}");
        }
    }
    eprintln!("ran on the hardware device: four thresholds, both words right in all 64 lanes");
}

// Divergent loops whose scalar is read per lane after them (Belobug/VIctory-Belobug-AMD PR #366,
// issues #299 and #352). One console wave64 of 64 threads on a host of 32-lane waves, two host
// waves: on the console the loop runs until the last lane is out and a scalar it counts holds the
// wave's count in every lane afterwards; with per-lane control flow each lane read the count of
// its own iterations. `VICTORY_LOOP_SCALARS=0` (here `lower::TEST_LOOP_SCALARS`) is the old form.

/// `body` then a store of v0 at v10 * 4 (v10 keeps the thread id) to dword v10 of the raw UAV.
fn scalar_loop_program(body: &[u32], wave64: bool, threads: u32) -> NativeProgram {
    let mut words = body.to_vec();
    words.extend_from_slice(&[
        0x3412_1482,                       // v_lshlrev_b32 v9, 2, v10
        0x38 << 26 | 0x1C << 18 | 1 << 12, // buffer_store_dword v0, v9, s[8:11], 0 offen
        0x80 << 24 | 2 << 16 | 9,
    ]);
    native_of(&words, wave64, threads)
}

/// The program through the whole of `emit` (its retries included) for a host of 32-lane waves.
fn scalar_loop_emitted(
    native: &NativeProgram,
    loop_scalars: Option<bool>,
) -> Result<HostShader, ShaderError> {
    let p = crate::spill::build(native).expect("IR");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resources");
    let io = io::raise(&p, native).expect("IO");
    let bytes = crate::dxbc::build(&[]);
    let container = crate::Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let before = lower::TEST_LOOP_SCALARS.replace(loop_scalars);
    let result = super::emit(&container, native, &p, &st, &io, &res);
    lower::TEST_LOOP_SCALARS.set(before);
    lower::TEST_HOST_WAVE.set(was);
    result
}

/// The loop latches `lower::scalar_loop_latches` selects in `native`.
fn scalar_loop_latches_of(native: &NativeProgram) -> HashSet<usize> {
    let p = crate::spill::build(native).expect("IR");
    let st = structure::structure(&p).expect("structure");
    lower::scalar_loop_latches(&p, &st)
}

/// What the program stores for each of its `threads` threads, on the card (None without one
/// that takes it), as translated with `loop_scalars` (None: the process's choice).
fn scalar_loop_on_the_card(
    native: &NativeProgram,
    loop_scalars: Option<bool>,
    threads: u32,
) -> Option<Vec<u32>> {
    let shader = scalar_loop_emitted(native, loop_scalars).expect("translation");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    whole_uav(&shader, [0, 0], threads)
}

/// A program the synchronized latches change: translated with them, every lane stores
/// `expected`; translated as before (`VICTORY_LOOP_SCALARS=0`) it does not (each lane stores
/// the count of its own iterations).
fn scalar_loop_is_fixed(body: &[u32], expected: &[u32]) {
    let n = expected.len() as u32;
    let native = scalar_loop_program(body, true, n);
    assert!(!scalar_loop_latches_of(&native).is_empty(), "no latch selected");
    let on = scalar_loop_emitted(&native, Some(true)).expect("translation");
    let off = scalar_loop_emitted(&native, Some(false)).expect("translation");
    assert_ne!(on.dxil, off.dxil, "the switch changes nothing");
    if let Some(got) = scalar_loop_on_the_card(&native, Some(true), n) {
        assert_eq!(got, expected, "with the synchronized latches");
        let Some(old) = scalar_loop_on_the_card(&native, Some(false), n) else {
            return;
        };
        assert_ne!(old, expected, "the old form is right here: nothing to fix on this card");
        eprintln!("old form: {old:?}\nnew form: {got:?}");
    }
}

/// A program the synchronized latches leave alone: the same translation either way.
fn scalar_loop_is_untouched(native: &NativeProgram, expected: Option<&[u32]>) {
    let on = scalar_loop_emitted(native, Some(true)).expect("translation");
    let off = scalar_loop_emitted(native, Some(false)).expect("translation");
    assert_eq!(on.dxil, off.dxil, "the switch changes the translation");
    if let Some(expected) = expected
        && let Some(got) = scalar_loop_on_the_card(native, None, expected.len() as u32)
    {
        assert_eq!(got, expected);
    }
}

#[test]
fn a_divergent_scalar_loop_counts_for_the_whole_wave() {
    // PR #299: each lane loops t + 1 times; s6 counts the iterations
    scalar_loop_is_fixed(
        &[
            0x7E14_0300, // v_mov_b32 v10, v0
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0xBE82_0480, // s_mov_b64 s[2:3], 0
            0xBE86_0380, // s_mov_b32 s6, 0
            0x8106_8106, // loop: s_add_i32 s6, s6, 1
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
            0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
            0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
            0xBF89_FFFA, // s_cbranch_execnz loop
            0x88FE_027E, // s_or_b64 exec, exec, s[2:3]
            0x7E00_0206, // v_mov_b32 v0, s6
        ],
        &[64; 64],
    );
}

/// The longest loop is in the second host wave, or in the first: the other one has to wait for it
/// (trip counts 1 to 4 in one half, 6 to 9 in the other; s6 += 3 each iteration).
#[test]
fn both_host_waves_count_the_longest_loop_of_the_wave() {
    for longer_first in [false, true] {
        let mut body = vec![
            0x7E14_0300, // v_mov_b32 v10, v0
            0x3602_0083, // v_and_b32 v1, 3, v0
            0x4A02_0281, // v_add_nc_u32 v1, 1, v1
            0x2C08_0085, // v_lshrrev_b32 v4, 5, v0: 0 in the first half, 1 in the second
        ];
        if longer_first {
            body.push(0x3A08_0881); // v_xor_b32 v4, 1, v4
        }
        body.extend([
            0x340A_0882, // v_lshlrev_b32 v5, 2, v4
            0x4A02_0304, // v_add_nc_u32 v1, v4, v1
            0x4A02_0305, // v_add_nc_u32 v1, v5, v1: 5 more in the longer half
            0xBE82_0480, // s_mov_b64 s[2:3], 0
            0xBE86_0380, // s_mov_b32 s6, 0
            0x8106_8306, // loop: s_add_i32 s6, s6, 3
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
            0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
            0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
            0xBF89_FFFA, // s_cbranch_execnz loop
            0x88FE_027E, // s_or_b64 exec, exec, s[2:3]
            0x7E00_0206, // v_mov_b32 v0, s6
        ]);
        scalar_loop_is_fixed(&body, &[27; 64]);
    }
}

#[test]
fn nested_divergent_scalar_loops_count_for_the_whole_wave() {
    // PR #299: the inner loop runs (t >> 4) + 1 times, the outer (t & 3) + 1 times
    scalar_loop_is_fixed(
        &[
            0x7E14_0300, // v_mov_b32 v10, v0
            0x3602_0083, // v_and_b32 v1, 3, v0
            0x4A02_0281, // v_add_nc_u32 v1, 1, v1
            0x2C08_0084, // v_lshrrev_b32 v4, 4, v0
            0x4A08_0881, // v_add_nc_u32 v4, 1, v4
            0xBE86_0380, // s_mov_b32 s6, 0
            0xBE82_0480, // s_mov_b64 s[2:3], 0
            0xBE8C_0480, // s_mov_b64 s[12:13], 0
            0x7E06_0304, // v_mov_b32 v3, v4
            0x8106_8106, // s_add_i32 s6, s6, 1
            0x4E06_0681, // v_subrev_nc_u32 v3, 1, v3
            0x7D84_0680, // v_cmp_eq_u32 vcc, 0, v3
            0x888C_0C6A, // s_or_b64 s[12:13], vcc, s[12:13]
            0x8AFE_0C7E, // s_andn2_b64 exec, exec, s[12:13]
            0xBF89_FFFA, // s_cbranch_execnz inner
            0x88FE_0C7E, // s_or_b64 exec, exec, s[12:13]
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
            0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
            0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
            0xBF89_FFF2, // s_cbranch_execnz outer
            0x88FE_027E, // s_or_b64 exec, exec, s[2:3]
            0x7E00_0206, // v_mov_b32 v0, s6
        ],
        &[16; 64],
    );
}

/// Two loops one after the other: the first loop's s6 goes on into the second (through a phi),
/// and the second leaves two scalars.
#[test]
fn consecutive_divergent_loops_carry_their_scalars() {
    scalar_loop_is_fixed(
        &[
            0x7E14_0300, // v_mov_b32 v10, v0
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0xBE86_0380, // s_mov_b32 s6, 0
            0xBE87_038B, // s_mov_b32 s7, 11
            0xBE82_0480, // s_mov_b64 s[2:3], 0
            0x8106_8106, // first loop: s6 += 1
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
            0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
            0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
            0xBF89_FFFA, // s_cbranch_execnz first loop
            0x88FE_027E, // s_or_b64 exec, exec, s[2:3]
            0x3602_1483, // v_and_b32 v1, 3, v10
            0x4A02_0281, // v_add_nc_u32 v1, 1, v1
            0xBE82_0480, // s_mov_b64 s[2:3], 0
            0x8106_8306, // second loop: s6 += 3, starting at the first loop's result
            0x8107_8207, // s7 += 2
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
            0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
            0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
            0xBF89_FFF9, // s_cbranch_execnz second loop
            0x88FE_027E, // s_or_b64 exec, exec, s[2:3]
            0x8106_0706, // s_add_i32 s6, s6, s7
            0x7E00_0206, // v_mov_b32 v0, s6
        ],
        &[64 + 3 * 4 + 11 + 2 * 4; 64],
    );
}

/// What the synchronized latches leave alone: a loop every lane leaves together (a uniform
/// exit), a loop whose only results are per lane, a loop of a console wave32.
#[test]
fn loops_without_a_per_lane_scalar_are_translated_as_before() {
    // s6 counts to 4 in every lane
    let uniform = scalar_loop_program(
        &[
            0x7E14_0300, // v_mov_b32 v10, v0
            0xBE86_0380, // s_mov_b32 s6, 0
            0x8106_8106, // loop: s_add_i32 s6, s6, 1
            0xBF0A_8406, // s_cmp_lt_u32 s6, 4
            0xBF85_FFFD, // s_cbranch_scc1 loop
            0x7E00_0206, // v_mov_b32 v0, s6
        ],
        true,
        64,
    );
    assert!(scalar_loop_latches_of(&uniform).is_empty());
    scalar_loop_is_untouched(&uniform, Some(&[4; 64]));
    // each lane counts its own iterations in a vector register, nothing scalar leaves the loop
    let vector_only = scalar_loop_program(&scalar_loop_vector_body(false), true, 64);
    assert!(scalar_loop_latches_of(&vector_only).is_empty());
    let want: Vec<u32> = (1..=64).collect();
    scalar_loop_is_untouched(&vector_only, Some(&want));
    // a console wave32 (the scalar is the wave's own: one host wave)
    let wave32 = scalar_loop_program(
        &[
            0x7E14_0300, // v_mov_b32 v10, v0
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0x7E16_0280, // v_mov_b32 v11, 0
            0xBE82_0380, // s_mov_b32 s2, 0
            0x4A16_1681, // loop: v_add_nc_u32 v11, 1, v11
            0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
            0x7D84_0280, // v_cmp_eq_u32 vcc_lo, 0, v1
            0x8802_026A, // s_or_b32 s2, vcc_lo, s2
            0x8A7E_027E, // s_andn2_b32 exec_lo, exec_lo, s2
            0xBF89_FFFA, // s_cbranch_execnz loop
            0x887E_027E, // s_or_b32 exec_lo, exec_lo, s2
            0x7E00_030B, // v_mov_b32 v0, v11
        ],
        false,
        32,
    );
    assert!(scalar_loop_latches_of(&wave32).is_empty());
    scalar_loop_is_untouched(&wave32, Some(&(1..=32).collect::<Vec<u32>>()));
}

/// A scalar shared by the lanes and a lane-local vector count: the scalar is the wave's, the
/// vector each lane's own (v0 = v11 + s6).
fn scalar_loop_vector_body(shared: bool) -> Vec<u32> {
    let mut body = vec![
        0x7E14_0300, // v_mov_b32 v10, v0
        0x4A02_0081, // v_add_nc_u32 v1, 1, v0
        0x7E16_0280, // v_mov_b32 v11, 0
        0xBE82_0480, // s_mov_b64 s[2:3], 0
    ];
    if shared {
        body.extend([0xBE86_0387, 0x8106_8306]); // s6 = 7; loop: s6 += 3
    }
    body.extend([
        0x4A16_1681, // v_add_nc_u32 v11, 1, v11
        0x4E02_0281, // v_subrev_nc_u32 v1, 1, v1
        0x7D84_0280, // v_cmp_eq_u32 vcc, 0, v1
        0x8882_026A, // s_or_b64 s[2:3], vcc, s[2:3]
        0x8AFE_027E, // s_andn2_b64 exec, exec, s[2:3]
        if shared { 0xBF89_FFF9 } else { 0xBF89_FFFA },
        0x88FE_027E,                                    // s_or_b64 exec, exec, s[2:3]
        if shared { 0x4A00_1606 } else { 0x7E00_030B }, // v0 = v11 + s6 or v11
    ]);
    body
}

#[test]
fn a_shared_scalar_and_a_lane_local_vector_count_each_their_own() {
    let want: Vec<u32> = (1..=64).map(|t| 7 + 3 * 64 + t).collect();
    scalar_loop_is_fixed(&scalar_loop_vector_body(true), &want);
}

/// The synchronized latches are for one console wave in the group (two host waves): a group of
/// two console waves is translated as before, whatever the loops hold.
#[test]
fn groups_of_several_console_waves_keep_their_translation() {
    let native = scalar_loop_program(&scalar_loop_vector_body(true), true, 128);
    assert_eq!(scalar_loop_latches_of(&native).len(), 1);
    scalar_loop_is_untouched(&native, None);
}

/// A skip around the loop (some lanes never enter it) is a shape the synchronized latches do not
/// take.
#[test]
fn a_loop_behind_a_skip_is_translated_as_before() {
    let body = scalar_loop_vector_body(true);
    let mut words = vec![
        0x7D88_00A0,                           // v_cmp_gt_u32 vcc, 32, v0
        0xBE8C_246A,                           // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0000 | (body.len() + 3) as u32, // s_cbranch_execz past body and store to s_endpgm
    ];
    words.extend(body);
    let native = scalar_loop_program(&words, true, 64);
    assert!(scalar_loop_latches_of(&native).is_empty());
    scalar_loop_is_untouched(&native, None);
}

/// A lane mask read as a number inside a lane branch, with both host waves of a console wave64
/// exchanging (kelvaroq patches 0033 and 0047, `VICTORY_WAVE_ZERO_MASK`; the test's choice here
/// is `lower::TEST_ZERO_MASK`). Threads below s0 read both dwords of a scalar mask s[14:15] and
/// store 1 + (lo ^ hi). An empty mask (`s_xor_b64 s[14:15], exec, exec`) is zero in both dwords
/// whatever the other half of the console wave does; a copy of EXEC made in the entry block
/// (`s_or_b64 s[14:15], exec, exec`) is read where it was made, by both waves; a mask only some
/// lanes hold (`s_and_b64 s[14:15], exec, 1`) still needs every lane there.
#[test]
fn empty_and_copied_wave_masks_can_be_read_in_a_lane_branch() {
    let lower_mask = |op, on| {
        let native = native(&[
            0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
            0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0007, // s_cbranch_execz to the restore of EXEC
            op,
            0x7E0A_020E, // v_mov_b32 v5, s14
            0x7E0C_020F, // v_mov_b32 v6, s15
            0x3A0A_0D05, // v_xor_b32 v5, v5, v6
            0x4A0A_0A81, // v_add_nc_u32 v5, 1, v5
            0x38 << 26 | 0x1C << 18,
            0x80 << 24 | 2 << 16 | 5 << 8, // buffer_store_dword v5, off, s[8:11], 0
            0xBEFE_040C, // s_mov_b64 exec, s[12:13]
        ]);
        let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
        let previous = lower::TEST_ZERO_MASK.replace(Some(on));
        let result = lowered(&native, &p, &HashSet::new());
        lower::TEST_ZERO_MASK.set(previous);
        result
    };
    const XOR_EXEC: u32 = 0x898E_7E7E; // s_xor_b64 s[14:15], exec, exec
    const OR_EXEC: u32 = 0x888E_7E7E; // s_or_b64 s[14:15], exec, exec
    const AND_ONE: u32 = 0x878E_817E; // s_and_b64 s[14:15], exec, 1
    const EXCHANGED: &str = "a lane mask exchanged where not every lane runs";
    // (as before: all three are refused, with the switch off)
    assert!(
        matches!(lower_mask(XOR_EXEC, false), Err(ShaderError::Unsupported { what, .. }) if what == EXCHANGED),
        "the empty mask with the switch off"
    );
    assert!(lower_mask(OR_EXEC, false).is_err(), "the copy of EXEC with the switch off");
    let dynamic = lower_mask(AND_ONE, true);
    assert!(
        matches!(&dynamic, Err(ShaderError::Unsupported { what, .. }) if *what == EXCHANGED),
        "{:?}",
        dynamic.err()
    );
    let empty = lower_mask(XOR_EXEC, true).expect("empty mask");
    let copied = lower_mask(OR_EXEC, true).expect("copy of EXEC");
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (shader, copied_mask) in [(&empty, false), (&copied, true)] {
        if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
            v.validate(&mut shader.dxil.clone()).expect("DXIL validation failed");
        }
        for below in [1, 32, 40, 64] {
            let bits = if copied_mask { u64::MAX >> (64 - below) } else { 0 };
            let expected = ((bits as u32) ^ (bits >> 32) as u32).wrapping_add(1);
            if let Some(actual) = on_the_card(shader, [below, 0]) {
                assert_eq!(actual, expected, "{below} active lanes, copied mask {copied_mask}");
            }
        }
    }
}

#[path = "paired_tests.rs"]
mod paired;
