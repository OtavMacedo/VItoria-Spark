//! [`opt::FLOATS`]: identities of the zero factor rule the lowering adds to
//! the console's legacy multiplies (`select(a == 0 | b == 0, +0, a * b)`:
//! a zero factor makes the product +0 whatever the other factor is). They
//! hold for every operand, a NaN, an infinity and either zero included.
//!
//! A value is "zero with `x`" when it is a zero of either sign whenever `x`
//! is one: `x` itself, a product of two such values, and a value the rule
//! makes +0 for a zero `x`.
//!
//! - The square of a value that is zero with `x` is +0 for a zero `x`
//!   already: the rule on `x` alone is left out.
//! - Where one factor is zero with the other, the other factor's test is
//!   implied by its own: only the one is tested.

use super::{Bin, Const, Module, Op, TypeDef, V, pred};

/// How many values an "is zero with" answer looks through.
const DEPTH: u32 = 4;

impl Module {
    fn inst_op(&self, v: V) -> Option<&Op> {
        match v {
            V::Inst(i) => Some(&self.insts[i as usize].op),
            _ => None,
        }
    }

    /// Whether `v` is the float constant +0 (`negative` too: or -0).
    fn is_float_zero(&self, v: V, negative: bool) -> bool {
        let V::Const(k) = v else { return false };
        match self.consts[k as usize].1 {
            Const::Float(bits) => bits == 0 || (negative && bits == 0x8000_0000),
            _ => false,
        }
    }

    /// `x`, if `v` is the ordered test of the float `x` against a zero.
    fn zero_tested(&self, v: V) -> Option<V> {
        match *self.inst_op(v)? {
            Op::Cmp(pred::OEQ, x, k) if self.is_float_zero(k, true) => Some(x),
            Op::Cmp(pred::OEQ, k, x) if self.is_float_zero(k, true) => Some(x),
            _ => None,
        }
    }

    /// The factors of `v`, if it is a float product.
    fn float_product(&self, v: V) -> Option<(V, V)> {
        match *self.inst_op(v)? {
            Op::Bin(Bin::Mul, a, b)
                if matches!(self.types[self.type_of(v).0 as usize], TypeDef::Float) =>
            {
                Some((a, b))
            }
            _ => None,
        }
    }

    /// Whether the condition `c` holds whenever the float `x` is a zero:
    /// the test of a value that is zero with `x`, or an `or` of such a
    /// condition and any other.
    fn holds_at_zero(&self, c: V, x: V, depth: u32) -> bool {
        if let Some(v) = self.zero_tested(c) {
            return self.zero_with(v, x, depth);
        }
        match self.inst_op(c) {
            Some(&Op::Bin(Bin::Or, p, q)) if depth > 0 => {
                self.holds_at_zero(p, x, depth - 1) || self.holds_at_zero(q, x, depth - 1)
            }
            _ => false,
        }
    }

    /// Whether the float `v` is a zero (of either sign) whenever the float
    /// `x` is one.
    fn zero_with(&self, v: V, x: V, depth: u32) -> bool {
        if v == x {
            return true;
        }
        if depth == 0 {
            return false;
        }
        if let Some((a, b)) = self.float_product(v) {
            return self.zero_with(a, x, depth - 1) && self.zero_with(b, x, depth - 1);
        }
        matches!(
            self.inst_op(v),
            Some(&Op::Select(c, zero, _))
                if self.is_float_zero(zero, true) && self.holds_at_zero(c, x, depth - 1)
        )
    }

