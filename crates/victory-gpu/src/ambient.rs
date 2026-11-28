//! An estimate of the indirect light the title traces rays for, put in
//! where the rays find nothing.
//!
//! With the ray-tracing structure builder off (`crate::pipeline::builder_runs`)
//! every ray misses. The program that resolves the diffuse light of the
//! title's screen probes (`CS_ScreenProbes_TemporalResolve_*`) then leaves
//! the sky's light outdoors, nothing indoors (walls no lamp or sun reaches
//! stay black) and, in blocks of tiles, whatever the probes' memory held.
//! Right after that program a pass of ours raises the light to an estimate
//! and bounds what the program left by a multiple of it.
//!
//! The estimate follows the frame: the light the visible surfaces send out
//! (which the title keeps for its own screen traces,
//! `CS_CacheScreenSpaceRadiance`, the frame before) is averaged in 64x36
//! cells, and each point gets the mean of the cells weighted by how much of
//! its surroundings they cover, as a directional value (first-order
//! harmonics at 160x90 points, picked between by depth). It assumes that
//! what is off screen sends out what is on it, with a slow mean over the
//! last seconds so that turning into a dark corner does not go black at
//! once, and no occlusion but the creases' (from the depth).
//!
//! Switches, all read once at the first use and off by default:
//! - `VICTORY_AMBIENT`: unset, `0` or `off` leave the title's light as it is;
//!   a number is the share of the estimate, clamped to 0.05..=4 (1: all of
//!   it; 0.8 is the value other trees of this code use); `constant:<level>`
//!   puts the same light everywhere instead, shaded in creases (level
//!   clamped to 0.001..=16).
//! - `VICTORY_AMBIENT_SHADE=<strength>,<radius in metres>`: the creases'
//!   shade, default `1,0.6`; strength clamped to 0..=4 (0: none), radius to
//!   0.05..=8.
//! - `VICTORY_AMBIENT_ONCE=1`: one bounce of the title's own light, without
//!   the estimate's own light coming back to it ([`once`]).
//! - `VICTORY_AMBIENT_AIR=<multiple>`: the probes also light the air of a
//!   room; without rays they hold the sky's light indoors too, a coloured
//!   veil. This bounds what the air scatters by that multiple of the light
//!   around, clamped to 0.1..=64 (unset or 0: the title's; 1.5 is the value
//!   other trees use). Only with `VICTORY_AMBIENT` set to a number.
//! - `VICTORY_AMBIENT_SPECULAR=<share>[,<sky>]`: reflection rays the title's
//!   screen-space march does not resolve go to the structures, find nothing,
//!   and `CS_RayMiss` gives each the sky's light from its cube maps, far
//!   brighter than a room and blue. Right after that program the same
//!   estimate is put in along the reflected direction ([`Role::Miss`]),
//!   `share` of it (clamped to 0.05..=4; 1 is the value other trees use),
//!   with `sky` (0..=1, default 0) of the title's sky kept, never brighter
//!   than the estimate. The screen-space resolve that comes next still
//!   writes what it found over it. Works without `VICTORY_AMBIENT`.
//! - `VICTORY_AMBIENT_SPECULAR_ALPHA=0`: the alpha layer's misses are left as
//!   the title has them (on by default with the switch above: without it
//!   glass in a dark room mirrors the bright sky).
//!
//! None of these is in the table of switches that may alternate within a
//! run (`crate::switches`): the estimate is made from the frame before and a
//! slow mean, which an arm that skips the keep would leave stale for the
//! other, and the programs' roles are found once, when a shader is first
//! seen, only if a switch is set.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};

/// Programs of the title the passes follow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// Resolves the diffuse light.
    Resolve,
    /// Keeps the light visible surfaces send out.
    Radiance,
    /// Adds the lamps' light to what the air scatters (the last program
    /// that writes that volume before it is summed along the view).
    Air,
    /// Gives reflection rays that found nothing the sky's light
    /// (`CS_RayMiss`, not the volumetric nor the screen probes' variant).
    Miss,
}

/// The program's role, by the name in its container (any variant of it);
/// `None` for every other program.
pub(crate) fn role(container: &[u8]) -> Option<Role> {
    let has = |name: &[u8]| container.windows(name.len()).any(|w| w == name);
    if has(b"CS_ScreenProbes_TemporalResolve") {
        Some(Role::Resolve)
    } else if has(b"CS_CacheScreenSpaceRadiance") {
        Some(Role::Radiance)
    } else if has(b"CS_AccumulateLocalLightInscatteringVolume") {
        Some(Role::Air)
    } else if is_miss(container) {
        Some(Role::Miss)
    } else {
        None
    }
}

/// `CS_RayMiss_` followed by the eight hex digits of a variant's hash and
/// `_`: the plain program, not its named variants.
fn is_miss(container: &[u8]) -> bool {
    const NAME: &[u8] = b"CS_RayMiss_";
    container.windows(NAME.len() + 9).any(|w| {
        w.starts_with(NAME)
            && w[NAME.len()..NAME.len() + 8]
                .iter()
                .all(u8::is_ascii_hexdigit)
            && w[NAME.len() + 8] == b'_'
    })
}

