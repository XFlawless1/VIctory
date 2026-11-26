//! First-lane reads of a value every lane of the console wave holds alike, where not every
//! lane runs.
//!
//! A console wave64 program run as two host waves of 32 reads the first lane of EXEC with
//! `v_readfirstlane_b32`. Where every lane runs, the two halves ask each other (two group
//! barriers) which half has the console's first lane. Where only some lanes run, the halves
//! cannot meet: a barrier inside a per-lane branch is waited for by threads that never come.
//! But the answer needs no one else when every lane that is in EXEC at the read holds the same
//! value in the register read: the host wave's own first live lane then holds what the
//! console's first lane holds, and a host wave with no live lane does not run the read at all.
//!
//! The title's block scans (the ray-tracing structure builder's prefix sums over 128 to 512
//! threads) read exactly such a value. After each wave has written its total to LDS and the
//! group's barrier, every wave of index `w` adds the totals of the waves below it, one at a
//! time, in a loop that runs `w` times, and hands the running sum from one round to the next
//! through a first-lane read of it:
//!
//! ```text
//! sum = 0;
//! for (k = 0; k < w; k++) {
//!     sum_in = readfirstlane(sum_v);   // sum_v: the same in every lane that is still looping
//!     sum_v  = sum_in + lds[k];        // a scalar plus a read of an address that is a scalar
//! }
//! ```
//!
//! [`find`] proves, for each such read, that the value read is made only of what is the same
//! for the whole console wave, with nothing taken on trust:
//!
//! - it is the result of integer vector ALU instructions ([`PURE`]: each writes all 32 bits from
//!   its operands) that every arriving lane runs (no guard), whose operands are scalars (not
//!   lane masks) or values of the same kind; or of a read of group-shared memory at an address
//!   of that kind, where the group's barrier comes first and no instruction that writes
//!   group-shared memory can follow it (so the host's two waves read the same dword whenever
//!   they read it);
//! - it comes from no phi and no entry register (a thread index, say: those differ by lane);
//! - every value of it is made in the loops the reader is in, or fewer (a lane that left a
//!   loop earlier holds the value of an earlier round: such a value is read outside the loop
//!   only if the loop's lanes leave it together, which this does not look into).
//!
//! Anything else is refused there as before ("a first-lane read where not every lane runs")
//! and left to the forms of the program in which every lane runs that code. `VICTORY_FIRSTLANE_ALIKE=0` turns this off.

use std::collections::{HashMap, HashSet};

use crate::gfx10::{Fields, Operand};
use crate::ir::{self, EXEC, Exit, Program, Reg, VCC, Value};
use crate::structure::Structured;

type Why = &'static str;

const NOT_MADE: Why = "a value joined or given on entry, not made by an instruction";
const LOOPS: Why = "a value of a loop the read is outside of";
const GUARDED: Why = "a value only some of the lanes that arrive make";
const MASK: Why = "a lane mask as an operand";
const KIND: Why = "a value made by an instruction that is not a plain integer operation on its operands";
const LDS: Why = "a read of group-shared memory without a barrier before it, or with writes after";
const INDIRECT: Why = "an indirect jump";

/// `VICTORY_FIRSTLANE_ALIKE=0`: a first-lane read where not every lane runs is refused as
/// before.
pub(super) fn on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_ALIKE.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FIRSTLANE_ALIKE").is_none_or(|v| v != "0"))
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`on`], for its thread only.
    pub(super) static TEST_ALIKE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

struct Ctx<'a> {
    p: &'a Program,
    st: &'a Structured,
    /// Each block's immediate dominator (`usize::MAX`: not reached).
    idom: Vec<usize>,
    /// The headers of the loops each block lies in, sorted.
    loops: Vec<Vec<usize>>,
    /// The instruction (block, index) making each VGPR value.
    def: HashMap<Value, (usize, usize)>,
    /// Every `s_barrier`, and every instruction that writes group-shared memory.
    barriers: Vec<(usize, usize)>,
    writers: Vec<(usize, usize)>,
    /// Blocks that end the program.
    ends: Vec<usize>,
    memo: HashMap<Value, Result<(), Why>>,
}

