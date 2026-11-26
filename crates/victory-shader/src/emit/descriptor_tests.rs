//! A descriptor set inside a branch that skips code when no lane is left for it, and read
//! after the join by an access EXEC guards: with a branch around left out
//! ([`crate::ir::without_early_outs`]) or taken wave-wide
//! ([`crate::structure::structure_wide`]) a lane outside EXEC reaches the access too, and
//! only the lanes that run it tell which edge the descriptor came by. Traced, lowered
//! for a host of 32-lane waves, validated by `dxil.dll` when present and run on the host GPU
//! when it takes waves of 32.

use std::collections::HashSet;

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::resource::{Role, Source};
use crate::{ShaderError, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
const S_CBRANCH_EXECZ: u32 = 0xBF88_0000;
/// `buffer_store_dword v0, off, s[8:11], 0`.
const STORE: [u32; 2] = [0x38 << 26 | 0x1C << 18, 0x80 << 24 | 2 << 16];
/// `s_load_dwordx4 s[8:11], s[16:17], 0`.
const LOAD: [u32; 2] = [0x3D << 26 | 0x02 << 18 | 8 << 6 | 8, 125 << 25];
/// `image_get_lod v[4:5], v[0:2], s[24:31], s[8:11]` of a 2D array: an access with a sampler.
const SAMPLED: [u32; 2] = [0xF180_0328, 0x0046_0400];
/// `s_mov_b32` of 0 to each of s8..s11: the dwords of an immediate sampler.
const SAMPLER: [u32; 4] = [0xBE88_0380, 0xBE89_0380, 0xBE8A_0380, 0xBE8B_0380];

/// A wave64 compute program of 64 threads in one console wave (the thread index in v0), 4
/// root constants in s0..s3 and a table in user data 4. Each thread t holds the bit
/// `1 << (t >> 1)`. The threads below s0 OR theirs over the wave; the others leave by the
/// first `s_cbranch_execz`, an early-out to the program's end. Past it a second branch, on those threads and the
/// program's own (every one of them: never skipped while a thread is left), runs `set` on
/// s[8:11], which hold lane masks until then; `access` follows its join with the OR in v0.
fn program(set: &[u32], access: &[u32]) -> NativeProgram {
    let mut inner = vec![
        0x7E00_0207,                        // v_mov_b32 v0, s7
        0xBE8E_2414,                        // s_and_saveexec_b64 s[14:15], s[20:21]
        S_CBRANCH_EXECZ | set.len() as u32, // past `set`
    ];
    inner.extend_from_slice(set);
    inner.push(0xBEFE_040E); // s_mov_b64 exec, s[14:15]
    inner.extend_from_slice(access);
    let reduction = &super::wave::WAVE_OR_V7_TO_S7;
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xBE90_0304, // s_mov_b32 s16, s4 (the reduction saves EXEC in s[4:5])
        0xBE91_0380, // s_mov_b32 s17, 0
        0xF40C_0602,
        0xFA00_0000, // s_load_dwordx8 s[24:31], s[4:5], 0
        0xBE94_047E, // s_mov_b64 s[20:21], exec
        0xBE88_047E, // s_mov_b64 s[8:9], exec
        0xBE8A_047E, // s_mov_b64 s[10:11], exec
        0x7E0A_0281, // v_mov_b32 v5, 1
        0x2C0C_0081, // v_lshrrev_b32 v6, 1, v0
        0x340E_0B06, // v_lshlrev_b32 v7, v6, v5
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        S_CBRANCH_EXECZ | (reduction.len() + inner.len()) as u32,
    ];
    words.extend_from_slice(reduction);
    words.extend_from_slice(&inner);
    native(&words)
}

