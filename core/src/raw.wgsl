// RAW load passes on the CFA mosaic: as-shot white balance and highlight
// reconstruction (inpaint opposed, after darktable 4.x). Work is spread
// over a 2-D grid of 256-thread workgroups, so huge sensors stay under the
// 65535-workgroups-per-dimension limit.

struct RawParams {
    width: u32,
    height: u32,
    // CFA pattern size (2×2 Bayer, 6×6 X-Trans).
    pw: u32,
    ph: u32,
    // Workgroups of the partial-sums pass (read by the reduce pass).
    groups: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    // As-shot WB gains = clip level of each colour after balancing.
    clip: vec4<f32>,
    // Colour (0 R, 1 G, 2 B) of each pattern cell, row-major.
    pattern: array<u32, 36>,
}

@group(0) @binding(0) var<storage, read> p: RawParams;
@group(0) @binding(1) var<storage, read_write> cfa: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@group(0) @binding(3) var<storage, read_write> partials: array<f32>;
@group(0) @binding(4) var<storage, read_write> chroma: array<f32>;

const WG: u32 = 256u;
/// Values at or above this fraction of a colour's clip level are clipped.
const CLIP_AT: f32 = 0.99;

fn index(wg: vec3<u32>, n: vec3<u32>, lid: u32) -> u32 {
    return (wg.y * n.x + wg.x) * WG + lid;
}

fn colour(x: u32, y: u32) -> u32 {
    return p.pattern[(y % p.ph) * p.pw + (x % p.pw)];
}

fn clipped(x: u32, y: u32) -> bool {
    let c = colour(x, y);
    return cfa[y * p.width + x] >= CLIP_AT * p.clip[c];
}

/// Mean of the two colours opposed to the one at (x, y), from its 3×3
/// neighbourhood, averaged in cube-root space as darktable does.
fn opposed(x: u32, y: u32) -> f32 {
    var sum = vec3<f32>(0.0);
    var cnt = vec3<f32>(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let xx = u32(i32(x) + dx);
            let yy = u32(i32(y) + dy);
            let c = colour(xx, yy);
            sum[c] += max(cfa[yy * p.width + xx], 0.0);
            cnt[c] += 1.0;
        }
    }
    let m = pow(sum / max(cnt, vec3<f32>(1.0)), vec3<f32>(1.0 / 3.0));
    let c = colour(x, y);
    var r = 0.0;
    if c == 0u { r = 0.5 * (m.y + m.z); } else if c == 1u { r = 0.5 * (m.x + m.z); } else { r = 0.5 * (m.x + m.y); }
    return r * r * r;
}

fn interior(x: u32, y: u32) -> bool {
    return x >= 2u && y >= 2u && x + 2u < p.width && y + 2u < p.height;
}

@compute @workgroup_size(256)
fn white_balance(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                 @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    cfa[i] = cfa[i] * p.clip[colour(i % p.width, i / p.width)];
}

var<workgroup> acc: array<vec4<f32>, 256>; // sums (rgb), w unused
var<workgroup> cnt: array<vec4<f32>, 256>; // counts (rgb)

/// Chrominance offset (value − opposed mean) on the unclipped border of
/// clipped areas, summed per workgroup.
@compute @workgroup_size(256)
fn hl_partials(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    var s = vec4<f32>(0.0);
    var k = vec4<f32>(0.0);
    if i < p.width * p.height {
        let x = i % p.width;
        let y = i / p.width;
        if interior(x, y) && !clipped(x, y) {
            var near = false;
            for (var dy = -1; dy <= 1; dy++) {
                for (var dx = -1; dx <= 1; dx++) {
                    near = near || clipped(u32(i32(x) + dx), u32(i32(y) + dy));
                }
            }
            if near {
                let c = colour(x, y);
                s[c] = cfa[i] - opposed(x, y);
                k[c] = 1.0;
            }
        }
    }
    acc[lid] = s;
    cnt[lid] = k;
    workgroupBarrier();
    for (var step = WG / 2u; step > 0u; step = step / 2u) {
        if lid < step {
            acc[lid] += acc[lid + step];
            cnt[lid] += cnt[lid + step];
        }
        workgroupBarrier();
    }
    if lid == 0u {
        let g = wg.y * n.x + wg.x;
        for (var c = 0u; c < 3u; c++) {
            partials[g * 6u + c] = acc[0][c];
            partials[g * 6u + 3u + c] = cnt[0][c];
        }
    }
}

/// One workgroup: sums the partials into the mean offset per colour.
@compute @workgroup_size(256)
fn hl_reduce(@builtin(local_invocation_index) lid: u32) {
    var s = vec4<f32>(0.0);
    var k = vec4<f32>(0.0);
    for (var g = lid; g < p.groups; g += WG) {
        for (var c = 0u; c < 3u; c++) {
            s[c] += partials[g * 6u + c];
            k[c] += partials[g * 6u + 3u + c];
        }
    }
    acc[lid] = s;
    cnt[lid] = k;
    workgroupBarrier();
    for (var step = WG / 2u; step > 0u; step = step / 2u) {
        if lid < step {
            acc[lid] += acc[lid + step];
            cnt[lid] += cnt[lid + step];
        }
        workgroupBarrier();
    }
    if lid == 0u {
        for (var c = 0u; c < 3u; c++) {
            chroma[c] = select(0.0, acc[0][c] / cnt[0][c], cnt[0][c] > 0.0);
        }
    }
}

/// Clipped photosites rebuilt from their opposed colours plus the offset;
/// never lowered, and unclipped ones copied as they are.
@compute @workgroup_size(256)
fn hl_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    var v = cfa[i];
    if interior(x, y) && clipped(x, y) {
        v = max(v, opposed(x, y) + chroma[colour(x, y)]);
    }
    out[i] = v;
}
