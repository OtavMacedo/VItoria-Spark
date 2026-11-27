//! What does not need the library: the switches, which DLSS mode a title's
//! render and output sizes call for, the FidelityFX states and flags as the
//! host and Streamline read them, and the camera constants built from what an
//! upscaling names.
//!
//! Ported from trustno1-coder/VIctory-NVIDIA
//! (`crates/victory-gpu/src/streamline/plan.rs`, GPL-3.0); the FidelityFX
//! state mapping there is from DexNom/VIctory (GPL-3.0-or-later).

use std::path::PathBuf;

use super::abi;

/// `VICTORY_DLSS_MODE`: the render scale asked for (the title's renderer is
/// patched to it before it is mapped), named as DLSS names its modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dlaa,
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}

impl Mode {
    /// Every mode, largest scene first.
    pub const ALL: [Mode; 5] = [
        Mode::Dlaa,
        Mode::Quality,
        Mode::Balanced,
        Mode::Performance,
        Mode::UltraPerformance,
    ];

    /// The mode named `name` (case and surrounding spaces ignored).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "dlaa" | "native" => Mode::Dlaa,
            "quality" => Mode::Quality,
            "balanced" => Mode::Balanced,
            "performance" => Mode::Performance,
            "ultraperformance" | "ultra-performance" | "ultra_performance" => {
                Mode::UltraPerformance
            }
            _ => return None,
        })
    }

    /// The name `from_name` reads.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Dlaa => "dlaa",
            Mode::Quality => "quality",
            Mode::Balanced => "balanced",
            Mode::Performance => "performance",
            Mode::UltraPerformance => "ultraperformance",
        }
    }

    /// The render size over the output size (DLSS's ratios).
    pub fn scale(self) -> f32 {
        match self {
            Mode::Dlaa => 1.0,
            Mode::Quality => 2.0 / 3.0,
            Mode::Balanced => 0.58,
            Mode::Performance => 0.5,
            Mode::UltraPerformance => 1.0 / 3.0,
        }
    }

    /// The FidelityFX quality mode of the same ratio (native AA, quality,
    /// balanced, performance, ultra performance), which the title notes.
    pub fn ffx_mode(self) -> u8 {
        match self {
            Mode::Dlaa => 0,
            Mode::Quality => 1,
            Mode::Balanced => 2,
            Mode::Performance => 3,
            Mode::UltraPerformance => 4,
        }
    }

    /// The `sl::DLSSMode` of the same ratio.
    pub fn sl_mode(self) -> u32 {
        match self {
            Mode::Dlaa => abi::DLSS_DLAA,
            Mode::Quality => abi::DLSS_MAX_QUALITY,
            Mode::Balanced => abi::DLSS_BALANCED,
            Mode::Performance => abi::DLSS_MAX_PERFORMANCE,
            Mode::UltraPerformance => abi::DLSS_ULTRA_PERFORMANCE,
        }
    }

    /// The render size (pixels) the title makes of `output` at this scale
    /// (it truncates, as the title's own sizing code does).
    pub fn render_size(self, output: [u32; 2]) -> [u32; 2] {
        output.map(|v| (v as f32 * self.scale()) as u32)
    }
}

/// `VICTORY_DLSS_PRESET`: the DLSS model for every mode (`sl::DLSSPreset`).
pub fn preset(name: &str) -> Option<u32> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "" | "default" => 0,
        "e" => 5,
        "f" => 6,
        "j" => 10,
        "k" => 11,
        "l" => 12,
        "m" => 13,
        _ => return None,
    })
}

/// The name of a `sl::DLSSMode`, for the log.
pub fn sl_mode_name(mode: u32) -> &'static str {
    match mode {
        abi::DLSS_OFF => "off",
        abi::DLSS_MAX_PERFORMANCE => "performance",
        abi::DLSS_BALANCED => "balanced",
        abi::DLSS_MAX_QUALITY => "quality",
        abi::DLSS_ULTRA_PERFORMANCE => "ultra performance",
        abi::DLSS_ULTRA_QUALITY => "ultra quality",
        abi::DLSS_DLAA => "DLAA",
        _ => "?",
    }
}