/// The results of the `v_readfirstlane_b32` instructions of `p` whose source every lane in EXEC
/// holds alike (see the module). With `why`, the reasons for the others are printed.
pub(super) fn find(p: &Program, st: &Structured, why: bool) -> HashSet<Value> {
    let mut out = HashSet::new();
    let reads: Vec<(usize, usize)> = p
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| {
            block
                .insts
                .iter()
                .enumerate()
                .filter(|(_, i)| i.name() == "v_readfirstlane_b32")
                .map(move |(i, _)| (b, i))
        })
        .collect();
    if reads.is_empty() {
        return out;
    }
    let Ok(mut ctx) = Ctx::new(p, st) else {
        return out;
    };
    for (b, i) in reads {
        let inst = &p.blocks[b].insts[i];
        let (Fields::Valu(v), Some(&(_, result))) = (&inst.fields, inst.writes.first()) else {
            continue;
        };
        let Some(&Operand::Vector { reg, .. }) = v.src.first() else {
            continue;
        };
        let Some(x) = inst.read(Reg::V(reg)) else {
            continue;
        };
        match ctx.alike(x, b) {
            Ok(()) => {
                out.insert(result);
            }
            Err(reason) => {
                if why {
                    eprintln!(
                        "WAVE64-ALIKE first-lane read at {:#x}: not proven the same in every lane: {reason}",
                        p.blocks[b].offset
                    );
                }
            }
        }
    }
    out
}

impl<'a> Ctx<'a> {
    fn new(p: &'a Program, st: &'a Structured) -> Result<Self, Why> {
        let n = p.blocks.len();
        let order = ir::reverse_postorder(&p.blocks);
        let mut index = vec![usize::MAX; n];
        for (k, &b) in order.iter().enumerate() {
            index[b] = k;
        }
        if p.blocks.iter().any(|b| matches!(b.exit, Exit::Indirect)) {
            return Err(INDIRECT);
        }
        let idom = ir::idoms(&p.blocks, &order, &index);
        let dominates = |a: usize, mut b: usize| {
            loop {
                if a == b {
                    return true;
                }
                if idom[b] == b || idom[b] == usize::MAX {
                    return false;
                }
                b = idom[b];
            }
        };
        // natural loops: the blocks that reach a retreating edge's source without passing its
        // header (a retreating edge whose target does not dominate its source is irreducible)
        let mut loops: Vec<Vec<usize>> = vec![Vec::new(); n];
        for &u in &order {
            for h in p.blocks[u].exit.successors() {
                if index[h] > index[u] {
                    continue;
                }
                if !dominates(h, u) {
                    return Err("an irreducible loop");
                }
                let mut inside = vec![false; n];
                inside[h] = true;
                let mut stack = vec![u];
                while let Some(b) = stack.pop() {
                    if inside[b] {
                        continue;
                    }
                    inside[b] = true;
                    stack.extend(p.blocks[b].preds.iter().copied().filter(|&q| index[q] != usize::MAX));
                }
                for b in (0..n).filter(|&b| inside[b]) {
                    if !loops[b].contains(&h) {
                        loops[b].push(h);
                    }
                }
            }
        }
        for l in &mut loops {
            l.sort_unstable();
        }
        let mut def = HashMap::new();
        let (mut barriers, mut writers, mut ends) = (Vec::new(), Vec::new(), Vec::new());
        for &b in &order {
            let block = &p.blocks[b];
            if matches!(block.exit, Exit::End) {
                ends.push(b);
            }
            for (i, inst) in block.insts.iter().enumerate() {
                for &(reg, v) in &inst.writes {
                    if matches!(reg, Reg::V(_)) {
                        def.insert(v, (b, i));
                    }
                }
                if inst.name() == "s_barrier" {
                    barriers.push((b, i));
                }
                if writes_lds(inst) {
                    writers.push((b, i));
                }
            }
        }
        Ok(Ctx { p, st, idom, loops, def, barriers, writers, ends, memo: HashMap::new() })
    }

    fn dominates(&self, a: usize, mut b: usize) -> bool {
        loop {
            if a == b {
                return true;
            }
            if self.idom[b] == b || self.idom[b] == usize::MAX {
                return false;
            }
            b = self.idom[b];
        }
    }

    /// Whether every loop `def` lies in, `user` lies in.
    fn within(&self, def: usize, user: usize) -> bool {
        self.loops[def].iter().all(|h| self.loops[user].contains(h))
    }

    /// `x`, read in block `user`, is the same in every lane that is in EXEC there.
    fn alike(&mut self, x: Value, user: usize) -> Result<(), Why> {
        let &(b, i) = self.def.get(&x).ok_or(NOT_MADE)?;
        if !self.within(b, user) {
            return Err(LOOPS);
        }
        if let Some(r) = self.memo.get(&x) {
            return *r;
        }
        let r = self.made_alike(b, i);
        self.memo.insert(x, r);
        r
    }