/// Where the resolve writes the diffuse light (R9G9B9E5 in a 32-bit
/// integer texture) and reads the scene's depth, and where the radiance
/// program writes the light of visible surfaces: root parameter and place
/// in its table, in dwords.
pub(crate) const LIGHT: (u8, u32) = (1, 176);
/// See [`LIGHT`].
pub(crate) const DEPTH: (u8, u32) = (0, 464);
/// See [`LIGHT`].
pub(crate) const RADIANCE: (u8, u32) = (1, 16);
/// Where the volume of the light the air scatters is.
pub(crate) const AIR: (u8, u32) = (1, 32);
/// Where the miss program writes the reflected light (R11G11B10, half the
/// picture's size each way) and reads the picture's 32-bit depth; variants
/// may bind them elsewhere (`crate::draw`, found by kind then).
pub(crate) const MISS_LIGHT: (u8, u32) = (1, 104);
/// See [`MISS_LIGHT`].
pub(crate) const MISS_DEPTH: (u8, u32) = (0, 1000);
/// The miss program's first root constant: which surfaces its rays are
/// of. Other values (water) are left as the title has them: the depth there
/// is of what lies under the surface.
pub(crate) const MISS_OPAQUE: u32 = 0;
/// See [`MISS_OPAQUE`].
pub(crate) const MISS_ALPHA: u32 = 1;

/// `VICTORY_AMBIENT`, parsed.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Set {
    Off,
    /// The share of the estimate.
    Estimate(f32),
    /// The same light everywhere.
    Constant(f32),
}

/// A positive finite number, clamped; `None` for anything else (0 too).
fn positive(text: &str, low: f32, high: f32) -> Option<f32> {
    text.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.clamp(low, high))
}

fn parse(set: &str) -> Set {
    match set.trim().strip_prefix("constant:") {
        Some(level) => positive(level, 0.001, 16.0).map_or(Set::Off, Set::Constant),
        None => positive(set, 0.05, 4.0).map_or(Set::Off, Set::Estimate),
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn set() -> Set {
    static SET: OnceLock<Set> = OnceLock::new();
    *SET.get_or_init(|| parse(&env("VICTORY_AMBIENT")))
}

/// Whether a switch asks for an estimate (diffuse or reflected): only then
/// are the programs' roles looked for when a shader is first seen.
pub(crate) fn wanted() -> bool {
    set() != Set::Off || specular().is_some()
}

/// Whether the passes run: asked for, and the title's rays find nothing.
pub(crate) fn on() -> bool {
    wanted() && !crate::pipeline::builder_runs()
}

/// `VICTORY_AMBIENT_SPECULAR`, parsed.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Specular {
    /// The share of the estimate along the reflected direction.
    share: f32,
    /// The share of the title's sky kept, 0 to 1.
    sky: f32,
}

fn parse_specular(set: &str) -> Option<Specular> {
    let mut parts = set.split(',');
    let share = positive(parts.next()?, 0.05, 4.0)?;
    let sky = parts
        .next()
        .and_then(|t| t.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map_or(0.0, |v| v.clamp(0.0, 1.0));
    Some(Specular { share, sky })
}

fn specular() -> Option<Specular> {
    static SET: OnceLock<Option<Specular>> = OnceLock::new();
    *SET.get_or_init(|| parse_specular(&env("VICTORY_AMBIENT_SPECULAR")))
}

/// Whether the alpha layer's misses get the estimate too
/// (`VICTORY_AMBIENT_SPECULAR_ALPHA`, on unless `0`).
pub(crate) fn specular_alpha() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| env("VICTORY_AMBIENT_SPECULAR_ALPHA").trim() != "0")
}

/// `VICTORY_AMBIENT_ONCE=1`: the estimate is one bounce of the light the
/// title's own lighting put there, not of the estimate's too.
///
/// The estimate is made from the light surfaces sent out the frame before,
/// which it had raised itself: its own light comes back, reflected. With a
/// share `level` and surfaces that send out `a` of the light they get, the
/// light settles at `level * e / (1 - level * a)` instead of one bounce, so
/// pale rooms and lamps come out too bright. With this switch the raise
/// notes, at the 25 places of each cell that the next keep reads, the share
/// of the light there that is the title's own, and the keep counts each
/// place by that share. Light a surface makes itself is counted down too
/// where the estimate raised it: there is no albedo to tell them apart.
fn once() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| env("VICTORY_AMBIENT_ONCE").trim() == "1")
}

fn parse_air(set: &str) -> f32 {
    positive(set, 0.1, 64.0).unwrap_or(0.0)
}

/// The most the air scatters, as a multiple of the light around; 0 leaves
/// it as the title has it.
fn air() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| parse_air(&env("VICTORY_AMBIENT_AIR")))
}

fn parse_shade(set: &str) -> [f32; 2] {
    let mut shade = [1.0, 0.6];
    let ranges = [(0.0, 4.0), (0.05, 8.0)];
    for ((to, (low, high)), v) in shade.iter_mut().zip(ranges).zip(set.split(',')) {
        if let Ok(v) = v.trim().parse::<f32>()
            && v.is_finite()
        {
            *to = v.clamp(low, high);
        }
    }
    shade
}

