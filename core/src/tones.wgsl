// Edge-aware shadows and highlights on the linear Rec.2020 develop buffer:
// darktable's tone equalizer (src/iop/toneequal.c) with its EIGF mask
// (src/common/eigf.h, no quantisation, linear blending). See tones.rs.
//
// `sa.x` holds the full-resolution mask; `sb` the downscaled guided filter
// statistics: (mean, mean²) at [0, dn), the horizontal blur at [dn, 2 dn).

struct TonesParams {
    w: u32,
    h: u32,
    dw: u32,
    dh: u32,
    // Integer downscale of the statistics; Gaussian radius (taps) and σ there.
    f: u32,
    radius: i32,
    sigma: f32,
    feather: f32,
    // Linear mask exposure compensation.
    boost: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    // RBF weights of the gain curve.
    factors: array<vec4<f32>, 2>,
}

@group(0) @binding(0) var<storage, read> p: TonesParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> sa: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sb: array<vec4<f32>>;

const MIN_FLOAT: f32 = 1.52587890625e-5; // 2^-16

fn ds_at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.dh) - 1)) * p.dw + u32(clamp(x, 0, i32(p.dw) - 1));
}

// Mask: the RGB euclidean norm, exposure-compensated (darktable's NORM_2).
@compute @workgroup_size(256)
fn te_mask(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    sa[i].x = max(p.boost * length(lin[i].xyz), MIN_FLOAT);
}

// The mask averaged over f × f blocks, with its square.
@compute @workgroup_size(256)
fn te_down(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.dw * p.dh { return; }
    let x0 = (i % p.dw) * p.f;
    let y0 = (i / p.dw) * p.f;
    var s = 0.0;
    for (var y = y0; y < y0 + p.f; y++) {
        for (var x = x0; x < x0 + p.f; x++) {
            s += sa[min(y, p.h - 1u) * p.w + min(x, p.w - 1u)].x;
        }
    }
    let g = s / f32(p.f * p.f);
    sb[i] = vec4<f32>(g, g * g, 0.0, 0.0);
}

fn gauss(k: i32) -> f32 {
    return exp(-f32(k * k) / (2.0 * p.sigma * p.sigma));
}

@compute @workgroup_size(256)
fn te_blur_h(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    let dn = p.dw * p.dh;
    if i >= dn { return; }
    let x = i32(i % p.dw);
    let y = i32(i / p.dw);
    var s = vec2<f32>(0.0);
    var wsum = 0.0;
    for (var k = -p.radius; k <= p.radius; k++) {
        let w = gauss(k);
        s += w * sb[ds_at(x + k, y)].xy;
        wsum += w;
    }
    sb[dn + i] = vec4<f32>(s / wsum, 0.0, 0.0);
}

// Vertical pass, then (mean, variance).
@compute @workgroup_size(256)
fn te_blur_v(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    let dn = p.dw * p.dh;
    if i >= dn { return; }
    let x = i32(i % p.dw);
    let y = i32(i / p.dw);
    var s = vec2<f32>(0.0);
    var wsum = 0.0;
    for (var k = -p.radius; k <= p.radius; k++) {
        let w = gauss(k);
        s += w * sb[dn + ds_at(x, y + k)].xy;
        wsum += w;
    }
    let m = s / wsum;
    sb[i] = vec4<f32>(m.x, m.y - m.x * m.x, 0.0, 0.0);
}

// EIGF blending (eigf_blending_no_mask, linear) with the statistics upsampled bilinearly.
@compute @workgroup_size(256)
fn te_blend(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let u = (vec2<f32>(f32(i % p.w), f32(i / p.w)) + 0.5) / f32(p.f) - 0.5;
    let c = floor(u);
    let t = u - c;
    let x = i32(c.x);
    let y = i32(c.y);
    let av = mix(
        mix(sb[ds_at(x, y)].xy, sb[ds_at(x + 1, y)].xy, t.x),
        mix(sb[ds_at(x, y + 1)].xy, sb[ds_at(x + 1, y + 1)].xy, t.x),
        t.y,
    );
    let image = sa[i].x;
    let nv = av.y / max(av.x * image, 1e-6);
    let a = nv / (nv + p.feather);
    let b = av.x - a * av.x;
    sa[i].x = max(image * a + b, MIN_FLOAT);
}

// The gain of the mask's exposure (apply_toneequalizer, interpolated in EV).
@compute @workgroup_size(256)
fn te_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let ev = clamp(log2(sa[i].x), -8.0, 0.0);
    var g = 0.0;
    for (var j = 0; j < 8; j++) {
        let d = ev - (-8.0 + 8.0 * f32(j) / 7.0);
        g += exp(-d * d / 4.0) * p.factors[j / 4][j % 4];
    }
    // Weights are in EV (tones.rs); darktable's ±2 EV limit.
    lin[i] = vec4<f32>(lin[i].xyz * exp2(clamp(g, -2.0, 2.0)), lin[i].w);
}
