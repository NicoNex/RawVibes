// Markesteijn 3-pass X-Trans demosaic: a port of darktable's OpenCL kernels
// (data/kernels/demosaic_markesteijn.cl, Ulrich Pegelow, after Frank
// Markesteijn's dcraw code). It runs in square tiles with an 18-px border all
// round, every tile aligned to the 6×6 pattern, so the pattern phase, `allhex`
// and the solitary green offset are the sensor's. Sensor edges read the mosaic
// repeated with the same phase. The per-direction homogeneity kernels
// (threshold, set, sum, max, quench, accumulate, final) are fused into four
// passes; their arithmetic is darktable's.

struct XtParams {
    // Whole mosaic, padded tile, visible area (x, y, width, height).
    fw: u32,
    fh: u32,
    tw: u32,
    th: u32,
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
    // Border the kernel leaves alone; direction index; solitary-green step and colour flip.
    border: i32,
    d: i32,
    dx: i32,
    dy: i32,
    hcomp: i32,
    // Offset of the solitary green pixels (mod 3); the first RGB plane of this pass.
    sgx: i32,
    sgy: i32,
    base: u32,
    pattern: array<u32, 36>,
    // For each (y mod 3, x mod 3): eight (x, y) offsets of the green hexagon.
    allhex: array<i32, 144>,
}

struct Tile {
    // Mosaic coordinates of the tile's pixel (0, 0).
    ox: i32,
    oy: i32,
}

@group(0) @binding(0) var<storage, read> p: XtParams;
@group(0) @binding(1) var<storage, read> t: Tile;
@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> rgb: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> gmm: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> aux: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> drv: array<f32>;
@group(0) @binding(7) var<storage, read_write> homo: array<u32>;
@group(0) @binding(8) var<storage, read_write> out: array<f32>;

// Tile border: darktable's 3-pass `pad_tile` (17), rounded up to the pattern.
const PAD: i32 = 18;
const FLT_MAX: f32 = 3.402823e38;

fn n() -> u32 {
    return p.tw * p.th;
}

fn ix(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.th) - 1)) * p.tw + u32(clamp(x, 0, i32(p.tw) - 1));
}

fn fc(x: i32, y: i32) -> i32 {
    return i32(p.pattern[u32(((y % 6) + 6) % 6) * 6u + u32(((x % 6) + 6) % 6)]);
}

fn hex(x: i32, y: i32, c: i32) -> vec2<i32> {
    let k = ((y % 3) * 3 + x % 3) * 8 + c;
    return vec2<i32>(p.allhex[2 * k], p.allhex[2 * k + 1]);
}

// RGB plane `plane` of the tile at (x, y) + o.
fn px(plane: u32, x: i32, y: i32, o: vec2<i32>) -> vec4<f32> {
    return rgb[plane * n() + ix(x + o.x, y + o.y)];
}

fn sq(v: f32) -> f32 {
    return v * v;
}

// The tile item (x, y), or a negative x when it is out of the tile or in `border`.
fn item(i: u32, border: i32) -> vec2<i32> {
    let x = i32(i % p.tw);
    let y = i32(i / p.tw);
    if i >= n() || x < border || x >= i32(p.tw) - border || y < border || y >= i32(p.th) - border {
        return vec2<i32>(-1, 0);
    }
    return vec2<i32>(x, y);
}

// A mosaic coordinate moved inside [0, size) in whole pattern periods.
fn inside(v: i32, size: i32) -> i32 {
    var r = v;
    if r < 0 { r += 6 * ((5 - r) / 6); }
    if r >= size { r -= 6 * ((r - size) / 6 + 1); }
    return r;
}

// The mosaic into RGB planes 0–3 (markesteijn_initial_copy and its three copies).
@compute @workgroup_size(256)
fn xt_init(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 0);
    if q.x < 0 { return; }
    let sx = inside(t.ox + q.x, i32(p.fw));
    let sy = inside(t.oy + q.y, i32(p.fh));
    var c = vec4<f32>(0.0);
    c[fc(q.x, q.y)] = max(src[u32(sy) * p.fw + u32(sx)], 0.0);
    let i = ix(q.x, q.y);
    for (var k = 0u; k < 4u; k++) { rgb[k * n() + i] = c; }
}

