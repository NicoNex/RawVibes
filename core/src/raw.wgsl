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
    // Visible area of the sensor: x, y, width, height.
    crop: vec4<u32>,
    // Mip passes: source width, height; destination width, height.
    size: vec4<u32>,
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

// ------------------------------------------------------------------ RCD
// Ratio Corrected Demosaicing v2.3 (Luis Sanz Rodríguez), after
// RawTherapee's rcd_demosaic.cc, one pass per step. `out` holds the
// reconstructed CFA; R, G and B planes are filled in place.

@group(0) @binding(5) var<storage, read_write> red: array<f32>;
@group(0) @binding(6) var<storage, read_write> green: array<f32>;
@group(0) @binding(7) var<storage, read_write> blue: array<f32>;
@group(0) @binding(8) var<storage, read_write> dir: array<f32>;
@group(0) @binding(9) var<storage, read_write> lpf: array<f32>;
@group(0) @binding(10) var<storage, read_write> level: array<vec2<u32>>;
@group(0) @binding(11) var<storage, read_write> next: array<vec2<u32>>;

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

fn at(x: i32, y: i32) -> f32 {
    return out[u32(y) * p.width + u32(x)];
}

fn plane(c: u32, i: u32) -> f32 {
    if c == 0u { return red[i]; }
    if c == 1u { return green[i]; }
    return blue[i];
}

fn set_plane(c: u32, i: u32, v: f32) {
    if c == 0u { red[i] = v; } else if c == 1u { green[i] = v; } else { blue[i] = v; }
}

fn inside(x: u32, y: u32, margin: u32) -> bool {
    return x >= margin && y >= margin && x + margin < p.width && y + margin < p.height;
}

/// Mean of the photosites of colour `c` in the 3×3 around (x, y), clamped at the edges.
fn bilinear(x: u32, y: u32, c: u32) -> f32 {
    var s = 0.0;
    var k = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let xx = u32(clamp(i32(x) + dx, 0, i32(p.width) - 1));
            let yy = u32(clamp(i32(y) + dy, 0, i32(p.height) - 1));
            if colour(xx, yy) == c {
                s += out[yy * p.width + xx];
                k += 1.0;
            }
        }
    }
    return s / max(k, 1.0);
}

/// Step 0: every channel bilinear (kept on the border), the photosite's own
/// colour exact; and the low-pass filter of step 2.1.
@compute @workgroup_size(256)
fn rcd_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    let own = colour(x, y);
    for (var c = 0u; c < 3u; c++) {
        set_plane(c, i, select(bilinear(x, y, c), out[i], c == own));
    }
    if inside(x, y, 1u) {
        let xi = i32(x);
        let yi = i32(y);
        lpf[i] = at(xi, yi) + 0.5 * (at(xi, yi - 1) + at(xi, yi + 1) + at(xi - 1, yi) + at(xi + 1, yi))
            + 0.25 * (at(xi - 1, yi - 1) + at(xi + 1, yi - 1) + at(xi - 1, yi + 1) + at(xi + 1, yi + 1));
    }
}

fn hpf_v(x: i32, y: i32) -> f32 {
    let v = (at(x, y - 3) - at(x, y - 1) - at(x, y + 1) + at(x, y + 3)) - 3.0 * (at(x, y - 2) + at(x, y + 2)) + 6.0 * at(x, y);
    return v * v;
}

fn hpf_h(x: i32, y: i32) -> f32 {
    let v = (at(x - 3, y) - at(x - 1, y) - at(x + 1, y) + at(x + 3, y)) - 3.0 * (at(x - 2, y) + at(x + 2, y)) + 6.0 * at(x, y);
    return v * v;
}

/// Step 1: vertical/horizontal discrimination.
@compute @workgroup_size(256)
fn rcd_vh(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    if !inside(x, y, 4u) { dir[i] = 0.5; return; }
    let xi = i32(x);
    let yi = i32(y);
    let v = max(EPSSQ, hpf_v(xi, yi - 1) + hpf_v(xi, yi) + hpf_v(xi, yi + 1));
    let h = max(EPSSQ, hpf_h(xi - 1, yi) + hpf_h(xi, yi) + hpf_h(xi + 1, yi));
    dir[i] = v / (v + h);
}

fn lp(x: i32, y: i32) -> f32 {
    return lpf[u32(y) * p.width + u32(x)];
}

fn dir_at(x: i32, y: i32) -> f32 {
    return dir[u32(y) * p.width + u32(x)];
}

/// The central discrimination, or its diagonal neighbours' mean when that is more decided.
fn refined(x: i32, y: i32) -> f32 {
    let c = dir_at(x, y);
    let nb = 0.25 * (dir_at(x - 1, y - 1) + dir_at(x + 1, y - 1) + dir_at(x - 1, y + 1) + dir_at(x + 1, y + 1));
    return select(c, nb, abs(0.5 - c) < abs(0.5 - nb));
}