/// A wave64 compute program of 64 threads with 4 root constants in s0..s3 and the table of
/// a raw UAV in user data 4: `body`, then the end.
fn native(body: &[u32]) -> NativeProgram {
    let mut words = body.to_vec();
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
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
            threads: [64, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// The blocks an instruction called `name` closes, in code order.
fn closed_by(p: &ir::Program, name: &str) -> Vec<usize> {
    let mut blocks: Vec<usize> = (0..p.blocks.len())
        .filter(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == name))
        .collect();
    blocks.sort_by_key(|&b| p.blocks[b].insts[0].machine.offset);
    blocks
}

/// What the descriptors of `native` trace to with the branches closing `wide` wave-wide.
fn traced(
    native: &NativeProgram,
    p: &ir::Program,
    wide: &HashSet<usize>,
) -> Result<resource::Resources, ShaderError> {
    let st = structure::structure_wide(p, wide).expect("structure");
    resource::track(p, &native.layout, native.stage, Some(&st))
}

/// `native` for a host of 32-lane waves, with the branches closing `wide` taken wave-wide.
fn lowered(
    native: &NativeProgram,
    p: &ir::Program,
    wide: &HashSet<usize>,
) -> Result<HostShader, ShaderError> {
    let st = structure::structure_wide(p, wide).expect("structure");
    let res = resource::track(p, &native.layout, native.stage, Some(&st))?;
    let io = crate::io::raise(p, native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
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

/// The wave-wide forms of `program`: the outer skip alone, then the inner one too.
fn wide_forms(p: &ir::Program) -> [HashSet<usize>; 2] {
    let skips = closed_by(p, "s_cbranch_execz");
    assert_eq!(skips.len(), 2);
    [HashSet::from([skips[0]]), HashSet::from_iter(skips)]
}

#[test]
fn a_descriptor_loaded_past_a_wave_wide_branch_is_the_one_the_running_lanes_loaded() {
    let native = program(&LOAD, &STORE);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let load = |r: &resource::Resources| {
        let [d] = &r.descriptors[..] else {
            panic!("{:?}", r.descriptors)
        };
        let Source::Loaded { load, first: 0 } = d.source else {
            panic!("{:?}", d.source)
        };
        assert_eq!(p.blocks[load.0].insts[load.1].name(), "s_load_dwordx4");
    };
    // (each lane deciding alone, a lane that reaches the store ran the load)
    load(&traced(&native, &p, &HashSet::new()).expect("per lane"));
    // Without the outer skip every lane reaches the store, each deciding alone; with it
    // taken wave-wide every lane of a wave that has one below s0 does.
    let unskipped = ir::without_early_outs(&p).expect("the outer skip is an early-out");
    let [outer, both] = wide_forms(&p);
    for (p, wide) in [(&unskipped, HashSet::new()), (&p, outer), (&p, both)] {
        load(&traced(&native, p, &wide).expect("EXEC as a guard"));
        let shader = lowered(&native, p, &wide).expect("lower");
        if let Ok(mut v) =
            validator::Validator::load(validator::Validator::default_path().as_deref())
        {
            let mut copy = shader.dxil.clone();
            v.validate(&mut copy).expect("DXIL validation failed");
        }
        // (threads below s0 take the branch; thread t gives bit t >> 1; none stores nothing)
        for (below, expected) in [
            (64u32, u32::MAX),
            (40, 0x000F_FFFF),
            (33, 0x0001_FFFF),
            (10, 0x1F),
            (1, 1),
            (0, 0),
        ] {
            let Some(stored) = on_the_card(&shader, below) else {
                return;
            };
            assert_eq!(stored, expected, "{below} threads in the branch");
        }
    }
    eprintln!("ran on the hardware device: the store went to the loaded view in every case");
}

#[test]
fn an_immediate_sampler_set_past_a_wave_wide_branch_is_the_one_the_running_lanes_set() {
    let native = program(&SAMPLER, &SAMPLED);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let unskipped = ir::without_early_outs(&p).expect("the outer skip is an early-out");
    let [outer, both] = wide_forms(&p);
    for (p, wide) in [(&unskipped, HashSet::new()), (&p, outer), (&p, both)] {
        let r = traced(&native, p, &wide).expect("EXEC as a guard");
        let sampler = r.descriptors.iter().find(|d| d.role == Role::Sampler);
        assert_eq!(
            sampler.map(|d| &d.source),
            Some(&Source::Constant(vec![0; 4]))
        );
    }
}

#[test]
fn a_lane_that_follows_its_wave_past_a_load_has_not_loaded() {
    // `s_cbranch_execnz` passes the load of the view's descriptor when some lane is below
    // s0; the lanes at or above s0 store after it.
    let native = native(&[
        0xBE85_0380,     // s_mov_b32 s5, 0
        0xBE88_047E,     // s_mov_b64 s[8:9], exec
        0xBE8A_047E,     // s_mov_b64 s[10:11], exec
        0x7D88_0000,     // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A,     // s_and_saveexec_b64 s[12:13], vcc
        0xBF89_0000 | 2, // s_cbranch_execnz past the load
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25,   // s_load_dwordx4 s[8:11], s[4:5], 0
        0x8AFE_7E0C, // s_andn2_b64 exec, s[12:13], exec
        STORE[0],
        STORE[1],
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    ]);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let [branch] = closed_by(&p, "s_cbranch_execnz")[..] else {
        panic!("one branch")
    };
    // Wave-wide, one lane below s0 takes the whole wave past the load: a lane that stores
    // has its own bit clear and still came that way, with lane masks for a descriptor.
    let r = traced(&native, &p, &HashSet::from([branch]));
    assert!(
        matches!(&r, Err(ShaderError::Unsupported { what, .. }) if *what == "a descriptor neither loaded nor built"),
        "{:?}",
        r.map(|r| r.descriptors)
    );
}

/// Runs `shader` on the hardware device with the root constant `s0` and gives dword 0 of
/// the UAV; None (and a line) without a device that takes it.
fn on_the_card(shader: &HostShader, s0: u32) -> Option<u32> {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
        if let (RootType::Constants, 0, 0) = (u.root, u.slot, u.dword) {
            ud[u.index as usize] = s0;
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