/// Strength and radius (metres) of the creases' shade.
fn shade() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| parse_shade(&env("VICTORY_AMBIENT_SHADE")))
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=12, b0), DescriptorTable(SRV(t0)), DescriptorTable(UAV(u0)), UAV(u1), UAV(u2)"
#define CX 64
#define CY 36
#define CELLS (CX * CY)
#define PX 160
#define PY 90
cbuffer Set : register(b0) {
    float level; float constant; float strength; float radius;
    float near; float tan_y; uint width; uint height;
    float follow; float most; float air; float once;
};
Texture2D<float> depth : register(t0);
// Cells: the light they send out (then, at CELLS, the slow mean), from
// CELLS + 1 on where they are, from SHARES on the share of the title's own
// light at each of the 25 places of a cell the keep reads (x; y: written).
#define SHARES (2 * CELLS + 1)
RWStructuredBuffer<float4> cells : register(u1);
// Three per point: red, green and blue, each a mean and a direction.
RWStructuredBuffer<float4> points : register(u2);
static const float3 LUMA = float3(0.2126, 0.7152, 0.0722);

// NaN or infinity in any channel: all exponent bits set. By the bits, as
// not every HLSL compiler (Wine's) has isnan and isinf.
bool bad(float3 c) {
    return any((asuint(c) & 0x7f800000u) == 0x7f800000u);
}

// The point seen at a texel, in metres from the eye (x right, y up, z away).
float3 seen(int2 p, float2 size) {
    float d = depth.Load(int3(clamp(p, int2(0, 0), int2(size) - 1), 0));
    float z = min(near / max(d, 1e-7), 1000.0);
    float2 ndc = (float2(p) + 0.5) / size * 2.0 - 1.0;
    return float3(ndc.x * tan_y * size.x / size.y * z, -ndc.y * tan_y * z, z);
}

#ifdef KEEP
RWTexture2D<float3> radiance : register(u0);
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void keep(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    radiance.GetDimensions(w, h);
    float3 sum = 0;
    for (uint y = 0; y < 5; y++) for (uint x = 0; x < 5; x++) {
        uint2 p = uint2((float2(at.xy) + (float2(x, y) + 0.5) / 5.0) * float2(w, h) / float2(CX, CY));
        float3 c = max(radiance[p], 0.0);
        c = bad(c) ? 0.0 : c;
        // Lamps and glints count, but they must not light everything.
        c /= max(1.0, dot(c, LUMA) / 20.0);
        // Without the estimate's own light, reflected (`once`).
        float4 share = cells[SHARES + (at.y * CX + at.x) * 25 + y * 5 + x];
        sum += once > 0.0 && share.y > 0.0 ? c * saturate(share.x) : c;
    }
    cells[at.y * CX + at.x] = float4(sum / 25.0, 1.0);
}
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void mean() {
    float3 sum = 0;
    for (uint i = 0; i < CELLS; i++) sum += cells[i].rgb;
    float4 was = cells[CELLS];
    float3 now = sum / CELLS;
    cells[CELLS] = float4(was.a > 0.0 ? lerp(was.rgb, now, follow) : now, 1.0);
}
#endif

#ifdef PLACE
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void place(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(CX, CY)), size);
    cells[CELLS + 1 + at.y * CX + at.x] = float4(p, p.z * p.z);
}
#endif

#ifdef GATHER
[RootSignature(RS)]
[numthreads(8, 2, 1)]
void gather(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(PX, PY)), size);
    float r2 = 0.25 + 0.0025 * p.z * p.z;
    float whole = 0;
    float3 toward = 0, light = 0, red = 0, green = 0, blue = 0;
    for (uint i = 0; i < CELLS; i++) {
        float4 c = cells[CELLS + 1 + i];
        float3 L = cells[i].rgb;
        float3 d = c.xyz - p;
        float dd = dot(d, d);
        float w = c.w / (dd + r2);
        float3 dir = d / sqrt(dd + 0.25 * r2);
        whole += w;
        toward += w * dir;
        light += w * L;
        red += w * L.r * dir;
        green += w * L.g * dir;
        blue += w * L.b * dir;
    }
    whole = max(whole, 1e-6);
    light /= whole;
    toward /= whole;
    uint at0 = (at.y * PX + at.x) * 3;
    points[at0] = float4(red / whole - light.r * toward, light.r);
    points[at0 + 1] = float4(green / whole - light.g * toward, light.g);
    points[at0 + 2] = float4(blue / whole - light.b * toward, light.b);
}
#endif

#ifdef AIR
RWTexture3D<float3> scattered : register(u0);
// No more than a multiple of the light around: of the room as the last
// seconds showed it near the eye, and of the surface seen in this
// direction far away (towards a window the air outside may be as bright
// as the sky).
[RootSignature(RS)]
[numthreads(8, 8, 2)]
void clear_air(uint3 at : SV_DispatchThreadID) {
    uint w, h, d;
    scattered.GetDimensions(w, h, d);
    if (at.x >= w || at.y >= h || at.z >= d) return;
    uint2 q = min(uint2((float2(at.xy) + 0.5) / float2(w, h) * float2(PX, PY)), uint2(PX - 1, PY - 1));
    uint at0 = (q.y * PX + q.x) * 3;
    float3 seen_there = float3(points[at0].w, points[at0 + 1].w, points[at0 + 2].w);
    float far = float(at.z) / float(d);
    float cap = air * lerp(dot(cells[CELLS].rgb, LUMA), dot(seen_there, LUMA), far);
    float3 c = scattered[at];
    float has = dot(c, LUMA);
    if (has > cap) scattered[at] = c * (cap / has);
}
#endif

