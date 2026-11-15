// Classic noise reduction on the linear Rec.2020 develop buffer `lin`.
// Luminance: generalised Anscombe transform (unit-variance noise under the
// Poisson–Gaussian model), non-local means, inverse. Chrominance: opponent
// space (Cb = B − Y, Cr = R − Y), edge-aware à-trous B3-spline smoothing
// guided by Y, blended in; Y is kept exactly.

struct NrParams {
    w: u32,
    h: u32,
    // NLM patch and search radii (pixels of this buffer); à-trous step.
    patch_r: i32,
    search: i32,
    step: i32,
    _pad: u32,
    // Noise model of Y at this buffer's scale: σ² = a·Y + b.
    a: f32,
    b: f32,
    // 0..1: luminance strength, detail kept back, chrominance strength.
    luma: f32,
    detail: f32,
    chroma: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<storage, read> p: NrParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> sa: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sb: array<vec4<f32>>;

fn at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
}

// --------------------------------------------------------------- luminance

fn gat(y: f32) -> f32 {
    if p.a <= 1e-9 { return y / sqrt(max(p.b, 1e-12)); }
    return 2.0 / p.a * sqrt(max(p.a * y + 0.375 * p.a * p.a + p.b, 0.0));
}

// Asymptotically unbiased inverse (1/8 in place of the algebraic 3/8).
fn gat_inverse(d: f32) -> f32 {
    if p.a <= 1e-9 { return d * sqrt(max(p.b, 1e-12)); }
    return p.a * d * d * 0.25 - p.a * 0.125 - p.b / p.a;
}

@compute @workgroup_size(256)
fn nr_gat(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    sa[i].x = gat(dot(lin[i].xyz, LUMA));
}

// NLM tiles: 16 × 16 pixels per workgroup, with an apron of the largest
// search + patch radius (7 + 2) loaded once into workgroup memory.
const TILE: i32 = 16;
const APRON: i32 = 9;
const SPAN: i32 = 34;
var<workgroup> tile: array<f32, 1156>;

fn t(x: i32, y: i32) -> f32 {
    return tile[y * SPAN + x];
}

@compute @workgroup_size(16, 16)
fn nr_nlm(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let o = vec2<i32>(wg.xy) * TILE - APRON;
    for (var k = i32(l.y) * TILE + i32(l.x); k < SPAN * SPAN; k += TILE * TILE) {
        tile[k] = sa[at(o.x + k % SPAN, o.y + k / SPAN)].x;
    }
    workgroupBarrier();
    let x = o.x + APRON + i32(l.x);
    let y = o.y + APRON + i32(l.y);
    if x >= i32(p.w) || y >= i32(p.h) { return; }
    let c = vec2<i32>(l.xy) + APRON;
    let area = f32((2 * p.patch_r + 1) * (2 * p.patch_r + 1));
    // Unit noise: two equal patches differ by 2 per pixel on average.
    let h2 = max(p.luma * p.luma, 1e-4);
    var sum = 0.0;
    var total = 0.0;
    var best = 0.0;
    for (var dy = -p.search; dy <= p.search; dy++) {
        for (var dx = -p.search; dx <= p.search; dx++) {
            if dx == 0 && dy == 0 { continue; }
            var d2 = 0.0;
            for (var py = -p.patch_r; py <= p.patch_r; py++) {
                for (var px = -p.patch_r; px <= p.patch_r; px++) {
                    let e = t(c.x + px, c.y + py) - t(c.x + dx + px, c.y + dy + py);
                    d2 += e * e;
                }
            }
            let wgt = exp(-max(d2 / area - 2.0, 0.0) / h2);
            sum += wgt * t(c.x + dx, c.y + dy);
            total += wgt;
            best = max(best, wgt);
        }
    }
    // The pixel itself counts as much as its most similar neighbour.
    best = max(best, 1e-3);
    sb[u32(y) * p.w + u32(x)].x = (sum + best * t(c.x, c.y)) / (total + best);
}

@compute @workgroup_size(256)
fn nr_luma_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                 @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let c = lin[i];
    let y = dot(c.xyz, LUMA);
    let clean = mix(gat_inverse(sb[i].x), y, p.detail);
    lin[i] = vec4<f32>(c.xyz + (clean - y), c.w);
}

// ------------------------------------------------------------- chrominance

@compute @workgroup_size(256)
fn nr_chroma_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                  @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let c = lin[i].xyz;
    let y = dot(c, LUMA);
    sa[i] = vec4<f32>(c.z - y, c.x - y, y, 0.0);
}

// One à-trous level, `sa` → `sb`, at `step`; weights fall with the luminance
// difference relative to the pixel's own, so chroma does not bleed across edges.
@compute @workgroup_size(256)
fn nr_atrous(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    let centre = sa[i];
    var sum = vec2<f32>(0.0);
    var total = 0.0;
    for (var ky = 0; ky < 5; ky++) {
        for (var kx = 0; kx < 5; kx++) {
            let q = sa[at(x + (kx - 2) * p.step, y + (ky - 2) * p.step)];
            let r = (q.z - centre.z) / (abs(centre.z) + 0.02);
            let wgt = B3[kx] * B3[ky] * exp(-r * r * 20.0);
            sum += wgt * q.xy;
            total += wgt;
        }
    }
    sb[i] = vec4<f32>(sum / total, centre.z, 0.0);
}

// `sa` holds the smoothest level: blend the chroma in, keep Y exactly.
@compute @workgroup_size(256)
fn nr_chroma_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                   @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let c = lin[i];
    let y = dot(c.xyz, LUMA);
    let cb = mix(c.z - y, sa[i].x, p.chroma);
    let cr = mix(c.x - y, sa[i].y, p.chroma);
    let r = cr + y;
    let b = cb + y;
    let g = (y - LUMA.x * r - LUMA.z * b) / LUMA.y;
    lin[i] = vec4<f32>(r, g, b, c.w);
}