    /// Instruction `i` of block `b` makes the same value in every lane that runs it, if its
    /// operands are the same in every lane.
    fn made_alike(&mut self, b: usize, i: usize) -> Result<(), Why> {
        let p = self.p;
        let inst = &p.blocks[b].insts[i];
        if self.st.guards[b][i].is_some() {
            return Err(GUARDED);
        }
        match &inst.fields {
            Fields::Valu(v) => {
                // Only instructions that write all 32 bits of their destination from their
                // operands: another's old value (a half kept, an accumulator) is no operand
                // of the names the IR lists, so none of those is taken.
                if !PURE.contains(&inst.name())
                    || v.dpp.is_some()
                    || v.sdwa.is_some()
                    || v.abs != 0
                    || v.neg != 0
                    || v.opsel != 0
                    || v.omod != 0
                {
                    return Err(KIND);
                }
                // a lane mask (a pair of dwords, or one the structure follows) is read bit by bit
                if v.src.iter().any(|o| matches!(o, Operand::Scalar { dwords, .. } if *dwords > 1)) {
                    return Err(MASK);
                }
                let operands = ir::operand_reads(p, inst);
                for &(reg, val) in &inst.reads {
                    match reg {
                        // every lane that arrives is in EXEC (no guard)
                        Reg::S(c) if c == EXEC || c == EXEC + 1 => {}
                        Reg::S(c) if c == VCC || c == VCC + 1 => return Err(MASK),
                        Reg::S(_) => {
                            if self.st.masks.contains_key(&val) || self.st.highs.contains_key(&val) {
                                return Err(MASK);
                            }
                        }
                        Reg::Scc => {}
                        Reg::V(_) if operands.contains(&reg) => self.alike(val, b)?,
                        // (the old value of the register written: no lane keeps it, no guard)
                        Reg::V(_) => {}
                    }
                }
                Ok(())
            }
            Fields::Ds { addr: Some(Operand::Vector { reg, .. }), gds: false, .. }
                if inst.name() == "ds_read_b32" =>
            {
                let a = inst.read(Reg::V(*reg)).ok_or(NOT_MADE)?;
                self.alike(a, b)?;
                if !self.lds_stable(b, i) {
                    return Err(LDS);
                }
                Ok(())
            }
            _ => Err(KIND),
        }
    }

    /// The group-shared memory `ds_read_b32` at instruction `i` of block `b` reads is not
    /// written by any thread of the group while it is read: a group barrier that every
    /// thread passes (it dominates every end of the program) comes before it, and no
    /// instruction that writes group-shared memory comes after that barrier.
    fn lds_stable(&self, b: usize, i: usize) -> bool {
        // (the title's barriers wait for the group: `VICTORY_GROUP_SYNC=0` and `1` let some
        // of them go by)
        if super::group_sync() != 2 {
            return false;
        }
        self.barriers.iter().any(|&(bb, bi)| {
            let before = if bb == b { bi < i } else { self.dominates(bb, b) };
            if !before || !self.ends.iter().all(|&e| self.dominates(bb, e)) {
                return false;
            }
            // the blocks reachable after the barrier's own block (it too, if a loop returns)
            let mut seen = vec![false; self.p.blocks.len()];
            let mut stack = self.p.blocks[bb].exit.successors();
            while let Some(c) = stack.pop() {
                if std::mem::replace(&mut seen[c], true) {
                    continue;
                }
                stack.extend(self.p.blocks[c].exit.successors());
            }
            self.writers.iter().all(|&(wb, wi)| !(seen[wb] || wb == bb && wi > bi))
        })
    }
}

/// Integer vector ALU instructions whose result is all 32 bits of a function of their operands:
/// no lane, no mask, no old value of the register written, nothing from another lane.
const PURE: [&str; 32] = [
    "v_mov_b32",
    "v_not_b32",
    "v_add_nc_u32",
    "v_sub_nc_u32",
    "v_subrev_nc_u32",
    "v_and_b32",
    "v_or_b32",
    "v_xor_b32",
    "v_xnor_b32",
    "v_lshlrev_b32",
    "v_lshrrev_b32",
    "v_ashrrev_i32",
    "v_min_u32",
    "v_max_u32",
    "v_min_i32",
    "v_max_i32",
    "v_mul_lo_u32",
    "v_mul_hi_u32",
    "v_mul_hi_i32",
    "v_mul_u32_u24",
    "v_mul_hi_u32_u24",
    "v_mul_i32_i24",
    "v_mul_hi_i32_i24",
    "v_add3_u32",
    "v_lshl_add_u32",
    "v_add_lshl_u32",
    "v_lshl_or_b32",
    "v_and_or_b32",
    "v_or3_b32",
    "v_xad_u32",
    "v_bfe_u32",
    "v_bfe_i32",
];

/// Whether `inst` can write group-shared memory.
fn writes_lds(inst: &ir::Inst) -> bool {
    let name = inst.name();
    match &inst.fields {
        Fields::Ds { .. } => {
            !(name.starts_with("ds_read")
                || name.starts_with("ds_swizzle")
                || name.starts_with("ds_bpermute")
                || name.starts_with("ds_permute"))
        }
        Fields::Buffer { lds, .. } | Fields::Flat { lds, .. } => *lds,
        _ => false,
    }
}
