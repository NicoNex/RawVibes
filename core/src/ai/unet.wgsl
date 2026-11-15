// UtNet2 layers on planar CHW f32 tensors (channel c of a w×h tensor starts
// at c·w·h). Concat is free: a producer writes at its channel offset of the
// consumer's buffer. Weights: one flat buffer, ONNX layouts.

struct LayerParams {
    // Output size (conv, convT, pool, shuffle).
    w: u32,
    h: u32,
    cin: u32,
    cout: u32,
    // First channel read from `src` / written to `dst`.
    src_c0: u32,
    dst_c0: u32,
    // Kernel size: 3 or 1 (conv), 2 (convT), 0 (no weights: pool, shuffle).
    k: u32,
    // 1: LeakyReLU 0.2 after the bias.
    relu: u32,
    w_off: u32,
    b_off: u32,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var<storage, read> p: LayerParams;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read> wt: array<f32>;

// Output channels per invocation.
const OC: u32 = 8u;
// Input channels per shared-memory chunk, and the 16 × 16 tile with a 1-px apron.
const IC: u32 = 8u;
const SPAN: u32 = 18u;
var<workgroup> tile: array<f32, 2592>;

fn activate(v: f32) -> f32 {
    return select(v, max(v, 0.2 * v), p.relu == 1u);
}

// 3×3 (pad 1) or 1×1 convolution; workgroup z = block of OC output channels.
@compute @workgroup_size(16, 16)
fn conv(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>,
        @builtin(local_invocation_index) li: u32) {
    let x = wg.x * 16u + l.x;
    let y = wg.y * 16u + l.y;
    let inside = x < p.w && y < p.h;
    let oc0 = wg.z * OC;
    let n = p.w * p.h;
    let kk = p.k * p.k;
    let r = p.k / 2u;
    var acc: array<f32, 8>;
    for (var o = 0u; o < OC; o++) {
        if oc0 + o < p.cout { acc[o] = wt[p.b_off + oc0 + o]; }
    }
    for (var ic0 = 0u; ic0 < p.cin; ic0 += IC) {
        for (var t = li; t < IC * SPAN * SPAN; t += 256u) {
            let c = t / (SPAN * SPAN);
            let gx = i32(wg.x * 16u + t % SPAN) - 1;
            let gy = i32(wg.y * 16u + (t / SPAN) % SPAN) - 1;
            var v = 0.0;
            if ic0 + c < p.cin && gx >= 0 && gy >= 0 && gx < i32(p.w) && gy < i32(p.h) {
                v = src[(p.src_c0 + ic0 + c) * n + u32(gy) * p.w + u32(gx)];
            }
            tile[t] = v;
        }
        workgroupBarrier();
        if inside {
            for (var c = 0u; c < IC && ic0 + c < p.cin; c++) {
                for (var ky = 0u; ky < p.k; ky++) {
                    for (var kx = 0u; kx < p.k; kx++) {
                        let v = tile[c * SPAN * SPAN + (l.y + ky + 1u - r) * SPAN + l.x + kx + 1u - r];
                        let base = p.w_off + (ic0 + c) * kk + ky * p.k + kx;
                        for (var o = 0u; o < OC; o++) {
                            if oc0 + o < p.cout { acc[o] += v * wt[base + (oc0 + o) * p.cin * kk]; }
                        }
                    }
                }
            }
        }
        workgroupBarrier();
    }
    if inside {
        for (var o = 0u; o < OC; o++) {
            if oc0 + o < p.cout { dst[(p.dst_c0 + oc0 + o) * n + y * p.w + x] = activate(acc[o]); }
        }
    }
}

// 2×2 stride-2 transposed convolution (ONNX weight [cin, cout, 2, 2]).
@compute @workgroup_size(16, 16)
fn convt(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let x = wg.x * 16u + l.x;
    let y = wg.y * 16u + l.y;
    if x >= p.w || y >= p.h { return; }
    let iw = p.w / 2u;
    let ih = p.h / 2u;
    let at = (y / 2u) * iw + x / 2u;
    let tap = (y % 2u) * 2u + x % 2u;
    let oc0 = wg.z * OC;
    var acc: array<f32, 8>;
    for (var o = 0u; o < OC; o++) {
        if oc0 + o < p.cout { acc[o] = wt[p.b_off + oc0 + o]; }
    }
    for (var ic = 0u; ic < p.cin; ic++) {
        let v = src[(p.src_c0 + ic) * iw * ih + at];
        let base = p.w_off + ic * p.cout * 4u + tap;
        for (var o = 0u; o < OC; o++) {
            if oc0 + o < p.cout { acc[o] += v * wt[base + (oc0 + o) * 4u]; }
        }
    }
    for (var o = 0u; o < OC; o++) {
        if oc0 + o < p.cout { dst[(p.dst_c0 + oc0 + o) * p.w * p.h + y * p.w + x] = acc[o]; }
    }
}

// 2×2 stride-2 max pooling; workgroup z = channel.
@compute @workgroup_size(16, 16)
fn pool(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let x = wg.x * 16u + l.x;
    let y = wg.y * 16u + l.y;
    if x >= p.w || y >= p.h { return; }
    let iw = p.w * 2u;
    let base = (p.src_c0 + wg.z) * iw * p.h * 2u + 2u * y * iw + 2u * x;
    let m = max(max(src[base], src[base + 1u]), max(src[base + iw], src[base + iw + 1u]));
    dst[(p.dst_c0 + wg.z) * p.w * p.h + y * p.w + x] = m;
}

// DepthToSpace, block 2, CRD: out[c, 2y+i, 2x+j] = in[4c + 2i + j, y, x]; z = channel.
@compute @workgroup_size(16, 16)
fn shuffle(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let x = wg.x * 16u + l.x;
    let y = wg.y * 16u + l.y;
    if x >= p.w || y >= p.h { return; }
    let iw = p.w / 2u;
    let ih = p.h / 2u;
    let ic = wg.z * 4u + (y % 2u) * 2u + x % 2u;
    dst[wg.z * p.w * p.h + y * p.w + x] = src[ic * iw * ih + (y / 2u) * iw + x / 2u];
}