// Minimum and maximum allowed green of each red/blue pixel pair (markesteijn_green_minmax).
@compute @workgroup_size(256)
fn xt_minmax(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
             @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 3);
    if q.x < 0 || fc(q.x, q.y) == 1 { return; }
    var lo = FLT_MAX;
    var hi = 0.0;
    for (var c = 0; c < 6; c++) {
        let v = px(0u, q.x, q.y, hex(q.x, q.y, c)).y;
        lo = min(lo, v);
        hi = max(hi, v);
    }
    // The one neighbouring non-green pixel of the four (the last one found, as darktable).
    var nb = vec2<i32>(0, 0);
    let around = array<vec2<i32>, 4>(vec2<i32>(1, 0), vec2<i32>(0, 1), vec2<i32>(-1, 0), vec2<i32>(0, -1));
    for (var k = 0; k < 4; k++) {
        if fc(q.x + around[k].x, q.y + around[k].y) != 1 { nb = around[k]; }
    }
    for (var c = 0; c < 6; c++) {
        let v = px(0u, q.x + nb.x, q.y + nb.y, hex(q.x + nb.x, q.y + nb.y, c)).y;
        lo = min(lo, v);
        hi = max(hi, v);
    }
    gmm[ix(q.x, q.y)] = vec2<f32>(lo, hi);
}

// Green horizontally, vertically and along both diagonals (markesteijn_interpolate_green).
@compute @workgroup_size(256)
fn xt_green(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 3);
    if q.x < 0 { return; }
    let f = fc(q.x, q.y);
    if f == 1 { return; }
    let x = q.x;
    let y = q.y;
    let m = gmm[ix(x, y)];
    let h0 = hex(x, y, 0);
    let h1 = hex(x, y, 1);
    let h2 = hex(x, y, 2);
    let h3 = hex(x, y, 3);
    let me = px(0u, x, y, vec2<i32>(0))[f];
    var color: array<f32, 4>;
    color[0] = 0.6796875 * (px(0u, x, y, h1).y + px(0u, x, y, h0).y)
        - 0.1796875 * (px(0u, x, y, 2 * h1).y + px(0u, x, y, 2 * h0).y);
    color[1] = 0.87109375 * px(0u, x, y, h3).y + 0.13 * px(0u, x, y, h2).y
        + 0.359375 * (me - px(0u, x, y, -h2)[f]);
    for (var c = 0; c < 2; c++) {
        let h = hex(x, y, 4 + c);
        color[2 + c] = 0.640625 * px(0u, x, y, h).y + 0.359375 * px(0u, x, y, -2 * h).y
            + 0.12890625 * (2.0 * me - px(0u, x, y, 3 * h)[f] - px(0u, x, y, -3 * h)[f]);
    }
    let d = select(0u, 1u, (y - p.sgy) % 3 == 0);
    let i = ix(x, y);
    for (var c = 0u; c < 4u; c++) {
        rgb[(c ^ d) * n() + i].y = clamp(color[c], m.x, m.y);
    }
}

// Green again from closer pixels, passes 2 and 3 (markesteijn_recalculate_green).
@compute @workgroup_size(256)
fn xt_regreen(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
              @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 6);
    if q.x < 0 { return; }
    let f = fc(q.x, q.y);
    if f == 1 { return; }
    let x = q.x;
    let y = q.y;
    let m = gmm[ix(x, y)];
    let flip = select(0, 1, (y - p.sgy) % 3 == 0);
    for (var d = 3; d < 6; d++) {
        let plane = p.base + u32((d - 2) ^ flip);
        let h = hex(x, y, d);
        let a = px(plane, x, y, -2 * h);
        let b = px(plane, x, y, h);
        let val = a.y + 2.0 * b.y - a[f] - 2.0 * b[f] + 3.0 * px(plane, x, y, vec2<i32>(0))[f];
        rgb[plane * n() + ix(x, y)].y = clamp(val / 3.0, m.x, m.y);
    }
}