#ifdef REFLECT
RWTexture2D<float3> reflected : register(u0);
// Each texel of the reflections' image: the light the points around send
// along the reflected direction, as `raise` takes it along the normal;
// `level` is the share, `constant` the share of the title's sky kept, made
// no brighter than that.
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void reflect_light(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    reflected.GetDimensions(w, h);
    if (at.x >= w || at.y >= h) return;
    float2 size = float2(width, height);
    int2 p = int2((float2(at.xy) + 0.5) * size / float2(w, h));
    float3 c = seen(p, size);
    // The sky reflects nothing: it keeps the title's value.
    if (c.z >= 999.0) return;
    float3 l = seen(p + int2(-1, 0), size), r = seen(p + int2(1, 0), size);
    float3 u = seen(p + int2(0, -1), size), d = seen(p + int2(0, 1), size);
    float3 dx = abs(l.z - c.z) < abs(r.z - c.z) ? c - l : r - c;
    float3 dy = abs(u.z - c.z) < abs(d.z - c.z) ? c - u : d - c;
    float3 n = normalize(cross(dy, dx) + float3(0, 0, -1e-9));
    if (dot(n, c) > 0.0) n = -n;
    float3 v = normalize(c);
    float3 ray = v - 2.0 * dot(v, n) * n;
    float2 g = (float2(p) + 0.5) / size * float2(PX, PY) - 0.5;
    int2 g0 = int2(floor(g));
    float2 f = g - float2(g0);
    float4 sr = 0, sg = 0, sb = 0;
    float sum = 0;
    for (int k = 0; k < 4; k++) {
        int2 q = clamp(g0 + int2(k & 1, k >> 1), int2(0, 0), int2(PX, PY) - 1);
        float z = seen(int2((float2(q) + 0.5) * size / float2(PX, PY)), size).z;
        float2 b = float2(k & 1 ? f.x : 1 - f.x, k >> 1 ? f.y : 1 - f.y);
        float wgt = (b.x * b.y + 1e-3) / (0.02 + abs(z - c.z) / c.z);
        uint at0 = (q.y * PX + q.x) * 3;
        sr += wgt * points[at0];
        sg += wgt * points[at0 + 1];
        sb += wgt * points[at0 + 2];
        sum += wgt;
    }
    float3 all = float3(sr.w, sg.w, sb.w) / sum;
    // Radiance from the first moments: three times the direction's share
    // when directions spread evenly, a little less for a smooth image.
    float3 found = all + 2.0 * float3(dot(sr.xyz, ray), dot(sg.xyz, ray), dot(sb.xyz, ray)) / sum;
    found = clamp(found, 0.1 * all, 3.0 * all);
    found = lerp(found, cells[CELLS].rgb, 0.2) * level;
    found = bad(found) ? 0.0 : max(found, 0.0);
    float3 was = reflected[at.xy];
    was = bad(was) ? 0.0 : max(was, 0.0);
    float has = dot(was, LUMA);
    was *= min(1.0, dot(found, LUMA) / max(has, 1e-6));
    reflected[at.xy] = lerp(found, was, constant);
}
#endif

