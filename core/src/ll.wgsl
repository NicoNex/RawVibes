// Local contrast: darktable's local Laplacian filter (Paris et al. 2011, in
// the fast form of Aubry et al. 2014; darktable src/common/locallaplacian.c).
// Works on L = L*/100 of Y. Eight remapped copies of L (curve around the
// gammas (k + ½)/8) get Gaussian pyramids; each output Laplacian coefficient
// is interpolated between the two copies whose gamma brackets the input's
// own Gaussian value there. The curve adds detail only to differences
// smaller than about σ, so strong edges pass unchanged (no halos).
// The scene-linear colour is scaled by the change of Y.
//
// Buffers: `g` holds the input Gaussian pyramid and the output pyramid (f32
// levels at offsets), `q` the remapped pyramids from level 1 (two vec4 per
// pixel: gammas 0–3 and 4–7). Level 0 of the remapped copies is computed on
// the fly from the input.

struct LlParams {
    // Fine and coarse level sizes.
    fw: u32,
    fh: u32,
    cw: u32,
    ch: u32,
    // Offsets (in pixels) of the input pyramid, output pyramid and remapped
    // pyramids, at the fine and the coarse level.
    g_f: u32,
    g_c: u32,
    o_f: u32,
    o_c: u32,
    q_f: u32,
    q_c: u32,
    // The fine level is level 0.
    base: u32,
    _pad: u32,
    // Curve: midtone range and the detail added.
    sigma: f32,
    clarity: f32,
    _pad2: vec2<f32>,
}

@group(0) @binding(0) var<storage, read> p: LlParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> g: array<f32>;
@group(0) @binding(3) var<storage, read_write> q: array<vec4<f32>>;

const WG: u32 = 256u;
const LUMA: vec3<f32> = vec3<f32>(0.2627, 0.6780, 0.0593);
const K: array<f32, 5> = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);
const GAMMAS: f32 = 8.0;

fn index(wg: vec3<u32>, n: vec3<u32>, lid: u32) -> u32 {
    return (wg.y * n.x + wg.x) * WG + lid;
}

fn lstar(y: f32) -> f32 {
    return select(9.033 * y, 1.16 * pow(max(y, 0.0), 1.0 / 3.0) - 0.16, y > 0.008856);
}

fn unlstar(l: f32) -> f32 {
    let t = (l + 0.16) / 1.16;
    return select(l / 9.033, t * t * t, l > 0.08);
}

// darktable's curve_scalar with shadows = highlights = 1: identity plus the
// midtone detail term.
fn curve(x: f32, gamma: f32) -> f32 {
    let c = x - gamma;
    return x + p.clarity * c * exp(-c * c / (2.0 * p.sigma * p.sigma / 3.0));
}

fn remap(x: f32) -> array<vec4<f32>, 2> {
    var r: array<vec4<f32>, 2>;
    for (var k = 0; k < 8; k++) {
        r[k / 4][k % 4] = curve(x, (f32(k) + 0.5) / GAMMAS);
    }
    return r;
}

fn fine_at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.fh) - 1)) * p.fw + u32(clamp(x, 0, i32(p.fw) - 1));
}

fn coarse_at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.ch) - 1)) * p.cw + u32(clamp(x, 0, i32(p.cw) - 1));
}

struct Taps {
    at: array<u32, 9>,
    w: array<f32, 9>,
}

// Taps of the 2× expand of a coarse level at fine (x, y).
fn expand_taps(x: i32, y: i32) -> Taps {
    var t: Taps;
    var n = 0;
    for (var j = y / 2 - 1; j <= y / 2 + 1; j++) {
        for (var i = x / 2 - 1; i <= x / 2 + 1; i++) {
            let dx = x - 2 * i;
            let dy = y - 2 * j;
            var w = 0.0;
            if abs(dx) <= 2 && abs(dy) <= 2 { w = 4.0 * K[dx + 2] * K[dy + 2]; }
            t.at[n] = coarse_at(i, j);
            t.w[n] = w;
            n++;
        }
    }
    return t;
}

@compute @workgroup_size(256)
fn ll_prep(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.fw * p.fh { return; }
    g[p.g_f + i] = lstar(dot(lin[i].xyz, LUMA));
}

// Input pyramid: one level down.
@compute @workgroup_size(256)
fn ll_reduce_g(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.cw * p.ch { return; }
    let x = i32(i % p.cw) * 2;
    let y = i32(i / p.cw) * 2;
    var s = 0.0;
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            s += K[dx + 2] * K[dy + 2] * g[p.g_f + fine_at(x + dx, y + dy)];
        }
    }
    g[p.g_c + i] = s;
}

// Remapped pyramids: one level down (from level 0, remapping on the fly).
@compute @workgroup_size(256)
fn ll_reduce_q(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.cw * p.ch { return; }
    let x = i32(i % p.cw) * 2;
    let y = i32(i / p.cw) * 2;
    var a = vec4<f32>(0.0);
    var b = vec4<f32>(0.0);
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            let w = K[dx + 2] * K[dy + 2];
            let f = fine_at(x + dx, y + dy);
            if p.base == 1u {
                let r = remap(g[p.g_f + f]);
                a += w * r[0];
                b += w * r[1];
            } else {
                a += w * q[2u * (p.q_f + f)];
                b += w * q[2u * (p.q_f + f) + 1u];
            }
        }
    }
    q[2u * (p.q_c + i)] = a;
    q[2u * (p.q_c + i) + 1u] = b;
}

// The coarsest output level is the input's.
@compute @workgroup_size(256)
fn ll_top(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
          @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.fw * p.fh { return; }
    g[p.o_f + i] = g[p.g_f + i];
}

// One output level: the coarser output expanded, plus the Laplacian
// interpolated between the two remapped copies bracketing the input.
@compute @workgroup_size(256)
fn ll_collapse(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.fw * p.fh { return; }
    let x = i32(i % p.fw);
    let y = i32(i / p.fw);
    let v = g[p.g_f + i];
    let pos = clamp(v * GAMMAS - 0.5, 0.0, GAMMAS - 1.001);
    let lo = u32(pos);
    let t = pos - f32(lo);
    let taps = expand_taps(x, y);
    var base = 0.0;
    var ea = vec4<f32>(0.0);
    var eb = vec4<f32>(0.0);
    for (var k = 0; k < 9; k++) {
        let c = taps.at[k];
        let w = taps.w[k];
        base += w * g[p.o_c + c];
        ea += w * q[2u * (p.q_c + c)];
        eb += w * q[2u * (p.q_c + c) + 1u];
    }
    var fa: vec4<f32>;
    var fb: vec4<f32>;
    if p.base == 1u {
        let r = remap(v);
        fa = r[0];
        fb = r[1];
    } else {
        fa = q[2u * (p.q_f + i)];
        fb = q[2u * (p.q_f + i) + 1u];
    }
    let la = fa - ea;
    let lb = fb - eb;
    let l0 = select(lb[(lo - 4u) % 4u], la[lo % 4u], lo < 4u);
    let l1 = select(lb[(lo - 3u) % 4u], la[(lo + 1u) % 4u], lo + 1u < 4u);
    g[p.o_f + i] = base + mix(l0, l1, t);
}

// Scales the colour by the new Y.
@compute @workgroup_size(256)
fn ll_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.fw * p.fh { return; }
    let c = lin[i];
    let y = dot(c.xyz, LUMA);
    if y <= 1e-6 { return; }
    let gain = max(unlstar(g[p.o_f + i]), 0.0) / y;
    lin[i] = vec4<f32>(c.xyz * gain, c.w);
}
