//! A Series X output smaller than the stock 3840x2160 (`VICTORY_OUTPUT_RESOLUTION`).
//!
//! The title presents at 3840x2160 on a Series X, its scene drawn at 2/3 of
//! that and scaled up by its own temporal pass. Four places of a known build
//! fix that size ([`OutputSites`]); rewriting them before the image is mapped
//! makes the title create a smaller output itself, so no 4K pass runs. Every
//! site is checked against its stock bytes first: another build changes
//! nothing and is an error.

/// The stock Series X SDR display mode: 3840x2160 at 60 Hz, then a flag of 1
/// (u32 each, little-endian).
const STOCK_DISPLAY_MODE: [u8; 16] = [0, 15, 0, 0, 112, 8, 0, 0, 60, 0, 0, 0, 1, 0, 0, 0];
/// `mov ecx, 0x4000000`: 64 MiB reserved for the scanout buffers.
const STOCK_SCANOUT_RESERVATION: [u8; 5] = [0xb9, 0, 0, 0, 4];
/// The stock scene scale, 2/3 (a 2560x1440 scene in a 3840x2160 output).
const STOCK_SCENE_SCALE: f32 = 2.0 / 3.0;
/// The size the title's frontend lays its screens out for.
pub const LEGACY_CANVAS: OutputSize = OutputSize {
    width: 3840,
    height: 2160,
};

/// Where a build fixes the size of its Series X output, as RVAs.
#[derive(Debug, Clone, Copy)]
pub struct OutputSites {
    /// The SDR display mode the title picks (16 bytes: width, height, refresh
    /// rate, flag; only the first two change).
    pub display_mode: u32,
    /// `mov ecx, imm32`: the bytes reserved for the two scanout buffers. The
    /// title checks its allocation against it, so it must match the size.
    pub scanout_reservation: u32,
    /// The scene's size as a fraction of the output (f32), which the title's
    /// temporal pass upscales.
    pub scene_scale: u32,
    /// A settings-screen viewport setter's 8-byte RIP-relative load
    /// (`vmovsd xmm1, [rip+disp32]`) of the legacy 3840x2160 UI size. It
    /// lays its preview panels out with that size whatever surface was
    /// made; it is pointed at the display mode's width and height instead.
    pub ui_size_load: u32,
    /// The stock bytes of that load (its displacement is per build).
    pub ui_size_load_stock: [u8; 8],
}

/// An output size, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSize {
    pub width: u32,
    pub height: u32,
}

impl OutputSize {
    /// Parses `1080p`, `1440p` or `<width>x<height>`. The size must be 16:9
    /// (the frontend lays out for 16:9), even, and from 1280x720 up to the
    /// stock 3840x2160 (the scanout reservation is never grown).
    pub fn parse(text: &str) -> Result<OutputSize, String> {
        let text = text.trim();
        let size = match text.to_ascii_lowercase().as_str() {
            "1080p" => OutputSize {
                width: 1920,
                height: 1080,
            },
            "1440p" => OutputSize {
                width: 2560,
                height: 1440,
            },
            other => {
                let (w, h) = other
                    .split_once('x')
                    .ok_or_else(|| format!("output size is not 1080p, 1440p or WxH: {text}"))?;
                let number = |s: &str| {
                    s.trim()
                        .parse::<u32>()
                        .map_err(|_| format!("output size is not 1080p, 1440p or WxH: {text}"))
                };
                OutputSize {
                    width: number(w)?,
                    height: number(h)?,
                }
            }
        };
        let OutputSize { width, height } = size;
        if u64::from(width) * 9 != u64::from(height) * 16 {
            return Err(format!("output size {width}x{height} is not 16:9"));
        }
        if !(1280..=LEGACY_CANVAS.width).contains(&width) || width % 2 != 0 || height % 2 != 0 {
            return Err(format!(
                "output size {width}x{height} is not even and within 1280x720..3840x2160"
            ));
        }
        Ok(size)
    }

    /// The scene's size at `scale` of this output, truncated as the title
    /// truncates it.
    pub fn scene(self, scale: f32) -> OutputSize {
        OutputSize {
            width: (self.width as f32 * scale) as u32,
            height: (self.height as f32 * scale) as u32,
        }
    }
}

/// Parses a scene scale: a decimal (`0.75`) or a fraction (`3/4`), from 0.5
/// to 1 (the scene's size over the output's).
pub fn parse_scene_scale(text: &str) -> Result<f32, String> {
    let text = text.trim();
    let bad = || format!("scene scale is not a number or a fraction from 0.5 to 1: {text}");
    let scale = match text.split_once('/') {
        Some((n, d)) => {
            let n = n.trim().parse::<f32>().map_err(|_| bad())?;
            let d = d.trim().parse::<f32>().map_err(|_| bad())?;
            n / d
        }
        None => text.parse::<f32>().map_err(|_| bad())?,
    };
    if !(0.5..=1.0).contains(&scale) {
        return Err(bad());
    }
    Ok(scale)
}

/// A change to title bytes made at run time: at `rva`, `expect` (checked
/// first) becomes `write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitlePatch {
    /// Our name for it, as logged.
    pub name: &'static str,
    pub rva: u32,
    pub expect: Vec<u8>,
    pub write: Vec<u8>,
}

/// The patch that sets the scene's size to `scale` of the output (the
/// title's temporal pass, and its FidelityFX path's partner scale), from the
/// stock 2/3.
pub fn scene_scale_patch(sites: &OutputSites, scale: f32) -> TitlePatch {
    TitlePatch {
        name: "output: scene scale",
        rva: sites.scene_scale,
        expect: STOCK_SCENE_SCALE.to_le_bytes().to_vec(),
        write: scale.to_le_bytes().to_vec(),
    }
}