// Red and blue of the solitary green pixels, one of six runs (markesteijn_solitary_green).
@compute @workgroup_size(256)
fn xt_solitary(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 5);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    if (x - p.sgx) % 3 != 0 || (y - p.sgy) % 3 != 0 { return; }
    var h = fc(x + 1, y) ^ p.hcomp;
    let i = ix(x, y);
    var color = vec4<f32>(0.0);
    var old = vec4<f32>(0.0);
    if p.d > 0 {
        color = aux[i];
        old = color;
    }
    let me = px(p.base, x, y, vec2<i32>(0));
    var diff = 0.0;
    for (var c = 0; c < 2; c++) {
        let off = vec2<i32>(p.dx, p.dy) << vec2<u32>(u32(c));
        let a = px(p.base, x, y, off);
        let b = px(p.base, x, y, -off);
        let g = 2.0 * me.y - a.y - b.y;
        color[h] = g + a[h] + b[h];
        if p.d > 1 {
            diff += sq(a.y - b.y - a[h] + b[h]) + sq(g);
        }
        h ^= 2;
    }
    color.w = diff;
    if p.d > 1 && (p.d & 1) == 1 {
        for (var c = 0; c < 2; c++) {
            color[c * 2] = select(color[c * 2], old[c * 2], old.w < diff);
        }
    }
    if p.d < 2 || (p.d & 1) == 1 {
        let k = p.base * n() + i;
        rgb[k] = vec4<f32>(color.x / 2.0, rgb[k].y, color.z / 2.0, rgb[k].w);
    }
    aux[i] = color;
}

// Red for blue pixels and vice versa (markesteijn_red_and_blue).
@compute @workgroup_size(256)
fn xt_redblue(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
              @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 5);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    let f = 2 - fc(x, y);
    if f == 1 { return; }
    let plane = p.base;
    let horiz = (y - p.sgy) % 3 == 0;
    let c = select(vec2<i32>(0, 1), vec2<i32>(1, 0), horiz);
    let h = select(vec2<i32>(3, 0), vec2<i32>(0, 3), horiz);
    let me = px(plane, x, y, vec2<i32>(0)).y;
    let by_c = abs(me - px(plane, x, y, c).y) + abs(me - px(plane, x, y, -c).y);
    let by_h = abs(me - px(plane, x, y, h).y) + abs(me - px(plane, x, y, -h).y);
    let use_c = p.d > 1 || (p.d == 0 && horiz) || (p.d == 1 && !horiz) || by_c < 2.0 * by_h;
    let off = select(h, c, use_c);
    let a = px(plane, x, y, off);
    let b = px(plane, x, y, -off);
    rgb[plane * n() + ix(x, y)][f] = (a[f] + b[f] + 2.0 * me - a.y - b.y) / 2.0;
}

// Red and blue for the 2×2 blocks of green (markesteijn_interpolate_twoxtwo).
@compute @workgroup_size(256)
fn xt_twoxtwo(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
              @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 4);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    if (y - p.sgy) % 3 == 0 || (x - p.sgx) % 3 == 0 { return; }
    let plane = p.base;
    let o = hex(x, y, p.d);
    let o1 = hex(x, y, p.d + 1);
    let far = any(o + o1 != vec2<i32>(0));
    // (m0, m1, m2, m3) and the divisor, as darktable's m[5].
    let m = select(vec4<f32>(2.0, -1.0, -1.0, 1.0), vec4<f32>(3.0, -2.0, -1.0, 2.0), far);
    let div = select(0.5, 1.0 / 3.0, far);
    let a = px(plane, x, y, o);
    let b = px(plane, x, y, o1);
    let g = m.x * px(plane, x, y, vec2<i32>(0)).y + m.y * a.y + m.z * b.y;
    let k = plane * n() + ix(x, y);
    rgb[k] = vec4<f32>((g + m.w * a.x + b.x) * div, rgb[k].y, (g + m.w * a.z + b.z) * div, rgb[k].w);
}

