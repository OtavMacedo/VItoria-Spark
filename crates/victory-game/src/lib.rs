//! Everything specific to the title: its container formats, build
//! identities, and the hooks we place in its code (located by per-build
//! offsets or byte-pattern signatures). What may and may not be stored here
//! is defined in docs/asset-policy.md.

pub mod output;
pub mod sgd5;
pub mod upscaler;

/// A specific build of the title executable we know how to run.
#[derive(Debug, Clone, Copy)]
pub struct KnownBuild {
    /// Our own label, e.g. "1.0.0 retail".
    pub label: &'static str,
    /// BLAKE3 of the original executable, hex.
    pub blake3: &'static str,
    /// Changes to its code made before it is mapped.
    pub patches: &'static [CodePatch],
    /// Where it fixes its Series X output size, if known
    /// ([`output::output_patches`]).
    pub output: Option<output::OutputSites>,
    /// Where it picks its renderer and sizes its FidelityFX path, if known
    /// ([`upscaler::upscaler_patches`], for DLSS).
    pub upscaler: Option<upscaler::UpscalerSites>,
}

impl KnownBuild {
    /// The code patches that apply: all of them, or, with the FidelityFX
    /// path patched in for DLSS (`upscaler_path`), all but those that keep
    /// the title on its own upscaler ([`CodePatch::upscaler_choice`]): they
    /// return before the renderer selection the path patches is reached.
    pub fn code_patches(&self, upscaler_path: bool) -> impl Iterator<Item = &CodePatch> {
        self.patches
            .iter()
            .filter(move |p| !(upscaler_path && p.upscaler_choice))
    }
}

/// A change to title code: at `rva`, the bytes `expect` (a short signature
/// checked before anything is written) become `write`, our own code.
#[derive(Debug, Clone, Copy)]
pub struct CodePatch {
    /// Our name for it, as logged.
    pub name: &'static str,
    pub rva: u32,
    pub expect: &'static [u8],
    pub write: &'static [u8],
    /// It makes the title keep its own upscaler: left out when the
    /// FidelityFX path is patched in for DLSS ([`KnownBuild::code_patches`]).
    pub upscaler_choice: bool,
}

/// `mov eax, 6; ret`: the upscaler a Series X uses.
const UPSCALER_6: &[u8] = &[0xb8, 0x06, 0x00, 0x00, 0x00, 0xc3];

/// Builds we support.
pub const KNOWN_BUILDS: &[KnownBuild] = &[KnownBuild {
    label: "Xbox Series master",
    blake3: "b99f876b74d4a77f03af5f878223b27bd647b5c35a79d5decd9f716504239e93",
    // The title picks its upscaler by console: on a Series S with the
    // FidelityFX upscaler's capability, FSR 4 (type 7), else its own
    // (type 6, as on a Series X). FSR 4's output never reaches the screen
    // here (every world frame is black with `--console series-s`), and
    // without its DLL the title divides by zero. Both functions that make
    // the choice return 6, so a Series S runs its own render profile with
    // the upscaler that works.
    patches: &[
        CodePatch {
            name: "upscaler choice",
            rva: 0x7e_93f0,
            expect: &[0x48, 0x83, 0xec, 0x28, 0x80, 0x3d],
            write: UPSCALER_6,
            upscaler_choice: true,
        },
        CodePatch {
            name: "upscaler choice (the per-frame copy)",
            rva: 0x540_0190,
            expect: &[0x48, 0x83, 0xec, 0x28, 0x48, 0x8b],
            write: UPSCALER_6,
            upscaler_choice: true,
        },
    ],
    output: Some(output::OutputSites {
        display_mode: 0x615_c100,
        scanout_reservation: 0x247_ad4a,
        scene_scale: 0x28f_1e7e,
        ui_size_load: 0x580_48d4,
        ui_size_load_stock: [0xc5, 0xfb, 0x10, 0x0d, 0x10, 0xf0, 0x13, 0x05],
    }),
    // The renderer selection lies inside the first upscaler choice above,
    // which returns before it: the two are never applied together.
    upscaler: Some(upscaler::UpscalerSites {
        renderer_selection: 0x7e_9421,
        renderer_selection_stock: "f0633abf37df17124108b1e37be64645bd4b2412b0524c2f89334f22bd948464",
        sizing: 0x28e_f69a,
        sizing_stock: "2ae2ac102d12ca003cb218413ef018e4dba68fc2003654a30c1743e13a820ffd",
        native_load: 0x28e_f6a8,
        native_load_stock: "2579c24601d173b086a8136edf54ffd82720e6bf186f9139274bb09a45ddadf5",
        scaled_load: 0x28e_f6b2,
        scaled_load_stock: "e218882c95d2e16163a409d55b3451519414eb87c4d6220785dc09852d4e14f4",
        scale: 0x615_d95c,
    }),
}];