/// The patches for an output of `size`, the scene at `scene_scale` of it,
/// and `scanout_bytes` reserved for the scanout buffers (the caller knows
/// their tiled layout).
pub fn output_patches(
    sites: &OutputSites,
    size: OutputSize,
    scene_scale: f32,
    scanout_bytes: u32,
) -> Vec<TitlePatch> {
    let mut mode = STOCK_DISPLAY_MODE;
    mode[..4].copy_from_slice(&size.width.to_le_bytes());
    mode[4..8].copy_from_slice(&size.height.to_le_bytes());
    let mut reservation = STOCK_SCANOUT_RESERVATION;
    reservation[1..].copy_from_slice(&scanout_bytes.to_le_bytes());
    let mut load = sites.ui_size_load_stock;
    // The displacement is from the end of the 8-byte instruction.
    let displacement = sites
        .display_mode
        .wrapping_sub(sites.ui_size_load.wrapping_add(8)) as i32;
    load[4..].copy_from_slice(&displacement.to_le_bytes());
    vec![
        TitlePatch {
            name: "output: display mode",
            rva: sites.display_mode,
            expect: STOCK_DISPLAY_MODE.to_vec(),
            write: mode.to_vec(),
        },
        TitlePatch {
            name: "output: scanout reservation",
            rva: sites.scanout_reservation,
            expect: STOCK_SCANOUT_RESERVATION.to_vec(),
            write: reservation.to_vec(),
        },
        scene_scale_patch(sites, scene_scale),
        TitlePatch {
            name: "output: settings viewport size",
            rva: sites.ui_size_load,
            expect: sites.ui_size_load_stock.to_vec(),
            write: load.to_vec(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITES: OutputSites = OutputSites {
        display_mode: 0x10_0000,
        scanout_reservation: 0x2000,
        scene_scale: 0x3000,
        ui_size_load: 0x4000,
        ui_size_load_stock: [0xc5, 0xfb, 0x10, 0x0d, 1, 2, 3, 4],
    };

    #[test]
    fn parses_output_sizes() {
        let size = |w, h| OutputSize {
            width: w,
            height: h,
        };
        assert_eq!(OutputSize::parse("1080p"), Ok(size(1920, 1080)));
        assert_eq!(OutputSize::parse(" 1440P "), Ok(size(2560, 1440)));
        assert_eq!(OutputSize::parse("1600x900"), Ok(size(1600, 900)));
        assert_eq!(OutputSize::parse("3840X2160"), Ok(LEGACY_CANVAS));
        for bad in [
            "",
            "720",
            "1920x1200",
            "5120x2880",
            "1024x576",
            "1x",
            "axb",
            "1922x1081",
        ] {
            assert!(OutputSize::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_scene_scales() {
        assert_eq!(parse_scene_scale("1"), Ok(1.0));
        assert_eq!(parse_scene_scale("0.75"), Ok(0.75));
        assert_eq!(parse_scene_scale(" 3/4 "), Ok(0.75));
        assert_eq!(parse_scene_scale("2/3"), Ok(2.0 / 3.0));
        for bad in ["0.4", "1.5", "3/2", "x", "1/0", ""] {
            assert!(parse_scene_scale(bad).is_err(), "{bad}");
        }
    }

    /// The scene the title computes (scale times output, truncated).
    #[test]
    fn scene_sizes() {
        let out = |s| OutputSize::parse(s).unwrap();
        assert_eq!(out("1440p").scene(1.0), out("1440p"));
        assert_eq!(out("1440p").scene(0.75), out("1080p"));
        assert_eq!(out("1080p").scene(2.0 / 3.0), out("1280x720"));
        assert_eq!(LEGACY_CANVAS.scene(STOCK_SCENE_SCALE), out("1440p"));
    }

    #[test]
    fn output_patches_write_the_size() {
        let size = OutputSize::parse("1080p").unwrap();
        let patches = output_patches(&SITES, size, 0.75, 0x0120_0000);
        assert_eq!(patches.len(), 4);
        for p in &patches {
            assert_eq!(p.expect.len(), p.write.len(), "{}", p.name);
        }
        let mode = &patches[0].write;
        assert_eq!(mode[..8], [0x80, 7, 0, 0, 0x38, 4, 0, 0]);
        assert_eq!(mode[8..], STOCK_DISPLAY_MODE[8..]);
        assert_eq!(patches[1].write, [0xb9, 0, 0, 0x20, 1]);
        assert_eq!(patches[2].expect, STOCK_SCENE_SCALE.to_le_bytes());
        assert_eq!(patches[2].write, 0.75f32.to_le_bytes());
        let load = &patches[3].write;
        assert_eq!(load[..4], SITES.ui_size_load_stock[..4]);
        let displacement = i32::from_le_bytes(load[4..].try_into().unwrap());
        assert_eq!(
            (SITES.ui_size_load + 8).wrapping_add_signed(displacement),
            SITES.display_mode
        );
    }

    /// A load that lies after the display mode gets a negative displacement.
    #[test]
    fn the_ui_load_reaches_back() {
        let sites = OutputSites {
            display_mode: 0x1000,
            ui_size_load: 0x9000,
            ..SITES
        };
        let patches = output_patches(&sites, LEGACY_CANVAS, 1.0, 0x0400_0000);
        let displacement = i32::from_le_bytes(patches[3].write[4..].try_into().unwrap());
        assert_eq!(displacement, 0x1000 - 0x9008);
    }
}
