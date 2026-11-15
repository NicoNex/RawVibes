// Defringe: darktable's "chromatic aberrations" module (src/iop/cacorrectrgb.c,
// rawfiner), "darken only" mode, green guide, no refinement. Darken only,
// because the standard mode spreads a purple fringe's colour into the dark
// side next to it; darkening can only take colour away.
// Red and blue are written as a ratio of green; the ratio comes from two
// local "manifolds", averages over the pixels whose green is above (higher)
// or below (lower) its local mean, with log2 ratios. darktable computes them
// at up to a third of the size for speed; here at full size, so a zoomed view
// matches the fit one exactly. Each pixel takes the geometric mix of the two ratios by where its
// green lies between them; a safety blend keeps the input wherever the local
// averages moved.

struct FringeParams {
    w: u32,
    h: u32,
    // Gaussian blur kernel radius and sigma, in this buffer's pixels.
    radius: i32,
    sigma: f32,
    // pow(20, 1 − strength): how much a moved local average falls back to the input.
    safety: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<storage, read> p: FringeParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> sa: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read> third: array<vec4<f32>>;

const MAX_EV_DIFF: f32 = 2.0;

fn at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
}

fn gauss(d: f32) -> f32 {
    return exp(-d * d / (2.0 * p.sigma * p.sigma));
}

@compute @workgroup_size(256)
fn fr_blur_h(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var d = -p.radius; d <= p.radius; d++) {
        let k = gauss(f32(d));
        sum += k * src[at(x + d, y)];
        total += k;
    }
    dst[i] = sum / total;
}

@compute @workgroup_size(256)
fn fr_blur_v(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var d = -p.radius; d <= p.radius; d++) {
        let k = gauss(f32(d));
        sum += k * src[at(x, y + d)];
        total += k;
    }
    dst[i] = sum / total;
}

/// third = blurred input; dst = higher manifold, sa = lower:
/// (log2 r/g, g, log2 b/g, weight), weighted.
@compute @workgroup_size(256)
fn fr_manifolds(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let c = lin[i];
    let g = max(c.y, 1e-6);
    let avg = third[i].y;
    var wh = select(0.0, 1.0, g >= avg);
    var wl = select(0.0, 1.0, g <= avg);
    let ld = log2(max(c.xz, vec2<f32>(1e-6)) / g);
    // Pixels far from green (noise, saturated colour) count less.
    let m = max(abs(ld.x), abs(ld.y));
    if m > MAX_EV_DIFF {
        wh *= MAX_EV_DIFF / m;
        wl *= MAX_EV_DIFF / m;
    }
    dst[i] = vec4<f32>(ld.x, g, ld.y, 1.0) * wh;
    sa[i] = vec4<f32>(ld.x, g, ld.y, 1.0) * wl;
}

/// One blurred manifold normalised by its weight, back to linear values,
/// falling back to the plain blur (`avg`) where it has almost no pixels.
fn normalise(b: vec4<f32>, avg: vec4<f32>) -> vec4<f32> {
    let w = max(b.w, 1e-2);
    let g = b.y / w;
    var m = vec4<f32>(exp2(b.x / w) * g, g, exp2(b.z / w) * g, 0.0);
    if w < 0.05 {
        m = mix(avg, m, (w - 0.01) / (0.05 - 0.01));
    }
    return m;
}

/// dst = blurred higher manifold, sa = blurred lower, third = blurred input:
/// both normalised in place.
@compute @workgroup_size(256)
fn fr_normalise(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let avg = third[i];
    dst[i] = normalise(dst[i], avg);
    sa[i] = normalise(sa[i], avg);
}

/// Red and blue of pixel `i` rebuilt from green and the manifolds (src higher, third lower).
fn corrected(i: u32) -> vec2<f32> {
    let high = src[i];
    let low = third[i];
    let hg = max(high.y, 1e-6);
    let lg = max(low.y, 1e-6);
    let log_high = log2(hg);
    let log_low = log2(lg);
    let dist = log_high - log_low;
    let g = max(lin[i].y, 0.0);
    let log_g = log2(min(max(g, lg), hg));
    var wl = abs(log_high - log_g) / max(dist, 1e-6);
    // Manifolds closer than 0.25 EV give a meaningless position: lean to the middle.
    if dist < 0.25 {
        let k = dist / 0.25;
        wl = wl * k + 0.5 * (1.0 - k);
    }
    let wh = max(1.0 - wl, 0.0);
    let ratio = pow(low.xz / lg, vec2<f32>(wl)) * pow(high.xz / hg, vec2<f32>(wh));
    return min(g * ratio, max(lin[i].xz, vec2<f32>(0.0)));
}

/// sa = (red in, red out, blue in, blue out).
@compute @workgroup_size(256)
fn fr_out(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let o = corrected(i);
    sa[i] = vec4<f32>(lin[i].x, o.x, lin[i].z, o.y);
}

/// sa = blur of `fr_out`'s; keeps the input where the local averages moved.
@compute @workgroup_size(256)
fn fr_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let b = log2(max(sa[i], vec4<f32>(1e-6)));
    let w = exp(-max(abs(b.y - b.x), 0.01) * p.safety) * exp(-max(abs(b.w - b.z), 0.01) * p.safety);
    let c = lin[i];
    let o = corrected(i);
    let rb = (1.0 - w) * max(c.xz, vec2<f32>(0.0)) + w * o;
    lin[i] = vec4<f32>(rb.x, c.y, rb.y, c.w);
}