/// Looks up a build by the BLAKE3 hash of its executable.
pub fn identify(blake3_hex: &str) -> Option<&'static KnownBuild> {
    KNOWN_BUILDS
        .iter()
        .find(|b| b.blake3.eq_ignore_ascii_case(blake3_hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_builds_are_well_formed() {
        for build in KNOWN_BUILDS {
            assert_eq!(build.blake3.len(), 64);
            assert!(build.blake3.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(
                identify(&build.blake3.to_uppercase()).map(|b| b.label),
                Some(build.label)
            );
            for patch in build.patches {
                assert!(!patch.write.is_empty(), "{}", patch.name);
                assert_eq!(patch.expect.len(), patch.write.len(), "{}", patch.name);
            }
            // The output sites overlap neither each other nor a code patch:
            // all are applied together and checked before any is written.
            if let Some(sites) = &build.output {
                let size = output::OutputSize::parse("1080p").unwrap();
                let mut runs: Vec<(u32, usize)> = output::output_patches(sites, size, 1.0, 0)
                    .iter()
                    .map(|p| (p.rva, p.write.len()))
                    .chain(build.patches.iter().map(|p| (p.rva, p.write.len())))
                    .collect();
                runs.sort();
                for pair in runs.windows(2) {
                    assert!(pair[0].0 + pair[0].1 as u32 <= pair[1].0, "{pair:?}");
                }
            }
        }
        assert!(identify("00").is_none());
    }

    /// With the FidelityFX path for DLSS, the upscaler choices are left
    /// out and every other code patch stays; without it, all apply. The
    /// path's runs overlap no output site and no code patch that stays.
    #[test]
    fn the_upscaler_path_and_the_upscaler_choice_exclude_each_other() {
        for build in KNOWN_BUILDS {
            let all: Vec<&str> = build.code_patches(false).map(|p| p.name).collect();
            assert_eq!(all.len(), build.patches.len());
            let with_path: Vec<&CodePatch> = build.code_patches(true).collect();
            assert!(with_path.iter().all(|p| !p.upscaler_choice));
            assert_eq!(
                with_path.len(),
                build.patches.iter().filter(|p| !p.upscaler_choice).count()
            );
            let Some(sites) = &build.upscaler else {
                continue;
            };
            for hash in [
                sites.renderer_selection_stock,
                sites.sizing_stock,
                sites.native_load_stock,
                sites.scaled_load_stock,
            ] {
                assert_eq!(hash.len(), 64);
                assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
            }
            // The path's selection is in a function an upscaler choice
            // replaces: applied together, the path would never be reached.
            assert!(build.patches.iter().any(|p| p.upscaler_choice
                && p.rva < sites.renderer_selection
                && sites.renderer_selection - p.rva < 0x100));
            let mut runs: Vec<(u32, usize)> = vec![
                (sites.renderer_selection, 8),
                (sites.sizing, 14),
                (sites.native_load + 4, 4),
                (sites.scaled_load + 4, 4),
            ];
            runs.extend(with_path.iter().map(|p| (p.rva, p.write.len())));
            if let Some(output) = &build.output {
                let size = output::OutputSize::parse("1440p").unwrap();
                runs.extend(
                    output::output_patches(output, size, 2.0 / 3.0, 0)
                        .iter()
                        .map(|p| (p.rva, p.write.len())),
                );
            }
            runs.sort();
            for pair in runs.windows(2) {
                assert!(pair[0].0 + pair[0].1 as u32 <= pair[1].0, "{pair:?}");
            }
        }
    }
}
