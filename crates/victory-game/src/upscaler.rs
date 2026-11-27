//! The title's FidelityFX upscaler path, patched in for DLSS (`VICTORY_DLSS`).
//!
//! The title renders through that path (renderer 7) only on a Series S with
//! the upscaler's capability; otherwise it takes its own temporal pass
//! (renderer 6), and it never loads its upscaler. DLSS takes the path's
//! upscale dispatches over (`victory_hle::modules::fidelityfx`), so with DLSS
//! available a known build's renderer selection is patched to pick 7 every
//! time, and the path's render size code to note the asked FidelityFX
//! quality mode and take its scaled branch at that mode's scale. The title
//! computes the render size there itself (scale times output, truncated).
//!
//! The stock bytes of each patched run are known only by their BLAKE3 hash:
//! every run is read from the image and checked first, so another build
//! changes nothing and is an error.
//!
//! Ported from trustno1-coder/VIctory-NVIDIA (`apps/victory/src/native_1440p.rs`,
//! `prepare_upscaler`, GPL-3.0), itself from DexNom/VIctory
//! (`prepare_fsr_4k`, GPL-3.0-or-later); here the patches go through the
//! per-build patch list.

use crate::output::TitlePatch;

/// Where a build picks its renderer and sizes the FidelityFX path, as RVAs,
/// with the BLAKE3 (hex) of the stock bytes of each run.
#[derive(Debug, Clone, Copy)]
pub struct UpscalerSites {
    /// `mov eax, 6; cmovne eax, ecx` (8 bytes): renderer 6, or 7 when the
    /// console check passed.
    pub renderer_selection: u32,
    pub renderer_selection_stock: &'static str,
    /// The console check's result noted as the FidelityFX quality mode and
    /// tested for the scaled branch (14 bytes, ending in a short `jne` to
    /// [`UpscalerSites::scaled_load`]).
    pub sizing: u32,
    pub sizing_stock: &'static str,
    /// The native branch: a RIP-relative load of 1.0 and a short jump past
    /// the scaled branch (10 bytes; the load's 4-byte displacement is at +4).
    pub native_load: u32,
    pub native_load_stock: &'static str,
    /// The scaled branch: a RIP-relative load of the scale (8 bytes, its
    /// displacement at +4).
    pub scaled_load: u32,
    pub scaled_load_stock: &'static str,
    /// The scale the scaled branch loads, the stock 2/3 (f32; shared with
    /// other code, so never written).
    pub scale: u32,
}

/// The stock FidelityFX path scale, 2/3 (quality).
const STOCK_SCALE: f32 = 2.0 / 3.0;
const RENDERER_SELECTION_LEN: usize = 8;
const SIZING_LEN: usize = 14;
const NATIVE_LOAD_LEN: usize = 10;
const SCALED_LOAD_LEN: usize = 8;
/// `jne rel8` at the end of the sizing run.
const SIZING_JUMP: u8 = 0x0a;