/// The DLSS modes tried for a render size, largest render size first.
pub const MODES: [u32; 6] = [
    abi::DLSS_DLAA,
    abi::DLSS_ULTRA_QUALITY,
    abi::DLSS_MAX_QUALITY,
    abi::DLSS_BALANCED,
    abi::DLSS_MAX_PERFORMANCE,
    abi::DLSS_ULTRA_PERFORMANCE,
];

/// The DLSS mode for a title that renders `render` for an output of
/// `output` (pixels): of the modes whose optimal render size (`optimal`,
/// from the library) holds `render` (DLSS makes its feature for that size
/// and takes smaller ones as a sub-rectangle), the one with the smallest.
/// `None` if no mode does, or the sizes are empty or inverted.
pub fn pick_mode(
    render: [u32; 2],
    output: [u32; 2],
    optimal: impl Fn(u32) -> Option<[u32; 2]>,
) -> Option<u32> {
    if render[0] == 0 || render[1] == 0 || render[0] > output[0] || render[1] > output[1] {
        return None;
    }
    MODES
        .iter()
        .filter_map(|&mode| optimal(mode).map(|size| (mode, size)))
        .filter(|&(_, size)| {
            size[0] >= render[0]
                && size[1] >= render[1]
                && size[0] <= output[0]
                && size[1] <= output[1]
        })
        .min_by_key(|&(_, size)| u64::from(size[0]) * u64::from(size[1]))
        .map(|(mode, _)| mode)
}

// FidelityFX API upscale flags (`FFX_UPSCALE_ENABLE_*`).
pub const FFX_HIGH_DYNAMIC_RANGE: u32 = 1 << 0;
pub const FFX_DISPLAY_RESOLUTION_MOTION_VECTORS: u32 = 1 << 1;
pub const FFX_MOTION_VECTORS_JITTER_CANCELLATION: u32 = 1 << 2;
pub const FFX_DEPTH_INVERTED: u32 = 1 << 3;
pub const FFX_AUTO_EXPOSURE: u32 = 1 << 5;

/// A FidelityFX resource state as the host resource state it means (as the
/// PC backend reads it); `None` for generic read and indirect arguments,
/// which a texture DLSS reads cannot be in.
pub fn host_state(state: u32) -> Option<u32> {
    Some(match state {
        // Common, present.
        0x1 | 0x80 => 0,
        // Unordered access.
        0x2 => 0x8,
        // Compute read, pixel read, both.
        0x4 => 0x40,
        0x8 => 0x80,
        0xc => 0xc0,
        // Copy source, copy destination.
        0x10 => 0x800,
        0x20 => 0x400,
        // Render target, depth write.
        0x100 => 0x4,
        0x200 => 0x10,
        _ => return None,
    })
}

/// The host state DLSS is told an input is in: the one the title gave, or,
/// where that cannot be a texture's that a shader reads, shader resource.
pub fn input_state(ffx_state: u32) -> u32 {
    host_state(ffx_state)
        .filter(|&s| s != 0x800 && s != 0x400 && s != 0)
        .unwrap_or(0xc0)
}

/// The host state DLSS is told the output is in: unordered access, whatever
/// the title gave (DLSS writes it from a compute shader).
pub fn output_state(ffx_state: u32) -> u32 {
    host_state(ffx_state).filter(|&s| s == 0x8).unwrap_or(0x8)
}

/// What an upscaling says of the camera, and of how to read its inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Sub-pixel jitter, render pixels.
    pub jitter: [f32; 2],
    /// Multiplies the motion vectors into render pixels (FidelityFX).
    pub motion_scale: [f32; 2],
    /// The scene's size, pixels.
    pub render: [u32; 2],
    /// The upscaled size, pixels.
    pub output: [u32; 2],
    pub near: f32,
    pub far: f32,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// The history is not to be used (a cut).
    pub reset: bool,
    /// The context's creation flags (`FFX_*` above).
    pub flags: u32,
}