/// Step 3: green at red and blue photosites.
@compute @workgroup_size(256)
fn rcd_green(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    if !inside(x, y, 4u) || colour(x, y) == 1u { return; }
    let xi = i32(x);
    let yi = i32(y);
    let c0 = at(xi, yi);
    let n_grad = EPS + abs(at(xi, yi - 1) - at(xi, yi + 1)) + abs(c0 - at(xi, yi - 2)) + abs(at(xi, yi - 1) - at(xi, yi - 3)) + abs(at(xi, yi - 2) - at(xi, yi - 4));
    let s_grad = EPS + abs(at(xi, yi - 1) - at(xi, yi + 1)) + abs(c0 - at(xi, yi + 2)) + abs(at(xi, yi + 1) - at(xi, yi + 3)) + abs(at(xi, yi + 2) - at(xi, yi + 4));
    let w_grad = EPS + abs(at(xi - 1, yi) - at(xi + 1, yi)) + abs(c0 - at(xi - 2, yi)) + abs(at(xi - 1, yi) - at(xi - 3, yi)) + abs(at(xi - 2, yi) - at(xi - 4, yi));
    let e_grad = EPS + abs(at(xi - 1, yi) - at(xi + 1, yi)) + abs(c0 - at(xi + 2, yi)) + abs(at(xi + 1, yi) - at(xi + 3, yi)) + abs(at(xi + 2, yi) - at(xi + 4, yi));
    let l0 = lp(xi, yi);
    let n_est = at(xi, yi - 1) * (1.0 + (l0 - lp(xi, yi - 2)) / (EPS + l0 + lp(xi, yi - 2)));
    let s_est = at(xi, yi + 1) * (1.0 + (l0 - lp(xi, yi + 2)) / (EPS + l0 + lp(xi, yi + 2)));
    let w_est = at(xi - 1, yi) * (1.0 + (l0 - lp(xi - 2, yi)) / (EPS + l0 + lp(xi - 2, yi)));
    let e_est = at(xi + 1, yi) * (1.0 + (l0 - lp(xi + 2, yi)) / (EPS + l0 + lp(xi + 2, yi)));
    let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
    let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
    let d = refined(xi, yi);
    green[i] = d * h_est + (1.0 - d) * v_est;
}

fn hpf_p(x: i32, y: i32) -> f32 {
    let v = (at(x - 3, y - 3) - at(x - 1, y - 1) - at(x + 1, y + 1) + at(x + 3, y + 3)) - 3.0 * (at(x - 2, y - 2) + at(x + 2, y + 2)) + 6.0 * at(x, y);
    return v * v;
}

fn hpf_q(x: i32, y: i32) -> f32 {
    let v = (at(x + 3, y - 3) - at(x + 1, y - 1) - at(x - 1, y + 1) + at(x - 3, y + 3)) - 3.0 * (at(x + 2, y - 2) + at(x - 2, y + 2)) + 6.0 * at(x, y);
    return v * v;
}

/// Step 4.1: P/Q diagonal discrimination (into `lpf`, no longer needed).
@compute @workgroup_size(256)
fn rcd_pq(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    if !inside(x, y, 4u) { lpf[i] = 0.5; return; }
    let xi = i32(x);
    let yi = i32(y);
    let ps = max(EPSSQ, hpf_p(xi - 1, yi - 1) + hpf_p(xi, yi) + hpf_p(xi + 1, yi + 1));
    let qs = max(EPSSQ, hpf_q(xi + 1, yi - 1) + hpf_q(xi, yi) + hpf_q(xi - 1, yi + 1));
    lpf[i] = ps / (ps + qs);
}

fn pq_refined(x: i32, y: i32) -> f32 {
    let c = lp(x, y);
    let nb = 0.25 * (lp(x - 1, y - 1) + lp(x + 1, y - 1) + lp(x - 1, y + 1) + lp(x + 1, y + 1));
    return select(c, nb, abs(0.5 - c) < abs(0.5 - nb));
}

fn px(c: u32, x: i32, y: i32) -> f32 {
    return plane(c, u32(y) * p.width + u32(x));
}

