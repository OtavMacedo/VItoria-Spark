//! Quad permutations (`ds_swizzle_b32` in quad mode, DPP `quad_perm`) with
//! `bitcode::opt::QUADS`: the instruction fixes which lane of the quad each
//! place reads, so only the lanes it names are read and the place picks
//! among them (a swap across an axis is the host's own quad operation),
//! where the general lowering reads all four lanes and picks by a source
//! lane it computes. Each lane gets the same lane's value either way.

use super::Lower;
use crate::emit::bitcode::{Attr, Bin, V, pred};

/// The quad lane each place reads under the 8-bit permutation `perm`: two
/// bits a place, the first place lowest.
pub(super) fn permuted_places(perm: u32) -> [u32; 4] {
    [0, 1, 2, 3].map(|place| perm >> (2 * place) & 3)
}

/// The quad lane each place reads under a `ds_swizzle_b32` whose 16-bit
/// offset field is `offset` and keeps lanes within their quad: a
/// permutation (bit 15) or and, or and xor masks on the lane index.
pub(super) fn swizzle_places(offset: u32) -> [u32; 4] {
    if offset & 0x8000 != 0 {
        return permuted_places(offset & 0xFF);
    }
    let (and, or, xor) = (offset & 0x1F, offset >> 5 & 0x1F, offset >> 10 & 0x1F);
    [0, 1, 2, 3].map(|place| ((place & and | or) ^ xor) & 3)
}

impl Lower<'_> {
    /// `x` of the quad lane `from` names for this lane's place in its quad
    /// (`lane`'s low two bits), in the current block.
    pub(super) fn quad_permute(&mut self, x: V, lane: V, from: [u32; 4]) -> V {
        let c = self.cur;
        let i32t = self.s.t.i32;
        // Every place reads its neighbour across an axis: one quad
        // operation (0 across x, 1 across y, 2 across both).
        let across = match from {
            [1, 0, 3, 2] => Some(0),
            [2, 3, 0, 1] => Some(1),
            [3, 2, 1, 0] => Some(2),
            _ => None,
        };
        if let Some(axis) = across {
            let axis = self.s.m.i8(axis);
            return self
                .s
                .op(c, "quadOp.i32", Attr::NoUnwind, i32t, 123, &[x, axis]);
        }
        let mut reads = [None; 4];
        let mut read = |l: &mut Self, n: u32| {
            *reads[n as usize].get_or_insert_with(|| {
                let kn = l.s.m.i32(n);
                l.s.op(c, "quadReadLaneAt.i32", Attr::NoUnwind, i32t, 122, &[x, kn])
            })
        };
        // The lane most places read needs no test of the place.
        let rest = (0..4)
            .max_by_key(|&n| from.iter().filter(|&&f| f == n).count())
            .unwrap_or(0);
        let mut r = read(self, rest);
        if from.iter().all(|&f| f == rest) {
            return r;
        }
        let three = self.s.m.i32(3);
        let place = self.s.m.bin(c, Bin::And, lane, three);
        for (n, &f) in from.iter().enumerate() {
            if f != rest {
                let kn = self.s.m.i32(n as u32);
                let is = self.s.m.cmp(c, pred::EQ, place, kn);
                let v = read(self, f);
                r = self.s.m.select(c, is, v, r);
            }
        }
        r
    }
}