    /// The value instruction `i` always has, where one of the identities
    /// above says so.
    pub(super) fn float_identity(&self, i: u32) -> Option<V> {
        match self.insts[i as usize].op {
            Op::Select(c, zero, product) => {
                let x = self.zero_tested(c)?;
                let (a, b) = self.float_product(product)?;
                (a == b && self.is_float_zero(zero, false) && self.zero_with(a, x, DEPTH))
                    .then_some(product)
            }
            Op::Bin(Bin::Or, p, q) => {
                let (a, b) = (self.zero_tested(p)?, self.zero_tested(q)?);
                if self.zero_with(b, a, DEPTH) {
                    Some(q)
                } else if self.zero_with(a, b, DEPTH) {
                    Some(p)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::opt::{DEAD, FLOATS, FOLD, REUSE};
    use super::super::{Attr, Bin, Module, Op, Term, V, pred};

    /// One block of `main`, a float input function and an output function.
    struct Kit {
        m: Module,
        b: usize,
        input: V,
        output: V,
    }

    fn kit() -> Kit {
        let mut m = Module::default();
        let (i32t, f32t, void) = (m.int(32), m.float(), m.void());
        let fty = m.func_ty(f32t, vec![i32t]);
        m.ptr(fty);
        let input = m.function("dx.op.loadInput.f32", fty, Some(Attr::ReadNone), false);
        let fty = m.func_ty(void, vec![i32t, f32t]);
        m.ptr(fty);
        let output = m.function("dx.op.storeOutput.f32", fty, Some(Attr::NoUnwind), false);
        let main_ty = m.func_ty(void, vec![]);
        m.ptr(main_ty);
        m.function("main", main_ty, None, true);
        let b = m.new_block();
        Kit {
            m,
            b,
            input,
            output,
        }
    }

    impl Kit {
        fn input(&mut self, n: u32) -> V {
            let n = self.m.i32(n);
            self.m.call(self.b, self.input, vec![n])
        }

        fn is_zero(&mut self, x: V) -> V {
            let zero = self.m.f32(0);
            self.m.cmp(self.b, pred::OEQ, x, zero)
        }

        /// The legacy multiply as the lowering writes it: the product, the
        /// condition and the ruled value.
        fn legacy(&mut self, a: V, b: V) -> (V, V, V) {
            let product = self.m.bin(self.b, Bin::Mul, a, b);
            let zero = self.m.f32(0);
            let (az, bz) = (self.is_zero(a), self.is_zero(b));
            let either = self.m.bin(self.b, Bin::Or, az, bz);
            (
                product,
                either,
                self.m.select(self.b, either, zero, product),
            )
        }

        /// Sends each of `values` to the output, runs `passes` and gives
        /// the module and what the outputs got, in order.
        fn sent(mut self, values: &[V], passes: u32) -> (Module, Vec<V>) {
            let one = self.m.i32(1);
            for &v in values {
                self.m.call(self.b, self.output, vec![one, v]);
            }
            self.m.terminate(self.b, Term::Ret);
            self.m.optimise(passes);
            self.m.write();
            // By name: the clean-up renumbers the functions it keeps.
            let m = &self.m;
            let got = m.blocks[self.b]
                .body
                .iter()
                .filter_map(|&i| match &m.insts[i as usize].op {
                    Op::Call(f, args) if m.funcs[*f as usize].name.contains("storeOutput") => {
                        Some(args[1])
                    }
                    _ => None,
                })
                .collect();
            (self.m, got)
        }
    }

    const PASSES: u32 = FLOATS | FOLD | REUSE | DEAD;

    /// The condition of the select `v` is.
    fn condition(m: &Module, v: V) -> V {
        let V::Inst(i) = v else { panic!("{v:?}") };
        match m.insts[i as usize].op {
            Op::Select(c, ..) => c,
            ref op => panic!("{op:?}"),
        }
    }

    #[test]
    fn a_square_needs_no_zero_factor_rule() {
        let mut k = kit();
        let x = k.input(0);
        let (square, _, ruled) = k.legacy(x, x);
        let (_, got) = k.sent(&[ruled], PASSES);
        assert_eq!(got, [square]);
    }

    #[test]
    fn without_the_pass_the_rule_stays() {
        let mut k = kit();
        let x = k.input(0);
        let (_, _, ruled) = k.legacy(x, x);
        let (_, got) = k.sent(&[ruled], FOLD | REUSE | DEAD);
        assert_eq!(got, [ruled]);
    }

    #[test]
    fn other_products_and_other_zeros_keep_their_rule() {
        let mut k = kit();
        let (x, y) = (k.input(0), k.input(1));
        let (zero, minus_zero) = (k.m.f32(0), k.m.f32(0x8000_0000));
        let (square, _, _) = k.legacy(x, x);
        let (_, either, two) = k.legacy(x, y);
        let (xz, yz) = (k.is_zero(x), k.is_zero(y));
        // The other factor's test, and a -0 for the zero.
        let other = k.m.select(k.b, yz, zero, square);
        let negative = k.m.select(k.b, xz, minus_zero, square);
        let (m, got) = k.sent(&[two, other, negative], PASSES);
        assert_eq!(got, [two, other, negative]);
        assert_eq!(condition(&m, two), either, "both factors are tested");
    }

    #[test]
    fn a_factor_of_a_factor_is_tested_once() {
        let mut k = kit();
        let (x, y) = (k.input(0), k.input(1));
        // x * (x * y), (x * y) * y and x * (x * x): the inner product is a
        // zero whenever the shared factor is.
        let (_, _, inner) = k.legacy(x, y);
        let inner_zero = k.is_zero(inner);
        let (_, _, right) = k.legacy(x, inner);
        let (_, _, left) = k.legacy(inner, y);
        let (square, _, _) = k.legacy(x, x);
        let square_zero = k.is_zero(square);
        let (_, _, cube) = k.legacy(x, square);
        let (m, got) = k.sent(&[right, left, cube], PASSES);
        assert_eq!(condition(&m, got[0]), inner_zero);
        assert_eq!(condition(&m, got[1]), inner_zero);
        assert_eq!(condition(&m, got[2]), square_zero);
    }

    #[test]
    fn powers_and_longer_chains_are_tested_once() {
        let mut k = kit();
        let (x, y) = (k.input(0), k.input(1));
        // x^4 as the square of the square, x^5, and x * (x * (x * y)).
        let (square, _, _) = k.legacy(x, x);
        let (fourth, _, ruled_fourth) = k.legacy(square, square);
        let fourth_zero = k.is_zero(fourth);
        let (_, _, fifth) = k.legacy(x, ruled_fourth);
        let (_, _, inner) = k.legacy(x, y);
        let (_, _, middle) = k.legacy(x, inner);
        let middle_zero = k.is_zero(middle);
        let (_, _, outer) = k.legacy(x, middle);
        let (m, got) = k.sent(&[ruled_fourth, fifth, outer], PASSES);
        assert_eq!(got[0], fourth, "no rule on the square of a square");
        assert_eq!(condition(&m, got[1]), fourth_zero);
        assert_eq!(condition(&m, got[2]), middle_zero);
    }

    #[test]
    fn an_unruled_product_does_not_imply_its_factor_test() {
        let mut k = kit();
        let (x, y) = (k.input(0), k.input(1));
        // x * y is a NaN, not a zero, for a zero x and an infinite y.
        let plain = k.m.bin(k.b, Bin::Mul, x, y);
        let (_, either, outer) = k.legacy(x, plain);
        let (m, got) = k.sent(&[outer], PASSES);
        assert_eq!(condition(&m, got[0]), either);
    }

    /// The identities on the floats themselves, over the values where a
    /// rule could break: both zeros, subnormals, infinities and NaNs.
    #[test]
    fn the_identities_hold_for_every_class_of_float() {
        let legacy = |a: f32, b: f32| if a == 0.0 || b == 0.0 { 0.0 } else { a * b };
        let mut values = vec![0u32, 1, 0x007F_FFFF, 0x0080_0000, 0x1234_5678, 0x3F80_0000];
        values.extend([0x7F7F_FFFF, 0x7F80_0000, 0x7F80_0001, 0x7FC0_0000]);
        values.extend(values.clone().iter().map(|v| v | 0x8000_0000));
        let same = |a: f32, b: f32| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
        for &xb in &values {
            let x = f32::from_bits(xb);
            assert!(same(legacy(x, x), x * x), "{xb:#x}");
            let square = x * x;
            let tested_once = if square == 0.0 { 0.0 } else { x * square };
            assert!(same(legacy(x, square), tested_once), "{xb:#x}");
            let fourth = square * square;
            assert!(same(legacy(square, square), fourth), "{xb:#x}");
            let tested_once = if fourth == 0.0 { 0.0 } else { x * fourth };
            assert!(same(legacy(x, fourth), tested_once), "{xb:#x}");
            for &yb in &values {
                let y = f32::from_bits(yb);
                let inner = legacy(x, y);
                let tested_once = if inner == 0.0 { 0.0 } else { x * inner };
                assert!(same(legacy(x, inner), tested_once), "{xb:#x} {yb:#x}");
                let tested_once = if inner == 0.0 { 0.0 } else { inner * y };
                assert!(same(legacy(inner, y), tested_once), "{xb:#x} {yb:#x}");
            }
        }
    }
}