/// Step 4.2: red at blue photosites and blue at red ones.
@compute @workgroup_size(256)
fn rcd_rb(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    let own = colour(x, y);
    if !inside(x, y, 4u) || own == 1u { return; }
    let c = 2u - own;
    let xi = i32(x);
    let yi = i32(y);
    let g0 = px(1u, xi, yi);
    let nw = EPS + abs(px(c, xi - 1, yi - 1) - px(c, xi + 1, yi + 1)) + abs(px(c, xi - 1, yi - 1) - px(c, xi - 3, yi - 3)) + abs(g0 - px(1u, xi - 2, yi - 2));
    let ne = EPS + abs(px(c, xi + 1, yi - 1) - px(c, xi - 1, yi + 1)) + abs(px(c, xi + 1, yi - 1) - px(c, xi + 3, yi - 3)) + abs(g0 - px(1u, xi + 2, yi - 2));
    let sw = EPS + abs(px(c, xi + 1, yi - 1) - px(c, xi - 1, yi + 1)) + abs(px(c, xi - 1, yi + 1) - px(c, xi - 3, yi + 3)) + abs(g0 - px(1u, xi - 2, yi + 2));
    let se = EPS + abs(px(c, xi - 1, yi - 1) - px(c, xi + 1, yi + 1)) + abs(px(c, xi + 1, yi + 1) - px(c, xi + 3, yi + 3)) + abs(g0 - px(1u, xi + 2, yi + 2));
    let nw_est = px(c, xi - 1, yi - 1) - px(1u, xi - 1, yi - 1);
    let ne_est = px(c, xi + 1, yi - 1) - px(1u, xi + 1, yi - 1);
    let sw_est = px(c, xi - 1, yi + 1) - px(1u, xi - 1, yi + 1);
    let se_est = px(c, xi + 1, yi + 1) - px(1u, xi + 1, yi + 1);
    let p_est = (nw * se_est + se * nw_est) / (nw + se);
    let q_est = (ne * sw_est + sw * ne_est) / (ne + sw);
    let d = pq_refined(xi, yi);
    set_plane(c, i, g0 + d * q_est + (1.0 - d) * p_est);
}

/// Step 4.3: red and blue at green photosites.
@compute @workgroup_size(256)
fn rcd_rb_green(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
                @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    let x = i % p.width;
    let y = i / p.width;
    if !inside(x, y, 4u) || colour(x, y) != 1u { return; }
    let xi = i32(x);
    let yi = i32(y);
    let d = refined(xi, yi);
    let g0 = px(1u, xi, yi);
    let n1 = px(1u, xi, yi - 1);
    let s1 = px(1u, xi, yi + 1);
    let w1 = px(1u, xi - 1, yi);
    let e1 = px(1u, xi + 1, yi);
    for (var k = 0u; k < 2u; k++) {
        let c = k * 2u;
        let sn = abs(px(c, xi, yi - 1) - px(c, xi, yi + 1));
        let ew = abs(px(c, xi - 1, yi) - px(c, xi + 1, yi));
        let n_grad = EPS + abs(g0 - px(1u, xi, yi - 2)) + sn + abs(px(c, xi, yi - 1) - px(c, xi, yi - 3));
        let s_grad = EPS + abs(g0 - px(1u, xi, yi + 2)) + sn + abs(px(c, xi, yi + 1) - px(c, xi, yi + 3));
        let w_grad = EPS + abs(g0 - px(1u, xi - 2, yi)) + ew + abs(px(c, xi - 1, yi) - px(c, xi - 3, yi));
        let e_grad = EPS + abs(g0 - px(1u, xi + 2, yi)) + ew + abs(px(c, xi + 1, yi) - px(c, xi + 3, yi));
        let n_est = px(c, xi, yi - 1) - n1;
        let s_est = px(c, xi, yi + 1) - s1;
        let w_est = px(c, xi - 1, yi) - w1;
        let e_est = px(c, xi + 1, yi) - e1;
        let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
        let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
        set_plane(c, i, g0 + d * h_est + (1.0 - d) * v_est);
    }
}

fn pack(c: vec3<f32>) -> vec2<u32> {
    let v = max(c, vec3<f32>(0.0));
    return vec2<u32>(pack2x16float(v.xy), pack2x16float(vec2<f32>(v.z, 0.0)));
}

fn unpack(v: vec2<u32>) -> vec3<f32> {
    return vec3<f32>(unpack2x16float(v.x), unpack2x16float(v.y).x);
}

/// The visible area of the RGB planes into mip level 0 (f16).
@compute @workgroup_size(256)
fn rcd_pack(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.crop.z * p.crop.w { return; }
    let s = (p.crop.y + i / p.crop.z) * p.width + p.crop.x + i % p.crop.z;
    level[i] = pack(vec3<f32>(red[s], green[s], blue[s]));
}

/// Interleaved camera RGB (rawler-demosaiced) into mip level 0.
@compute @workgroup_size(256)
fn rgb_pack(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.width * p.height { return; }
    level[i] = pack(vec3<f32>(cfa[i * 3u], cfa[i * 3u + 1u], cfa[i * 3u + 2u]));
}

/// The next mip level: 2×2 box average (edge pixels repeat on odd sizes).
@compute @workgroup_size(256)
fn mip(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
       @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.size.z * p.size.w { return; }
    let x = (i % p.size.z) * 2u;
    let y = (i / p.size.z) * 2u;
    let x1 = min(x + 1u, p.size.x - 1u);
    let y1 = min(y + 1u, p.size.y - 1u);
    let s = unpack(level[y * p.size.x + x]) + unpack(level[y * p.size.x + x1])
        + unpack(level[y1 * p.size.x + x]) + unpack(level[y1 * p.size.x + x1]);
    next[i] = pack(0.25 * s);
}
