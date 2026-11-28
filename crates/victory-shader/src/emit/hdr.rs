//! Colour outputs of pixel programs held to what the console's HDR formats
//! can hold (`VICTORY_HDR_CLAMP`), a child module so it shares the
//! emitter's state.

use super::Lower;
use crate::emit::bitcode::V;

/// Whether the float colours a pixel program writes are held to what every HDR format of the
/// console can hold, [-[`HDR_MAX`], [`HDR_MAX`]], with a NaN made the maximum
/// (`VICTORY_HDR_CLAMP=1`; unset or `0`: written as they are). See [`Lower::clamp_hdr`]. Read
/// once; part of the shader cache key, as every switch read by the translator.
fn hdr_clamp() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_HDR_CLAMP.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_HDR_CLAMP").is_some_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`hdr_clamp`], for its thread only.
    pub(in crate::emit) static TEST_HDR_CLAMP: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// Colour channels [`Lower::clamp_hdr`] held on this thread, for tests to count.
    pub(in crate::emit) static TEST_HDR_HELD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The largest magnitude [`hdr_clamp`] lets a colour channel have: R11G11B10_FLOAT's largest,
/// the smallest maximum of the console's HDR formats (R9G9B9E5 ends at 65408, a 16-bit float
/// at 65504).
pub(in crate::emit) const HDR_MAX: f32 = 65024.0;

impl Lower<'_> {
    /// A colour channel a pixel program writes, held to [-[`HDR_MAX`], [`HDR_MAX`]]
    /// ([`hdr_clamp`]). The console's HDR targets (R9G9B9E5, R11G11B10) saturate at their
    /// largest value; the host stands in a 16-bit float target for R9G9B9E5, where a value past
    /// 65504 is stored as infinity, and a tonemap reading it back makes black of it. A NaN
    /// becomes the maximum: an HDR value that overflowed is the brightest, not the darkest.
    /// The view's format is not known to the translation, so a 32-bit float colour target is
    /// held too; unorm and snorm targets saturate further anyway.
    ///
    /// After the mandy project (ProjectLeonida) commit 1411b6ca3, from aki-runtime 01f6f31,
    /// GPL-3.0.
    pub(super) fn clamp_hdr(&mut self, f: V) -> V {
        if !hdr_clamp() {
            return f;
        }
        #[cfg(test)]
        TEST_HDR_HELD.set(TEST_HDR_HELD.get() + 1);
        let c = self.cur;
        let min = self.s.m.f32((-HDR_MAX).to_bits());
        let max = self.s.m.f32(HDR_MAX.to_bits());
        // DXIL's FMax and FMin give the other operand for a NaN: tested apart.
        let lo = self.binary(35, f, min);
        let hi = self.binary(36, lo, max);
        let nan = self.is_nan_f32(f);
        self.s.m.select(c, nan, max, hi)
    }
}