impl Camera {
    /// Depth 1 is near.
    pub fn depth_inverted(&self) -> bool {
        self.flags & FFX_DEPTH_INVERTED != 0
    }

    /// The motion vectors cover the output, not the render.
    pub fn display_motion_vectors(&self) -> bool {
        self.flags & FFX_DISPLAY_RESOLUTION_MOTION_VECTORS != 0
    }

    /// The size (pixels) the motion vectors cover.
    pub fn motion_size(&self) -> [u32; 2] {
        if self.display_motion_vectors() {
            self.output
        } else {
            self.render
        }
    }

    /// Streamline's motion vector scale: it multiplies by the render size
    /// itself before handing the scale to DLSS, which then gets FidelityFX's
    /// scale back.
    pub fn mvec_scale(&self, multiplier: [f32; 2]) -> [f32; 2] {
        let size = self.motion_size();
        [0, 1].map(|i| self.motion_scale[i] * multiplier[i] / size[i].max(1) as f32)
    }

    /// The common constants of a frame. The title names no camera matrices:
    /// the projection is made of the field of view, the aspect and the depth
    /// range it gives; camera motion is in the motion vectors, so the
    /// clip-to-previous-clip matrices stay the identity and the camera stands
    /// at the origin looking down +z. DLSS Super Resolution reads none of the
    /// matrices.
    pub fn constants(
        &self,
        jitter_multiplier: [f32; 2],
        mvec_multiplier: [f32; 2],
    ) -> abi::Constants {
        let aspect = self.render[0].max(1) as f32 / self.render[1].max(1) as f32;
        let view_to_clip = projection(
            self.near,
            self.far,
            self.fov_y,
            aspect,
            self.depth_inverted(),
        );
        let clip_to_view = invert(&view_to_clip).unwrap_or(IDENTITY);
        abi::Constants {
            base: abi::BaseStructure::new(abi::CONSTANTS, 2),
            camera_view_to_clip: view_to_clip,
            clip_to_camera_view: clip_to_view,
            clip_to_lens_clip: IDENTITY,
            clip_to_prev_clip: IDENTITY,
            prev_clip_to_clip: IDENTITY,
            jitter_offset: [
                self.jitter[0] * jitter_multiplier[0],
                self.jitter[1] * jitter_multiplier[1],
            ],
            mvec_scale: self.mvec_scale(mvec_multiplier),
            camera_pinhole_offset: [0.0, 0.0],
            camera_pos: [0.0, 0.0, 0.0],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_fwd: [0.0, 0.0, 1.0],
            camera_near: self.near,
            camera_far: self.far,
            camera_fov: self.fov_y,
            camera_aspect_ratio: aspect,
            motion_vectors_invalid_value: 0.0,
            depth_inverted: abi::boolean(self.depth_inverted()),
            camera_motion_included: abi::TRUE,
            motion_vectors_3d: abi::FALSE,
            reset: abi::boolean(self.reset),
            orthographic_projection: abi::FALSE,
            motion_vectors_dilated: abi::FALSE,
            motion_vectors_jittered: abi::boolean(
                self.flags & FFX_MOTION_VECTORS_JITTER_CANCELLATION != 0,
            ),
            min_relative_linear_depth_object_separation: 40.0,
        }
    }
}