#ifdef RAISE
RWTexture2D<uint> light : register(u0);
float3 unpack(uint v) {
    return float3(v & 511, (v >> 9) & 511, (v >> 18) & 511) * exp2(float(v >> 27) - 24.0);
}
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float top = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(top, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (top * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void raise(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    if (at.x >= width || at.y >= height) return;
    int2 p = int2(at.xy);
    float3 c = seen(p, size);
    float3 l = seen(p + int2(-1, 0), size), r = seen(p + int2(1, 0), size);
    float3 u = seen(p + int2(0, -1), size), d = seen(p + int2(0, 1), size);
    float3 dx = abs(l.z - c.z) < abs(r.z - c.z) ? c - l : r - c;
    float3 dy = abs(u.z - c.z) < abs(d.z - c.z) ? c - u : d - c;
    float3 n = normalize(cross(dy, dx) + float3(0, 0, -1e-9));
    if (dot(n, c) > 0.0) n = -n;
    // How open the point is: what of the depth around rises above its plane.
    float open = 1.0;
    float px = min(radius * size.y / (2.0 * tan_y * c.z), 128.0);
    float noise = frac(52.9829189 * frac(dot(float2(p), float2(0.06711056, 0.00583715))));
    if (strength > 0.0 && px >= 1.5 && c.z < 400.0) {
        float sum = 0.0;
        const int N = 12;
        for (int i = 0; i < N; i++) {
            float a = i * 2.399963 + noise * 6.2831853;
            float2 o = float2(cos(a), sin(a)) * (sqrt((i + 0.5) / N) * px);
            float3 v = seen(p + int2(round(o)), size) - c;
            float vv = dot(v, v);
            float over = dot(v, n) - 0.004 * c.z - 0.005;
            sum += max(over, 0.0) / sqrt(vv + 1e-6) * saturate(1.0 - vv / (radius * radius));
        }
        open = saturate(1.0 - strength * 2.0 * sum / N);
    }
    float3 found;
    if (constant > 0.0) {
        found = float3(1.0, 0.98, 0.94) * constant * open;
        light[p] = pack(found);
        return;
    }
    // Between the four points around, those at its own depth first; which
    // four is dithered a little.
    float2 g = (float2(p) + 0.5) / size * float2(PX, PY) - 0.5 + (noise - 0.5) * 0.5;
    int2 g0 = int2(floor(g));
    float2 f = g - float2(g0);
    float4 sr = 0, sg = 0, sb = 0;
    float sum = 0;
    for (int k = 0; k < 4; k++) {
        int2 q = clamp(g0 + int2(k & 1, k >> 1), int2(0, 0), int2(PX, PY) - 1);
        float z = seen(int2((float2(q) + 0.5) * size / float2(PX, PY)), size).z;
        float2 b = float2(k & 1 ? f.x : 1 - f.x, k >> 1 ? f.y : 1 - f.y);
        float w = (b.x * b.y + 1e-3) / (0.02 + abs(z - c.z) / c.z);
        uint at0 = (q.y * PX + q.x) * 3;
        sr += w * points[at0];
        sg += w * points[at0 + 1];
        sb += w * points[at0 + 2];
        sum += w;
    }
    float3 all = float3(sr.w, sg.w, sb.w) / sum;
    found = all + 1.2 * float3(dot(sr.xyz, n), dot(sg.xyz, n), dot(sb.xyz, n)) / sum;
    found = clamp(found, 0.2 * all, 2.5 * all);
    found = lerp(found, cells[CELLS].rgb, 0.2) * level * open;
    float3 was = unpack(light[p]);
    // Magenta leftovers of the probes' memory (green far under red and
    // blue), and the bluer kind (blue over 2.5x green and over red): no
    // light is like that. Sky light keeps green above 0.4x blue and sunsets
    // keep blue low, so neither is touched.
    if (was.g < 0.5 * min(was.r, was.b) && max(was.r, was.b) > 0.02) was = 0.0;
    if (was.b > 2.5 * was.g && was.b > was.r && was.b > 0.02) was = 0.0;
    // What the program left counts as far as it can be true: the sky's
    // light outdoors, not the blocks of other memory.
    float top = max(dot(found, LUMA) * most, 1e-6);
    float has = dot(was, LUMA);
    was *= min(1.0, top / max(has, 1e-6));
    float3 now = max(was, found);
    light[p] = pack(now);
    if (once > 0.0) {
        // At the places the next keep reads, the share of the light that is
        // the title's own.
        float2 gc = (float2(p) + 0.5) / size * float2(CX, CY);
        uint2 cell = min(uint2(gc), uint2(CX - 1, CY - 1));
        uint2 k = min(uint2((gc - float2(cell)) * 5.0), uint2(4, 4));
        uint2 q = uint2((float2(cell) + (float2(k) + 0.5) / 5.0) * size / float2(CX, CY));
        if (q.x == uint(p.x) && q.y == uint(p.y)) {
            float share = dot(was, LUMA) / max(dot(now, LUMA), 1e-6);
            cells[SHARES + (cell.y * CX + cell.x) * 25 + k.y * 5 + k.x] = float4(share, 1.0, 0.0, 0.0);
        }
    }
}
#endif
"#;

/// The cells' buffer: light, slow mean, places, shares (16 bytes each).
const CELLS: u64 = 64 * 36;
const CELL_BYTES: u64 = (2 * CELLS + 1 + 25 * CELLS) * 16;
/// The points' buffer: three 16-byte entries a point.
const POINT_BYTES: u64 = 160 * 90 * 48;

/// The near plane (metres) and the tangent of half the vertical field of
/// view the depth is read with; the title's own are not known here.
const NEAR_M: f32 = 0.1;
const TAN_HALF_FOV_Y: f32 = 0.45;
/// How fast the slow mean follows the frame's mean, per frame.
const FOLLOW: f32 = 0.04;
/// The most the title's leftover light may be, as a multiple of the
/// estimate.
const MOST: f32 = 4.0;

/// The root constants of the passes, in the order of `SOURCE`'s `Set`.
fn constants(level: f32, constant: f32, depth_size: [u32; 2]) -> [u32; 12] {
    let [strength, radius] = shade();
    [
        level.to_bits(),
        constant.to_bits(),
        strength.to_bits(),
        radius.to_bits(),
        NEAR_M.to_bits(),
        TAN_HALF_FOV_Y.to_bits(),
        depth_size[0],
        depth_size[1],
        FOLLOW.to_bits(),
        MOST.to_bits(),
        air().to_bits(),
        if once() { 1.0f32 } else { 0.0 }.to_bits(),
    ]
}

struct Pass {
    root: ID3D12RootSignature,
    keep: ID3D12PipelineState,
    mean: ID3D12PipelineState,
    place: ID3D12PipelineState,
    gather: ID3D12PipelineState,
    raise: ID3D12PipelineState,
    air: ID3D12PipelineState,
    reflect: ID3D12PipelineState,
    cells: ID3D12Resource,
    points: ID3D12Resource,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

fn make(
    device: &ID3D12Device,
    buffer: impl Fn(u64) -> Option<ID3D12Resource>,
) -> windows::core::Result<Pass> {
    let mut root: Option<ID3D12RootSignature> = None;
    let mut pipeline = |part: &str, entry| -> windows::core::Result<ID3D12PipelineState> {
        let code = crate::host::compile(&format!("#define {part}\n{SOURCE}"), entry, s!("cs_5_1"))?;
        // SAFETY: the root signature is in the shader's bytecode, which lives
        // through both calls; `root` outlives the description that borrows it.
        unsafe {
            if root.is_none() {
                root = Some(device.CreateRootSignature(0, &code)?);
            }
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            };
            device.CreateComputePipelineState(&desc)
        }
    };
    let keep = pipeline("KEEP", s!("keep"))?;
    let mean = pipeline("KEEP", s!("mean"))?;
    let place = pipeline("PLACE", s!("place"))?;
    let gather = pipeline("GATHER", s!("gather"))?;
    let raise = pipeline("RAISE", s!("raise"))?;
    let air = pipeline("AIR", s!("clear_air"))?;
    let reflect = pipeline("REFLECT", s!("reflect_light"))?;
    let made = |bytes| buffer(bytes).ok_or_else(windows::core::Error::empty);
    Ok(Pass {
        root: root.ok_or_else(windows::core::Error::empty)?,
        keep,
        mean,
        place,
        gather,
        raise,
        air,
        reflect,
        cells: made(CELL_BYTES)?,
        points: made(POINT_BYTES)?,
    })
}

/// The passes, made at their first use; none (said once) if the system's
/// compiler or the device refuses them.
fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| {
        let host = host()?;
        make(&host.device, |bytes| host.uav_buffer(bytes))
            .inspect_err(|e| tracing::warn!(error = %e, "no estimate of the indirect light"))
            .ok()
    })
    .as_ref()
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Whether the light of a frame has been kept: until then the estimate has
/// nothing to start from and the title's light is left as it is.
static KEPT: AtomicBool = AtomicBool::new(false);

/// What the passes read and write, as views in the heap the list has set.
pub(crate) struct Views {
    /// The image the title's program just wrote.
    pub(crate) image: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// The scene's depth (for [`Role::Radiance`] and [`Role::Air`]: unused).
    pub(crate) depth: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// The image's size in texels, and its slices (a volume's depth).
    pub(crate) size: [u32; 2],
    /// See [`Views::size`].
    pub(crate) slices: u32,
    /// The depth's size in texels (the image's, but for [`Role::Miss`]).
    pub(crate) depth_size: [u32; 2],
}

/// What the reflections' pass did, for the line every ten seconds.
struct Said {
    /// Dispatches of the miss program it followed and frames they were in.
    ran: AtomicU64,
    frames: AtomicU64,
    /// Dispatches it left as they were: of other layers, and before a
    /// frame's light was kept (or with no image or depth found).
    other_layer: AtomicU64,
    not_ready: AtomicU64,
    last_frame: AtomicU64,
    /// Bit 0 opaque surfaces, 1 alpha, 2 others.
    layers: AtomicU64,
}

static SAID_MISS: Said = Said {
    ran: AtomicU64::new(0),
    frames: AtomicU64::new(0),
    other_layer: AtomicU64::new(0),
    not_ready: AtomicU64::new(0),
    last_frame: AtomicU64::new(u64::MAX),
    layers: AtomicU64::new(0),
};

/// What came of a dispatch of the miss program.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MissNote {
    /// The estimate was put in, for rays of that layer.
    Ran(u32),
    /// A layer left as the title has it.
    OtherLayer,
    /// No frame's light kept yet, or no image or depth found.
    NotReady,
}

