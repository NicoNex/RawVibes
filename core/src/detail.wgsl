// Capture and output sharpening on the linear Rec.2020 develop buffer.
// Sharpening: Richardson–Lucy deconvolution of luminance with a Gaussian
// PSF, kept within each pixel's 3×3 range (no halos) and blended in only
// where a detail mask finds edges above the noise.
// Output sharpening: unsharp mask of perceptual lightness L = Y^(1/3) (as
// darktable's sharpen works on Lab L), with a soft threshold for fine noise.

struct DetailParams {
    w: u32,
    h: u32,
    // Gaussian PSF sigma and its kernel radius, in this buffer's pixels.
    sigma: f32,
    radius: i32,
    // Noise model of Y: σ² = a·Y + b.
    a: f32,
    b: f32,
    // Output sharpening: gain on L − blur(L); threshold on |L − blur(L)|.
    usm: f32,
    usm_threshold: f32,
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

// ----------------------------------------------------- output sharpening

// sa.x = L; sb.x = L blurred along x; sa.y = blurred along both.
@compute @workgroup_size(256)
fn us_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    sa[i].x = pow(max(dot(lin[i].xyz, LUMA), 0.0), 1.0 / 3.0);
}

@compute @workgroup_size(256)
fn us_blur_h(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = 0.0;
    var total = 0.0;
    for (var d = -p.radius; d <= p.radius; d++) {
        let k = gauss(f32(d));
        sum += k * sa[at(x + d, y)].x;
        total += k;
    }
    sb[i].x = sum / total;
}

@compute @workgroup_size(256)
fn us_blur_v(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = 0.0;
    var total = 0.0;
    for (var d = -p.radius; d <= p.radius; d++) {
        let k = gauss(f32(d));
        sum += k * sb[at(x, y + d)].x;
        total += k;
    }
    sa[i].y = sum / total;
}

@compute @workgroup_size(256)
fn us_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let l = sa[i].x;
    let d = l - sa[i].y;
    // Below the threshold the detail is taken for noise and left alone.
    let keep = select(1.0, smoothstep(p.usm_threshold, 2.0 * p.usm_threshold, abs(d)), p.usm_threshold > 0.0);
    let sharp = max(l + p.usm * d * keep, 0.0);
    if l > 1e-6 {
        let r = sharp / l;
        lin[i] = vec4<f32>(lin[i].xyz * (r * r * r), lin[i].w);
    }
}