/// The 4x4 identity.
pub const IDENTITY: abi::Float4x4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// A left-handed perspective projection in Streamline's layout (row vectors,
/// rows as stored): depth 0 at `near` and 1 at `far`, or reversed; a far
/// plane past 1e30 (or not finite) is at infinity. Degenerate inputs give
/// the identity.
pub fn projection(near: f32, far: f32, fov_y: f32, aspect: f32, inverted: bool) -> abi::Float4x4 {
    if !(near > 0.0 && fov_y > 0.0 && fov_y < std::f32::consts::PI && aspect > 0.0) {
        return IDENTITY;
    }
    let y = 1.0 / (fov_y * 0.5).tan();
    let x = y / aspect;
    let infinite = !far.is_finite() || far > 1e30 || far <= near;
    let (a, b) = match (infinite, inverted) {
        (false, false) => (far / (far - near), -near * far / (far - near)),
        (false, true) => (near / (near - far), far * near / (far - near)),
        (true, false) => (1.0, -near),
        (true, true) => (0.0, near),
    };
    [
        [x, 0.0, 0.0, 0.0],
        [0.0, y, 0.0, 0.0],
        [0.0, 0.0, a, 1.0],
        [0.0, 0.0, b, 0.0],
    ]
}

/// A 4x4 inverse by cofactors; `None` if singular.
pub fn invert(m: &abi::Float4x4) -> Option<abi::Float4x4> {
    let a: [f64; 16] = std::array::from_fn(|i| f64::from(m[i / 4][i % 4]));
    // The cofactor of element (r, c): the determinant of the 3x3 left
    // without row r and column c, signed.
    let cofactor = |r: usize, c: usize| {
        let rows: Vec<usize> = (0..4).filter(|&i| i != r).collect();
        let cols: Vec<usize> = (0..4).filter(|&i| i != c).collect();
        let e = |i: usize, j: usize| a[rows[i] * 4 + cols[j]];
        let det = e(0, 0) * (e(1, 1) * e(2, 2) - e(1, 2) * e(2, 1))
            - e(0, 1) * (e(1, 0) * e(2, 2) - e(1, 2) * e(2, 0))
            + e(0, 2) * (e(1, 0) * e(2, 1) - e(1, 1) * e(2, 0));
        if (r + c).is_multiple_of(2) { det } else { -det }
    };
    let det: f64 = (0..4).map(|c| a[c] * cofactor(0, c)).sum();
    if det.abs() < 1e-30 || !det.is_finite() {
        return None;
    }
    // The inverse is the transposed cofactor matrix over the determinant.
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| (cofactor(c, r) / det) as f32)
    }))
}

/// `"x,y"` as two multipliers (`VICTORY_DLSS_JITTER`, `VICTORY_DLSS_MV`):
/// what a run may have to flip; unset or malformed is `[1, 1]`.
pub fn pair(value: Option<&str>) -> [f32; 2] {
    let Some(value) = value else {
        return [1.0, 1.0];
    };
    let mut parts = value.split(',').map(|p| p.trim().parse::<f32>());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y)), None) if x.is_finite() && y.is_finite() => [x, y],
        _ => [1.0, 1.0],
    }
}

/// What the user asked for, read from the environment once
/// ([`Asked::from_env`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    /// `VICTORY_DLSS=1`: DLSS takes the title's FidelityFX upscalings.
    pub dlss: bool,
    /// `VICTORY_DLSS_MODE` (default quality).
    pub mode: Mode,
    /// `VICTORY_DLSS_PRESET` as an `sl::DLSSPreset` (default 0, the
    /// library's own choice).
    pub preset: u32,
    /// `VICTORY_STREAMLINE_DIR`: the folder of `sl.interposer.dll`, its
    /// plugins and `nvngx_dlss.dll`. No default: nothing is looked for
    /// anywhere the user did not name.
    pub dir: Option<PathBuf>,
    /// `VICTORY_DLSS_JITTER` and `VICTORY_DLSS_MV`: sign or scale fixes.
    pub jitter: [f32; 2],
    pub mvec: [f32; 2],
    /// `VICTORY_DLSS_REACTIVE=0`: the reactive mask is not given to DLSS.
    pub reactive: bool,
    /// `VICTORY_STREAMLINE_LOG=<folder>`: Streamline's own log there,
    /// verbose.
    pub log_dir: Option<PathBuf>,
}