fn ypbpr(c: vec4<f32>) -> vec3<f32> {
    let y = 0.2627 * c.x + 0.6780 * c.y + 0.0593 * c.z;
    return vec3<f32>(y, (c.z - y) * 0.56433, (c.x - y) * 0.67815);
}

// Perceptual derivative of every direction's result along its own axis, and their
// minimum as the homogeneity threshold (convert_yuv, differentiate, homo_threshold).
@compute @workgroup_size(256)
fn xt_drv(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 13);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    let axes = array<vec2<i32>, 4>(vec2<i32>(1, 0), vec2<i32>(0, 1), vec2<i32>(1, 1), vec2<i32>(-1, 1));
    var tr = FLT_MAX;
    for (var d = 0u; d < 8u; d++) {
        let o = axes[d & 3u];
        let e = 2.0 * ypbpr(px(d, x, y, vec2<i32>(0))) - ypbpr(px(d, x, y, o)) - ypbpr(px(d, x, y, -o));
        let v = dot(e, e);
        drv[d * n() + ix(x, y)] = v;
        tr = min(tr, v);
    }
    aux[ix(x, y)].x = tr;
}

// Homogeneity maps: neighbours within 8× the threshold (markesteijn_homo_set).
@compute @workgroup_size(256)
fn xt_homo(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), 15);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    let tr = max(8.0 * aux[ix(x, y)].x, 1e-10);
    for (var d = 0u; d < 8u; d++) {
        var count = 0u;
        for (var v = -1; v <= 1; v++) {
            for (var h = -1; h <= 1; h++) {
                count += select(0u, 1u, drv[d * n() + ix(x + h, y + v)] <= tr);
            }
        }
        homo[d * n() + ix(x, y)] = count;
    }
}

// 5×5 sums of the maps, the best direction(s), their average into the visible
// area (homo_sum, homo_max, homo_max_corr, homo_quench, accu, final).
@compute @workgroup_size(256)
fn xt_final(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let q = item(index(wg, nw, lid), PAD);
    if q.x < 0 { return; }
    let x = q.x;
    let y = q.y;
    let sx = t.ox + x - i32(p.cx);
    let sy = t.oy + y - i32(p.cy);
    if sx < 0 || sy < 0 || sx >= i32(p.cw) || sy >= i32(p.ch) { return; }
    var sum: array<u32, 8>;
    var best = 0u;
    for (var d = 0u; d < 8u; d++) {
        var s = 0u;
        for (var v = -2; v <= 2; v++) {
            for (var h = -2; h <= 2; h++) { s += homo[d * n() + ix(x + h, y + v)]; }
        }
        sum[d] = s;
        best = max(best, s);
    }
    best -= best >> 3u;
    for (var d = 0u; d < 4u; d++) {
        let a = sum[d];
        let b = sum[d + 4u];
        if a < b { sum[d] = 0u; }
        if a > b { sum[d + 4u] = 0u; }
    }
    var acc = vec4<f32>(0.0);
    for (var d = 0u; d < 8u; d++) {
        if sum[d] >= best { acc += vec4<f32>(px(d, x, y, vec2<i32>(0)).xyz, 1.0); }
    }
    let c = max(select(vec3<f32>(0.0), acc.xyz / acc.w, acc.w > 0.0), vec3<f32>(0.0));
    let o = 3u * (u32(sy) * p.cw + u32(sx));
    out[o] = c.x;
    out[o + 1u] = c.y;
    out[o + 2u] = c.z;
}

// Planes 0–3 into 4–7 before the second pass (a buffer cannot copy onto itself).
@compute @workgroup_size(256)
fn xt_copy(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, nw, lid);
    if i >= 4u * n() { return; }
    rgb[4u * n() + i] = rgb[i];
}