/// Counts what came of a dispatch of the miss program; every ten seconds an
/// info line says it, so that a run shows whether and where the pass ran.
pub(crate) fn note_miss(note: MissNote) {
    use Ordering::Relaxed;
    let Some(Specular { share, sky }) = specular() else {
        return;
    };
    let s = &SAID_MISS;
    match note {
        MissNote::Ran(layer) => {
            s.ran.fetch_add(1, Relaxed);
            s.layers.fetch_or(1 << layer.min(2), Relaxed);
            let frame = crate::frame::current();
            if s.last_frame.swap(frame, Relaxed) != frame {
                s.frames.fetch_add(1, Relaxed);
            }
        }
        MissNote::OtherLayer => {
            s.other_layer.fetch_add(1, Relaxed);
        }
        MissNote::NotReady => {
            s.not_ready.fetch_add(1, Relaxed);
        }
    }
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let Some(since) = *last else {
        *last = Some(now);
        return;
    };
    let seconds = now.duration_since(since).as_secs_f32();
    if seconds < 10.0 {
        return;
    }
    *last = Some(now);
    let layers = s.layers.swap(0, Relaxed);
    let named: Vec<&str> = [(1, "opaque"), (2, "alpha"), (4, "other")]
        .iter()
        .filter(|(bit, _)| layers & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    tracing::info!(
        share,
        sky,
        seconds,
        frames = s.frames.swap(0, Relaxed),
        dispatches = s.ran.swap(0, Relaxed),
        layers = if named.is_empty() {
            "none".to_owned()
        } else {
            named.join(",")
        },
        other_layer = s.other_layer.swap(0, Relaxed),
        not_ready = s.not_ready.swap(0, Relaxed),
        "reflection estimate (VICTORY_AMBIENT_SPECULAR): dispatches of the miss program followed"
    );
}

/// The share of the estimate and the constant (for reflections: the share
/// of the title's sky kept) a pass after a program of `role` uses; `None`
/// when no pass follows it under these switches.
fn levels(role: Role, set: Set, specular: Option<Specular>) -> Option<(f32, f32)> {
    match (role, set, specular) {
        (Role::Miss, _, Some(s)) => Some((s.share, s.sky)),
        (Role::Miss, _, None) => None,
        // The reflections' estimate needs the kept light as well.
        (Role::Radiance, Set::Off, Some(_)) => Some((0.0, 0.0)),
        (_, Set::Off, _) => None,
        (_, Set::Estimate(level), _) => Some((level, 0.0)),
        (_, Set::Constant(level), _) => Some((0.0, level)),
    }
}

/// After the title's program of `role`, in `list` with the program's view
/// heap set: records the passes that follow it. They replace the list's
/// compute root signature and pipeline; false if nothing was recorded.
pub(crate) fn after(list: &HostList, role: Role, views: &Views) -> bool {
    let Some((level, constant)) = levels(role, set(), specular()) else {
        return false;
    };
    let estimate = role == Role::Miss || constant == 0.0;
    if role != Role::Radiance && estimate && !KEPT.load(Ordering::Relaxed) {
        if role == Role::Miss {
            note_miss(MissNote::NotReady);
        }
        return false;
    }
    if role == Role::Air && (constant > 0.0 || air() == 0.0) {
        return false;
    }
    let Some(pass) = pass() else {
        return false;
    };
    static SAID: Mutex<u32> = Mutex::new(0);
    {
        let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
        if *said < 5 {
            *said += 1;
            tracing::info!(?role, size = ?views.size, level, constant, "an estimate of the indirect light");
        }
    }
    list.keep(&pass.cells);
    list.keep(&pass.points);
    let words = constants(level, constant, views.depth_size);
    let l = &list.list;
    // SAFETY: an open list with the views' heap set; the title's program has
    // just used both views, so their textures are in the states the views
    // need; the buffers live as long as the process.
    unsafe {
        uav_barrier(l);
        l.SetComputeRootSignature(&pass.root);
        l.SetComputeRoot32BitConstants(0, 12, words.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(1, views.depth);
        l.SetComputeRootDescriptorTable(2, views.image);
        l.SetComputeRootUnorderedAccessView(3, pass.cells.GetGPUVirtualAddress());
        l.SetComputeRootUnorderedAccessView(4, pass.points.GetGPUVirtualAddress());
        let run = |pso: &ID3D12PipelineState, x, y, z| {
            l.SetPipelineState(pso);
            l.Dispatch(x, y, z);
            uav_barrier(l);
        };
        let [w, h] = views.size;
        match role {
            Role::Radiance => {
                run(&pass.keep, 8, 9, 1);
                run(&pass.mean, 1, 1, 1);
                KEPT.store(true, Ordering::Relaxed);
            }
            Role::Air => run(
                &pass.air,
                w.div_ceil(8),
                h.div_ceil(8),
                views.slices.div_ceil(2),
            ),
            Role::Resolve => {
                if constant == 0.0 {
                    run(&pass.place, 8, 9, 1);
                    run(&pass.gather, 20, 45, 1);
                }
                run(&pass.raise, w.div_ceil(8), h.div_ceil(8), 1);
            }
            Role::Miss => {
                // The points afresh from this frame's depth: the diffuse
                // estimate makes them later in the frame.
                run(&pass.place, 8, 9, 1);
                run(&pass.gather, 20, 45, 1);
                run(&pass.reflect, w.div_ceil(8), h.div_ceil(8), 1);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_share_is_off_unless_a_positive_number() {
        assert_eq!(parse(""), Set::Off);
        assert_eq!(parse("off"), Set::Off);
        assert_eq!(parse("0"), Set::Off);
        assert_eq!(parse("-1"), Set::Off);
        assert_eq!(parse("nan"), Set::Off);
        assert_eq!(parse("inf"), Set::Off);
        assert_eq!(parse(" 0.8 "), Set::Estimate(0.8));
        assert_eq!(parse("constant:0.3"), Set::Constant(0.3));
        assert_eq!(parse("constant:nan"), Set::Off);
        assert_eq!(parse("constant:0"), Set::Off);
    }

    #[test]
    fn the_shares_are_clamped() {
        assert_eq!(parse("100"), Set::Estimate(4.0));
        assert_eq!(parse("0.001"), Set::Estimate(0.05));
        assert_eq!(parse("constant:99"), Set::Constant(16.0));
        assert_eq!(parse_air("1.5"), 1.5);
        assert_eq!(parse_air("1000"), 64.0);
        assert_eq!(parse_air("0.01"), 0.1);
        assert_eq!(parse_air(""), 0.0);
        assert_eq!(parse_air("0"), 0.0);
        assert_eq!(parse_air("x"), 0.0);
    }

    #[test]
    fn reflection_settings() {
        assert_eq!(parse_specular(""), None);
        assert_eq!(parse_specular("0"), None);
        assert_eq!(parse_specular("off"), None);
        assert_eq!(
            parse_specular("1"),
            Some(Specular {
                share: 1.0,
                sky: 0.0
            })
        );
        assert_eq!(
            parse_specular("0.6, 0.25"),
            Some(Specular {
                share: 0.6,
                sky: 0.25
            })
        );
        assert_eq!(
            parse_specular("0.6,7"),
            Some(Specular {
                share: 0.6,
                sky: 1.0
            })
        );
        assert_eq!(
            parse_specular("0.6,x"),
            Some(Specular {
                share: 0.6,
                sky: 0.0
            })
        );
        assert_eq!(
            parse_specular("9"),
            Some(Specular {
                share: 4.0,
                sky: 0.0
            })
        );
    }

    #[test]
    fn crease_settings() {
        assert_eq!(parse_shade(""), [1.0, 0.6]);
        assert_eq!(parse_shade("0"), [0.0, 0.6]);
        assert_eq!(parse_shade("2,1.5"), [2.0, 1.5]);
        assert_eq!(parse_shade("x,0.3"), [1.0, 0.3]);
        assert_eq!(parse_shade("-1,0"), [0.0, 0.05]);
        assert_eq!(parse_shade("9,nan"), [4.0, 0.6]);
    }

    /// Which pass follows which program under which switches: none at all
    /// with both switches off, the keep alone for the reflections.
    #[test]
    fn passes_follow_only_with_a_switch_on() {
        let spec = Some(Specular {
            share: 1.0,
            sky: 0.0,
        });
        for role in [Role::Resolve, Role::Radiance, Role::Air, Role::Miss] {
            assert_eq!(levels(role, Set::Off, None), None, "{role:?}");
        }
        assert_eq!(levels(Role::Radiance, Set::Off, spec), Some((0.0, 0.0)));
        assert_eq!(levels(Role::Resolve, Set::Off, spec), None);
        assert_eq!(levels(Role::Air, Set::Off, spec), None);
        assert_eq!(levels(Role::Miss, Set::Off, spec), Some((1.0, 0.0)));
        assert_eq!(levels(Role::Miss, Set::Estimate(0.8), None), None);
        assert_eq!(
            levels(Role::Resolve, Set::Estimate(0.8), None),
            Some((0.8, 0.0))
        );
        assert_eq!(
            levels(Role::Resolve, Set::Constant(0.3), None),
            Some((0.0, 0.3))
        );
    }

    #[test]
    fn roles_by_name() {
        assert_eq!(
            role(b"..CS_ScreenProbes_TemporalResolve_a911b49c_Wrapped.."),
            Some(Role::Resolve)
        );
        assert_eq!(
            role(b"..CS_CacheScreenSpaceRadiance_eed2fc35.."),
            Some(Role::Radiance)
        );
        assert_eq!(
            role(b"CS_AccumulateLocalLightInscatteringVolume_7e2f5eef"),
            Some(Role::Air)
        );
        assert_eq!(role(b"CS_TemporalResolve"), None);
        assert_eq!(role(b"..CS_RayMiss_de6b91a6_Wrapped.."), Some(Role::Miss));
        assert_eq!(role(b"..CS_RayMiss_Volumetrics_3c5d8c00_Wrapped.."), None);
        assert_eq!(role(b"..CS_ScreenProbe_RayMiss_80a88544_Wrapped.."), None);
        assert_eq!(role(b"..CS_RayMiss_de6b91a_.."), None);
        assert_eq!(role(b""), None);
    }

    /// The constants the passes read: the share and constant as given, the
    /// depth's size as integers.
    #[test]
    fn the_constants_carry_the_sizes() {
        let words = constants(0.8, 0.0, [1920, 1080]);
        assert_eq!(f32::from_bits(words[0]), 0.8);
        assert_eq!(f32::from_bits(words[1]), 0.0);
        assert_eq!((words[6], words[7]), (1920, 1080));
        assert_eq!(f32::from_bits(words[9]), MOST);
    }

    /// The keep and raise as they act on the light, frame after frame, for
    /// surfaces that send out `a` of the light they get and the title's own
    /// light `title` on each: the light it settles at on each surface.
    fn settle(level: f32, a: f32, title: &[f32], once: bool) -> Vec<f32> {
        let mut light = title.to_vec();
        let mut share = vec![1.0f32; title.len()];
        for _ in 0..500 {
            let sent: f32 = light
                .iter()
                .zip(&share)
                .map(|(l, s)| a * l * if once { *s } else { 1.0 })
                .sum::<f32>()
                / title.len() as f32;
            // The raise never goes below the title's own light.
            for (i, t) in title.iter().enumerate() {
                light[i] = t.max(level * sent);
                share[i] = t / light[i].max(1e-6);
            }
        }
        light
    }

    /// Without `once` the estimate's own light comes back to it; with it,
    /// one bounce of the title's light.
    #[test]
    fn once_is_one_bounce_of_the_titles_light() {
        for a in [0.6f32, 0.9] {
            let one_bounce = 0.8 * a * 1.0 / 2.0;
            let fed = settle(0.8, a, &[1.0, 0.0], false)[1];
            let once = settle(0.8, a, &[1.0, 0.0], true)[1];
            assert!(
                (fed - one_bounce / (1.0 - 0.8 * a / 2.0)).abs() < 1e-4,
                "a={a}: fed {fed}"
            );
            assert!((once - one_bounce).abs() < 1e-4, "a={a}: once {once}");
            assert!(fed > 1.3 * once);
        }
        assert_eq!(settle(0.8, 0.6, &[1.0, 1.0], true), vec![1.0, 1.0]);
    }

    /// Each pass compiles with the system's compiler, no device needed.
    #[test]
    fn the_passes_compile() {
        let parts = [
            ("KEEP", s!("keep")),
            ("KEEP", s!("mean")),
            ("PLACE", s!("place")),
            ("GATHER", s!("gather")),
            ("RAISE", s!("raise")),
            ("AIR", s!("clear_air")),
            ("REFLECT", s!("reflect_light")),
        ];
        for (part, entry) in parts {
            let code =
                crate::host::compile(&format!("#define {part}\n{SOURCE}"), entry, s!("cs_5_1"));
            assert!(matches!(&code, Ok(c) if !c.is_empty()), "{part}: {code:?}");
        }
    }

    /// The passes compile with the system's compiler and the device takes
    /// them (no device: nothing to check).
    #[test]
    fn the_passes_build() {
        let Some(host) = host() else {
            return;
        };
        make(&host.device, |bytes| host.uav_buffer(bytes)).expect("the passes");
    }
}
