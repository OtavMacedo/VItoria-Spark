//! [`opt::READS`]: a read of resource memory gives the result of an equal
//! read every path to it has run, in any block, where nothing the program
//! does can change what the read sees.
//!
//! That holds for a program without a store, an atomic or a barrier on
//! resource memory: draws and dispatches run one after another, so only the
//! program's own invocations run beside each other, and none of them
//! writes. Reads whose result depends on the lanes that run them (a sample
//! that takes its own derivatives) are left to the walk's per-block reuse.

use super::opt::{self, Effect, WriteKind};
use super::{Module, Op};

/// The `dx.op` reads whose result is the memory's alone: explicit level or
/// gradient, or no filtering at all.
const PLACED: [&str; 8] = [
    "rawBufferLoad",
    "bufferLoad",
    "textureLoad",
    "sampleLevel",
    "sampleGrad",
    "sampleCmpLevelZero",
    "textureGather",
    "textureGatherCmp",
];

impl Module {
    /// Whether no reached or unreached instruction writes resource memory:
    /// stage outputs and a discard write none.
    pub(super) fn memory_frozen(&self, effects: &[Effect]) -> bool {
        self.blocks.iter().flat_map(|b| &b.body).all(|&i| {
            let Op::Call(f, _) = &self.insts[i as usize].op else {
                return true;
            };
            let func = &self.funcs[*f as usize];
            effects[*f as usize] != Effect::Writes
                || match opt::write_kind(func) {
                    WriteKind::OutputOnly => true,
                    WriteKind::All => func.name == "dx.op.discard",
                    WriteKind::ResourceMemory | WriteKind::Barrier => false,
                }
        })
    }