/// The patches that make the title render through its FidelityFX path at
/// `scale` of its output (0 < scale <= 1), noting FidelityFX quality mode
/// `ffx_mode` (0 native AA to 4 ultra performance). `read(rva, len)` gives
/// the image's bytes there (`None` if not file-backed). Each run is checked
/// against its stock hash first; an error names the first that differs.
/// The temporal scene scale is not among them: the caller sets it to the
/// same `scale` ([`crate::output::scene_scale_patch`]).
pub fn upscaler_patches(
    sites: &UpscalerSites,
    ffx_mode: u8,
    scale: f32,
    read: impl Fn(u32, usize) -> Option<Vec<u8>>,
) -> Result<Vec<TitlePatch>, String> {
    if !(scale > 0.0 && scale <= 1.0) || ffx_mode > 4 {
        return Err(format!(
            "upscaler path: scale {scale} or FidelityFX mode {ffx_mode} out of range"
        ));
    }
    if sites.sizing + SIZING_LEN as u32 + u32::from(SIZING_JUMP) != sites.scaled_load
        || sites.native_load != sites.sizing + SIZING_LEN as u32
        || sites.scaled_load != sites.native_load + NATIVE_LOAD_LEN as u32
    {
        return Err("upscaler path: the sizing, native and scaled runs are not in a row".into());
    }
    let stock = |what: &str, rva: u32, len: usize, hash: &str| -> Result<Vec<u8>, String> {
        let bytes = read(rva, len)
            .filter(|b| b.len() == len)
            .ok_or_else(|| format!("upscaler path: the {what} at +{rva:#x} is not in the image"))?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(format!(
                "upscaler path: unexpected {what} at +{rva:#x} (another build?)"
            ));
        }
        Ok(bytes)
    };
    let selection = stock(
        "renderer selection",
        sites.renderer_selection,
        RENDERER_SELECTION_LEN,
        sites.renderer_selection_stock,
    )?;
    let sizing = stock(
        "render size selection",
        sites.sizing,
        SIZING_LEN,
        sites.sizing_stock,
    )?;
    let native = stock(
        "native scale load",
        sites.native_load,
        NATIVE_LOAD_LEN,
        sites.native_load_stock,
    )?;
    let scaled = stock(
        "scaled load",
        sites.scaled_load,
        SCALED_LOAD_LEN,
        sites.scaled_load_stock,
    )?;
    if read(sites.scale, 4).as_deref() != Some(&STOCK_SCALE.to_le_bytes()[..]) {
        return Err(format!(
            "upscaler path: unexpected scale at +{:#x}",
            sites.scale
        ));
    }
    let mut patches = vec![
        // Renderer 7 every time it is asked: `mov eax, 7` and three `nop`s.
        TitlePatch {
            name: "upscaler path: renderer selection",
            rva: sites.renderer_selection,
            expect: selection,
            write: vec![0xb8, 7, 0, 0, 0, 0x90, 0x90, 0x90],
        },
        // `mov eax, mode; mov [r14+0x90], eax; jmp scaled`.
        TitlePatch {
            name: "upscaler path: render size selection",
            rva: sites.sizing,
            expect: sizing,
            write: vec![
                0xb8,
                ffx_mode,
                0,
                0,
                0,
                0x41,
                0x89,
                0x86,
                0x90,
                0,
                0,
                0,
                0xeb,
                SIZING_JUMP,
            ],
        },
    ];
    if scale != STOCK_SCALE {
        // The jump leaves the native branch unreached: its load's operand
        // becomes a literal of ours, which the scaled branch loads.
        let literal = sites.native_load + 4;
        let displacement = literal.wrapping_sub(sites.scaled_load + SCALED_LOAD_LEN as u32) as i32;
        patches.push(TitlePatch {
            name: "upscaler path: the scale",
            rva: literal,
            expect: native[4..8].to_vec(),
            write: scale.to_le_bytes().to_vec(),
        });
        patches.push(TitlePatch {
            name: "upscaler path: the scaled load",
            rva: sites.scaled_load + 4,
            expect: scaled[4..8].to_vec(),
            write: displacement.to_le_bytes().to_vec(),
        });
    }
    Ok(patches)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic image: our own bytes at made-up RVAs, the sites hashed
    /// from them.
    struct Image {
        base: u32,
        bytes: Vec<u8>,
    }

    impl Image {
        fn read(&self, rva: u32, len: usize) -> Option<Vec<u8>> {
            let at = rva.checked_sub(self.base)? as usize;
            self.bytes.get(at..at + len).map(<[u8]>::to_vec)
        }

        fn write(&mut self, rva: u32, bytes: &[u8]) {
            let at = (rva - self.base) as usize;
            self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        }
    }

    const BASE: u32 = 0x1000;
    const SIZING: u32 = 0x1100;
    const SCALE: u32 = 0x1f00;

    fn image() -> (Image, UpscalerSites) {
        let mut image = Image {
            base: BASE,
            bytes: (0..0x1000u32).map(|i| (i * 7 + 3) as u8).collect(),
        };
        image.write(SCALE, &STOCK_SCALE.to_le_bytes());
        // The stock scaled branch loads the shared scale.
        let scaled_load = SIZING + 24;
        let displacement = SCALE as i32 - (scaled_load + 8) as i32;
        image.write(scaled_load + 4, &displacement.to_le_bytes());
        let hash = |rva: u32, len: usize| -> &'static str {
            let hex = blake3::hash(&image.read(rva, len).unwrap()).to_hex();
            Box::leak(hex.as_str().to_owned().into_boxed_str())
        };
        let sites = UpscalerSites {
            renderer_selection: 0x1040,
            renderer_selection_stock: hash(0x1040, 8),
            sizing: SIZING,
            sizing_stock: hash(SIZING, 14),
            native_load: SIZING + 14,
            native_load_stock: hash(SIZING + 14, 10),
            scaled_load: SIZING + 24,
            scaled_load_stock: hash(SIZING + 24, 8),
            scale: SCALE,
        };
        (image, sites)
    }

    /// Applies `patches` as the loader does: every expected run first.
    fn apply(image: &mut Image, patches: &[TitlePatch]) {
        for p in patches {
            assert_eq!(
                image.read(p.rva, p.expect.len()).unwrap(),
                p.expect,
                "{}",
                p.name
            );
            assert_eq!(p.expect.len(), p.write.len(), "{}", p.name);
        }
        for p in patches {
            image.write(p.rva, &p.write);
        }
    }

    /// Quality keeps the stock scale load; every other mode makes the scaled
    /// branch load its own scale from the unreached native branch.
    #[test]
    fn each_mode_loads_its_scale() {
        for (mode, scale) in [
            (0u8, 1.0f32),
            (1, 2.0 / 3.0),
            (2, 0.58),
            (3, 0.5),
            (4, 1.0 / 3.0),
        ] {
            let (mut image, sites) = image();
            let patches = upscaler_patches(&sites, mode, scale, |r, l| image.read(r, l)).unwrap();
            assert_eq!(patches.len(), if mode == 1 { 2 } else { 4 }, "{mode}");
            apply(&mut image, &patches);
            let selection = image.read(sites.renderer_selection, 8).unwrap();
            assert_eq!(selection, [0xb8, 7, 0, 0, 0, 0x90, 0x90, 0x90]);
            let sizing = image.read(sites.sizing, 14).unwrap();
            assert_eq!(sizing[1], mode);
            assert_eq!(sizing[12..], [0xeb, SIZING_JUMP]);
            // The jump lands on the scaled load.
            assert_eq!(sites.sizing + 14 + u32::from(sizing[13]), sites.scaled_load);
            let load = image.read(sites.scaled_load, 8).unwrap();
            let displacement = i32::from_le_bytes(load[4..].try_into().unwrap());
            let loaded = (sites.scaled_load + 8).wrapping_add_signed(displacement);
            let value = f32::from_le_bytes(image.read(loaded, 4).unwrap().try_into().unwrap());
            assert_eq!(value, scale, "{mode}");
            // The shared constant stays as it was.
            assert_eq!(image.read(SCALE, 4).unwrap(), STOCK_SCALE.to_le_bytes());
            // Patched once, the image is no longer the stock one: refused.
            assert!(upscaler_patches(&sites, mode, scale, |r, l| image.read(r, l)).is_err());
        }
    }

    /// Another build (a differing byte in any run, or a moved scale) and
    /// impossible arguments are errors.
    #[test]
    fn another_build_is_refused() {
        let (image, sites) = image();
        let at = [
            sites.renderer_selection,
            sites.sizing + 13,
            sites.native_load + 9,
            sites.scaled_load + 2,
            SCALE,
        ];
        for rva in at {
            let mut other = Image {
                base: image.base,
                bytes: image.bytes.clone(),
            };
            let byte = other.read(rva, 1).unwrap()[0];
            other.write(rva, &[!byte]);
            assert!(
                upscaler_patches(&sites, 1, STOCK_SCALE, |r, l| other.read(r, l)).is_err(),
                "{rva:#x}"
            );
        }
        let read = |r, l| image.read(r, l);
        assert!(upscaler_patches(&sites, 5, 0.5, read).is_err());
        assert!(upscaler_patches(&sites, 1, 0.0, read).is_err());
        assert!(upscaler_patches(&sites, 1, 1.5, read).is_err());
        let moved = UpscalerSites {
            scaled_load: sites.scaled_load + 1,
            ..sites
        };
        assert!(upscaler_patches(&moved, 1, STOCK_SCALE, read).is_err());
        assert!(upscaler_patches(&sites, 1, STOCK_SCALE, |_, _| None).is_err());
    }
}
