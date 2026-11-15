// Capture sharpening and defringe on the linear Rec.2020 develop buffer.
// Sharpening: Richardson–Lucy deconvolution of luminance with a Gaussian
// PSF, kept within each pixel's 3×3 range (no halos) and blended in only
// where a detail mask finds edges above the noise. Defringe: purple/green
// hues near luminance edges move towards the chroma of their neighbours.

struct DetailParams {
    w: u32,
    h: u32,
    // Gaussian PSF sigma and its kernel radius, in this buffer's pixels.
    sigma: f32,
    radius: i32,
    // Defringe reach around an edge, in pixels.
    reach: i32,
    _pad: u32,
    // Noise model of Y: σ² = a·Y + b.
    a: f32,
    b: f32,
    defringe: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

@group(0) @binding(0) var<storage, read> p: DetailParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> sa: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sb: array<vec4<f32>>;

fn at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
}

fn luma_at(x: i32, y: i32) -> f32 {
    return dot(lin[at(x, y)].xyz, LUMA);
}

/// Sobel gradient magnitude of Y, normalised to a per-pixel step.
fn gradient(x: i32, y: i32) -> f32 {
    let gx = (luma_at(x + 1, y - 1) + 2.0 * luma_at(x + 1, y) + luma_at(x + 1, y + 1))
        - (luma_at(x - 1, y - 1) + 2.0 * luma_at(x - 1, y) + luma_at(x - 1, y + 1));
    let gy = (luma_at(x - 1, y + 1) + 2.0 * luma_at(x, y + 1) + luma_at(x + 1, y + 1))
        - (luma_at(x - 1, y - 1) + 2.0 * luma_at(x, y - 1) + luma_at(x + 1, y - 1));
    return length(vec2<f32>(gx, gy)) * 0.25;
}

// ---------------------------------------------------------------- sharpen

// sa = (observed Y, estimate, detail mask, 0).
@compute @workgroup_size(256)
fn sh_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    let l = dot(lin[i].xyz, LUMA);
    // A floor keeps smoothstep's edges apart (equal edges are undefined: NaN).
    let sigma = max(sqrt(max(p.a * l + p.b, 0.0)), 1e-6);
    let mask = smoothstep(2.0 * sigma, 4.0 * sigma, gradient(x, y));
    sa[i] = vec4<f32>(l, l, mask, 0.0);
}

fn gauss(d: f32) -> f32 {
    return exp(-d * d / (2.0 * p.sigma * p.sigma));
}

// sb.x = observed / (PSF ⊗ estimate).
@compute @workgroup_size(256)
fn sh_ratio(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = 0.0;
    var total = 0.0;
    for (var dy = -p.radius; dy <= p.radius; dy++) {
        for (var dx = -p.radius; dx <= p.radius; dx++) {
            let k = gauss(f32(dx)) * gauss(f32(dy));
            sum += k * sa[at(x + dx, y + dy)].y;
            total += k;
        }
    }
    sb[i].x = sa[i].x / max(sum / total, 1e-6);
}

// estimate ×= PSF ⊗ ratio (the PSF is symmetric, so it is its own adjoint).
@compute @workgroup_size(256)
fn sh_update(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = 0.0;
    var total = 0.0;
    for (var dy = -p.radius; dy <= p.radius; dy++) {
        for (var dx = -p.radius; dx <= p.radius; dx++) {
            let k = gauss(f32(dx)) * gauss(f32(dy));
            sum += k * sb[at(x + dx, y + dy)].x;
            total += k;
        }
    }
    sa[i].y = sa[i].y * (sum / total);
}

@compute @workgroup_size(256)
fn sh_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var lo = sa[i].x;
    var hi = lo;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let v = sa[at(x + dx, y + dy)].x;
            lo = min(lo, v);
            hi = max(hi, v);
        }
    }
    let l = sa[i].x;
    let sharp = mix(l, clamp(sa[i].y, lo, hi), sa[i].z);
    if l > 1e-6 {
        lin[i] = vec4<f32>(lin[i].xyz * (sharp / l), lin[i].w);
    }
}

// ---------------------------------------------------------------- defringe

const TO_709: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876, 1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083, 1.1187));

/// OKLab hue in degrees (0..360) of a linear Rec.2020 colour.
fn hue(c: vec3<f32>) -> f32 {
    let s = max(TO_709 * c, vec3<f32>(0.0));
    let lms = pow(vec3<f32>(
        dot(s, vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929)),
        dot(s, vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566)),
        dot(s, vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005))), vec3<f32>(1.0 / 3.0));
    let a = dot(lms, vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099));
    let b = dot(lms, vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660));
    let h = degrees(atan2(b, a));
    return select(h, h + 360.0, h < 0.0);
}

/// 1 inside the purple (270–330°) and green (100–160°) fringe bands, fading over 10°.
fn fringe_band(h: f32) -> f32 {
    let purple = smoothstep(260.0, 270.0, h) * (1.0 - smoothstep(330.0, 340.0, h));
    let green = smoothstep(90.0, 100.0, h) * (1.0 - smoothstep(160.0, 170.0, h));
    return max(purple, green);
}

// sa = (Cb, Cr, band weight, edge).
@compute @workgroup_size(256)
fn df_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    let c = lin[i].xyz;
    let l = dot(c, LUMA);
    let chroma = length(vec2<f32>(c.z - l, c.x - l));
    let band = fringe_band(hue(c)) * smoothstep(0.0, 0.02, chroma / (l + 0.02));
    let edge = step(0.25, gradient(x, y) / (l + 0.02));
    sa[i] = vec4<f32>(c.z - l, c.x - l, band, edge);
}

@compute @workgroup_size(256)
fn df_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let own = sa[i];
    if own.z <= 0.0 { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var near = 0.0;
    var sum = vec2<f32>(0.0);
    var total = 0.0;
    for (var dy = -p.reach; dy <= p.reach; dy++) {
        for (var dx = -p.reach; dx <= p.reach; dx++) {
            let q = sa[at(x + dx, y + dy)];
            near = max(near, q.w);
            // The neighbours' colour, fringe pixels left out.
            sum += (1.0 - q.z) * q.xy;
            total += 1.0 - q.z;
        }
    }
    let goal = select(vec2<f32>(0.0), sum / total, total > 1e-3);
    let k = p.defringe * own.z * near;
    if k <= 0.0 { return; }
    let c = lin[i];
    let l = dot(c.xyz, LUMA);
    let ch = mix(own.xy, goal, k);
    let r = ch.y + l;
    let b = ch.x + l;
    lin[i] = vec4<f32>(r, (l - LUMA.x * r - LUMA.z * b) / LUMA.y, b, c.w);
}