    /// Whether instruction `i` is a read that gives the same result in
    /// every block below it while memory is frozen.
    pub(super) fn reads_below(&self, i: u32) -> bool {
        let Op::Call(f, _) = &self.insts[i as usize].op else {
            return false;
        };
        let name = &self.funcs[*f as usize].name;
        let name = name.strip_prefix("dx.op.").unwrap_or(name);
        let name = name.split('.').next().unwrap_or(name);
        PLACED.contains(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::super::opt::{LOADS, READS, REUSE};
    use super::super::{Attr, Module, Op, Term, V, pred};

    /// An entry that branches to `then` or `other`, which meet in `join`,
    /// with a read, a store and an output function.
    struct Diamond {
        m: Module,
        blocks: [usize; 4],
        read: V,
        store: V,
        output: V,
    }

    fn diamond(read: &str) -> Diamond {
        let mut m = Module::default();
        let (i32t, void) = (m.int(32), m.void());
        let fty = m.func_ty(i32t, vec![i32t, i32t]);
        m.ptr(fty);
        let read = m.function(read, fty, Some(Attr::ReadOnly), false);
        let store = m.function("dx.op.rawBufferStore.i32", fty, Some(Attr::NoUnwind), false);
        let output = m.function("dx.op.storeOutput.i32", fty, Some(Attr::NoUnwind), false);
        let main_ty = m.func_ty(void, vec![]);
        m.ptr(main_ty);
        m.function("main", main_ty, None, true);
        let blocks = [m.new_block(), m.new_block(), m.new_block(), m.new_block()];
        Diamond {
            m,
            blocks,
            read,
            store,
            output,
        }
    }

    /// Reads (1, 2) in the entry and in each other block, each sent to an
    /// output; `extra` adds to the entry before the read. The values the
    /// outputs of the four blocks got, and the entry's read.
    fn outputs(read: &str, passes: u32, extra: impl FnOnce(&mut Diamond)) -> ([V; 4], V) {
        let mut d = diamond(read);
        let [entry, then, other, join] = d.blocks;
        let (one, two) = (d.m.i32(1), d.m.i32(2));
        extra(&mut d);
        let mut reads = [one; 4];
        for (n, b) in d.blocks.into_iter().enumerate() {
            reads[n] = d.m.call(b, d.read, vec![one, two]);
            d.m.call(b, d.output, vec![one, reads[n]]);
        }
        let c = d.m.cmp(entry, pred::IULT, reads[0], two);
        d.m.terminate(entry, Term::CondBr(c, then, other));
        d.m.terminate(then, Term::Br(join));
        d.m.terminate(other, Term::Br(join));
        d.m.terminate(join, Term::Ret);
        d.m.optimise(passes);
        d.m.write();
        let got = d.blocks.map(|b| {
            let sent = d.m.blocks[b]
                .body
                .iter()
                .find_map(|&i| match &d.m.insts[i as usize].op {
                    Op::Call(f, args) if d.m.funcs[*f as usize].name.contains("storeOutput") => {
                        Some(args[1])
                    }
                    _ => None,
                });
            sent.expect("an output in every block")
        });
        (got, reads[0])
    }

    #[test]
    fn a_read_is_reused_in_every_block_below_it() {
        let (got, first) = outputs("dx.op.rawBufferLoad.i32", REUSE | LOADS | READS, |_| {});
        assert_eq!(got, [first; 4]);
    }

    #[test]
    fn without_the_pass_each_block_reads() {
        let (got, first) = outputs("dx.op.rawBufferLoad.i32", REUSE | LOADS, |_| {});
        assert_eq!(got[0], first);
        assert!(got[1..].iter().all(|&v| v != first), "{got:?}");
    }

    #[test]
    fn a_read_beside_a_block_is_not_reused_in_it() {
        let mut d = diamond("dx.op.rawBufferLoad.i32");
        let [entry, then, other, join] = d.blocks;
        let (one, two) = (d.m.i32(1), d.m.i32(2));
        let c = d.m.cmp(entry, pred::IULT, one, two);
        let beside = d.m.call(then, d.read, vec![one, two]);
        d.m.call(then, d.output, vec![one, beside]);
        let own = [other, join].map(|b| {
            let r = d.m.call(b, d.read, vec![one, two]);
            d.m.call(b, d.output, vec![one, r]);
            r
        });
        d.m.terminate(entry, Term::CondBr(c, then, other));
        d.m.terminate(then, Term::Br(join));
        d.m.terminate(other, Term::Br(join));
        d.m.terminate(join, Term::Ret);
        d.m.optimise(READS);
        for (b, r) in [other, join].into_iter().zip(own) {
            let V::Inst(i) = r else { unreachable!() };
            assert!(
                d.m.blocks[b].body.contains(&i),
                "block {b} reads for itself"
            );
        }
        d.m.write();
    }

    #[test]
    fn a_store_anywhere_in_the_program_keeps_every_read() {
        let (got, first) = outputs("dx.op.rawBufferLoad.i32", REUSE | LOADS | READS, |d| {
            let (b, one) = (d.blocks[3], d.m.i32(1));
            d.m.call(b, d.store, vec![one, one]);
        });
        assert!(got[1..].iter().all(|&v| v != first), "{got:?}");
    }

    #[test]
    fn a_barrier_keeps_every_read_and_a_discard_does_not() {
        for (name, kept) in [("dx.op.barrier", true), ("dx.op.discard", false)] {
            let (got, first) = outputs("dx.op.rawBufferLoad.i32", REUSE | LOADS | READS, |d| {
                let (i32t, void) = (d.m.int(32), d.m.void());
                let fty = d.m.func_ty(void, vec![i32t, i32t]);
                d.m.ptr(fty);
                let f = d.m.function(name, fty, Some(Attr::NoUnwind), false);
                let one = d.m.i32(1);
                d.m.call(d.blocks[0], f, vec![one, one]);
            });
            assert_eq!(
                got[1..].iter().all(|&v| v != first),
                kept,
                "{name}: {got:?}"
            );
        }
    }

    #[test]
    fn a_sample_with_its_own_derivatives_is_read_in_each_block() {
        let (got, first) = outputs("dx.op.sample.f32", REUSE | LOADS | READS, |_| {});
        assert!(got[1..].iter().all(|&v| v != first), "{got:?}");
        let (got, first) = outputs("dx.op.sampleLevel.f32", REUSE | LOADS | READS, |_| {});
        assert_eq!(got, [first; 4]);
    }
}