impl Asked {
    /// The switches from the process environment; an unknown mode or
    /// preset name is an error.
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// The switches as `var` gives them (unset is `None`).
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        let mode = match set("VICTORY_DLSS_MODE") {
            Some(name) => Mode::from_name(&name).ok_or_else(|| {
                format!(
                    "VICTORY_DLSS_MODE={name}: not one of dlaa, quality, balanced, performance, \
                     ultraperformance"
                )
            })?,
            None => Mode::Quality,
        };
        let preset = match set("VICTORY_DLSS_PRESET") {
            Some(name) => preset(&name).ok_or_else(|| {
                format!("VICTORY_DLSS_PRESET={name}: not one of default, e, f, j, k, l, m")
            })?,
            None => 0,
        };
        let folder = |name: &str| set(name).filter(|v| v.trim() != "0").map(PathBuf::from);
        Ok(Asked {
            dlss: set("VICTORY_DLSS").is_some_and(|v| v.trim() == "1"),
            mode,
            preset,
            dir: folder("VICTORY_STREAMLINE_DIR"),
            jitter: pair(set("VICTORY_DLSS_JITTER").as_deref()),
            mvec: pair(set("VICTORY_DLSS_MV").as_deref()),
            reactive: set("VICTORY_DLSS_REACTIVE").is_none_or(|v| v.trim() != "0"),
            log_dir: folder("VICTORY_STREAMLINE_LOG"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DLSS's optimal render sizes for a 2560x1440 output, as the 310 SDK
    /// reports them (ultra quality has none).
    fn optimal_1440p(mode: u32) -> Option<[u32; 2]> {
        Some(match mode {
            abi::DLSS_DLAA => [2560, 1440],
            abi::DLSS_MAX_QUALITY => [1707, 960],
            abi::DLSS_BALANCED => [1485, 835],
            abi::DLSS_MAX_PERFORMANCE => [1280, 720],
            abi::DLSS_ULTRA_PERFORMANCE => [853, 480],
            _ => return None,
        })
    }

    #[test]
    fn the_title_render_sizes_pick_their_mode() {
        let out = [2560, 1440];
        for (mode, sl) in [
            (Mode::Dlaa, abi::DLSS_DLAA),
            (Mode::Quality, abi::DLSS_MAX_QUALITY),
            (Mode::Balanced, abi::DLSS_BALANCED),
            (Mode::Performance, abi::DLSS_MAX_PERFORMANCE),
            (Mode::UltraPerformance, abi::DLSS_ULTRA_PERFORMANCE),
        ] {
            let render = mode.render_size(out);
            assert_eq!(
                pick_mode(render, out, optimal_1440p),
                Some(sl),
                "{mode:?} {render:?}"
            );
            assert_eq!(mode.sl_mode(), sl);
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::Quality.render_size(out), [1706, 960]);
        assert_eq!(Mode::Balanced.render_size(out), [1484, 835]);
        // A 1080p output: its quality scene is 1280x720, DLSS's own size.
        let s = |mode: u32| optimal_1440p(mode).map(|[w, h]| [w * 3 / 4, h * 3 / 4]);
        assert_eq!(Mode::Quality.render_size([1920, 1080]), [1280, 720]);
        assert_eq!(
            pick_mode([1280, 720], [1920, 1080], s),
            Some(abi::DLSS_MAX_QUALITY)
        );
        // A size between modes takes the next larger one.
        assert_eq!(
            pick_mode([1600, 900], out, optimal_1440p),
            Some(abi::DLSS_MAX_QUALITY)
        );
        // Nothing for a render larger than the output, or empty.
        assert_eq!(pick_mode([2561, 1440], out, optimal_1440p), None);
        assert_eq!(pick_mode([0, 1440], out, optimal_1440p), None);
        assert_eq!(pick_mode([1280, 720], out, |_| None), None);
    }

    #[test]
    fn names_and_presets() {
        assert_eq!(Mode::from_name(" Quality "), Some(Mode::Quality));
        assert_eq!(
            Mode::from_name("ultra-performance"),
            Some(Mode::UltraPerformance)
        );
        assert_eq!(Mode::from_name("fast"), None);
        assert_eq!(Mode::Balanced.ffx_mode(), 2);
        for mode in Mode::ALL {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(preset("K"), Some(11));
        assert_eq!(preset("default"), Some(0));
        assert_eq!(preset("a"), None);
        assert_eq!(sl_mode_name(abi::DLSS_DLAA), "DLAA");
    }

    #[test]
    fn states_read_as_the_pc_backend_reads_them() {
        assert_eq!(host_state(0xc), Some(0xc0));
        assert_eq!(host_state(0x2), Some(0x8));
        assert_eq!(host_state(0x200), Some(0x10));
        assert_eq!(host_state(0x14), None);
        // Inputs a texture cannot be read in are read as shader resources.
        assert_eq!(input_state(0x14), 0xc0);
        assert_eq!(input_state(0x10), 0xc0);
        assert_eq!(input_state(0x4), 0x40);
        assert_eq!(input_state(0x200), 0x10);
        assert_eq!(output_state(0x2), 0x8);
        assert_eq!(output_state(0x100), 0x8);
    }

    fn camera(flags: u32) -> Camera {
        Camera {
            jitter: [0.25, -0.5],
            motion_scale: [1706.0, 960.0],
            render: [1706, 960],
            output: [2560, 1440],
            near: 0.1,
            far: 10_000.0,
            fov_y: 1.0,
            reset: true,
            flags,
        }
    }

    #[test]
    fn constants_follow_the_upscaling() {
        let c = camera(FFX_HIGH_DYNAMIC_RANGE | FFX_DEPTH_INVERTED | FFX_AUTO_EXPOSURE)
            .constants([1.0, 1.0], [1.0, 1.0]);
        // Motion vectors in UV space scaled to pixels: Streamline gets them
        // back to [-1, 1].
        assert_eq!(c.mvec_scale, [1.0, 1.0]);
        assert_eq!(c.jitter_offset, [0.25, -0.5]);
        assert_eq!(
            (c.depth_inverted, c.reset, c.camera_motion_included),
            (abi::TRUE, abi::TRUE, abi::TRUE)
        );
        assert_eq!(c.motion_vectors_jittered, abi::FALSE);
        assert!((c.camera_aspect_ratio - 1706.0 / 960.0).abs() < 1e-6);
        let flipped = camera(0).constants([-1.0, 1.0], [1.0, -2.0]);
        assert_eq!(flipped.jitter_offset, [-0.25, -0.5]);
        assert_eq!(flipped.mvec_scale, [1.0, -2.0]);
        assert_eq!(flipped.depth_inverted, abi::FALSE);
        // Display-resolution motion vectors are scaled by the output size.
        let display = camera(FFX_DISPLAY_RESOLUTION_MOTION_VECTORS);
        assert_eq!(display.motion_size(), [2560, 1440]);
        assert_eq!(
            display.mvec_scale([1.0, 1.0]),
            [1706.0 / 2560.0, 960.0 / 1440.0]
        );
    }

    /// Points at the near and far planes land at the depth the convention
    /// says; the inverse maps clip space back.
    #[test]
    fn projections_place_near_and_far() {
        let depth = |m: &abi::Float4x4, z: f32| {
            let (zc, wc) = (z * m[2][2] + m[3][2], z * m[2][3] + m[3][3]);
            zc / wc
        };
        let standard = projection(0.5, 100.0, 1.2, 16.0 / 9.0, false);
        assert!((depth(&standard, 0.5) - 0.0).abs() < 1e-5);
        assert!((depth(&standard, 100.0) - 1.0).abs() < 1e-5);
        let reversed = projection(0.5, 100.0, 1.2, 16.0 / 9.0, true);
        assert!((depth(&reversed, 0.5) - 1.0).abs() < 1e-5);
        assert!((depth(&reversed, 100.0) - 0.0).abs() < 1e-5);
        let infinite = projection(0.5, f32::MAX, 1.2, 1.0, true);
        assert!((depth(&infinite, 0.5) - 1.0).abs() < 1e-5);
        assert!(depth(&infinite, 1e7) < 1e-6);
        assert_eq!(projection(0.0, 100.0, 1.2, 1.0, false), IDENTITY);
        assert_eq!(projection(f32::NAN, 100.0, 1.2, 1.0, false), IDENTITY);
        for m in [standard, reversed, infinite] {
            let inverse = invert(&m).unwrap();
            let product: abi::Float4x4 = std::array::from_fn(|r| {
                std::array::from_fn(|c| (0..4).map(|k| m[r][k] * inverse[k][c]).sum())
            });
            for (r, row) in product.iter().enumerate() {
                for (c, value) in row.iter().enumerate() {
                    let want = if r == c { 1.0 } else { 0.0 };
                    assert!((value - want).abs() < 1e-4, "{r} {c} {value}");
                }
            }
        }
        assert_eq!(invert(&IDENTITY), Some(IDENTITY));
        assert_eq!(invert(&[[0.0; 4]; 4]), None);
    }

    #[test]
    fn multiplier_pairs() {
        assert_eq!(pair(None), [1.0, 1.0]);
        assert_eq!(pair(Some("-1, 1")), [-1.0, 1.0]);
        assert_eq!(pair(Some("1")), [1.0, 1.0]);
        assert_eq!(pair(Some("1,2,3")), [1.0, 1.0]);
        assert_eq!(pair(Some("x,1")), [1.0, 1.0]);
    }

    fn asked(vars: &[(&str, &str)]) -> Result<Asked, String> {
        Asked::from_vars(|name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    /// Off unless `VICTORY_DLSS=1`; quality and the library's preset by
    /// default; no folder unless one is named; bad names are errors.
    #[test]
    fn switches_are_off_by_default() {
        let none = asked(&[]).unwrap();
        assert!(!none.dlss);
        assert_eq!((none.mode, none.preset), (Mode::Quality, 0));
        assert_eq!((none.dir, none.log_dir), (None, None));
        assert_eq!((none.jitter, none.mvec), ([1.0, 1.0], [1.0, 1.0]));
        assert!(none.reactive);
        assert!(!asked(&[("VICTORY_DLSS", "0")]).unwrap().dlss);
        assert!(!asked(&[("VICTORY_DLSS", "yes")]).unwrap().dlss);
        let on = asked(&[
            ("VICTORY_DLSS", " 1 "),
            ("VICTORY_DLSS_MODE", "Performance"),
            ("VICTORY_DLSS_PRESET", "k"),
            ("VICTORY_STREAMLINE_DIR", r"D:\sdk\bin\x64"),
            ("VICTORY_STREAMLINE_LOG", "0"),
            ("VICTORY_DLSS_JITTER", "-1,1"),
            ("VICTORY_DLSS_REACTIVE", "0"),
        ])
        .unwrap();
        assert!(on.dlss);
        assert_eq!((on.mode, on.preset), (Mode::Performance, 11));
        assert_eq!(on.dir, Some(PathBuf::from(r"D:\sdk\bin\x64")));
        assert_eq!(on.log_dir, None);
        assert_eq!(on.jitter, [-1.0, 1.0]);
        assert!(!on.reactive);
        assert!(asked(&[("VICTORY_DLSS_MODE", "fast")]).is_err());
        assert!(asked(&[("VICTORY_DLSS_PRESET", "z")]).is_err());
        assert_eq!(
            asked(&[("VICTORY_DLSS_MODE", "")]).unwrap().mode,
            Mode::Quality
        );
    }
}
