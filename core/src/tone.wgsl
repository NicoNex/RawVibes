// Dehaze on the linear Rec.2020 develop buffer (local contrast: ll.wgsl).
// Dehaze: dark channel prior (He et al. 2009) with the transmission refined
// by a guided filter on Y (He et al. 2010); the airlight is the brightest
// dark-channel value (a grey haze) of the whole frame, so every view of the
// photo agrees (darktable takes it from its preview pipe). Negative amounts
// add haze through the same model (transmission above 1), as darktable does.

struct ToneParams {
    w: u32,
    h: u32,
    // Box radius of the guided filter; dark-channel window radius.
    radius: i32,
    window: i32,
    eps: f32,
    // Dehaze: haze removed (0..0.95) or, negative, haze added (0..−0.95).
    omega: f32,
    // 1: dh_min_v takes the airlight from these pixels (the whole-frame pass).
    seed: u32,
    _pad: f32,
}

@group(0) @binding(0) var<storage, read> p: ToneParams;
@group(0) @binding(1) var<storage, read_write> lin: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> sa: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sb: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> air: array<atomic<u32>>;

fn at(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
}

fn min3(c: vec3<f32>) -> f32 {
    return min(c.x, min(c.y, c.z));
}

// ---------------------------------------------------------- guided filter

@compute @workgroup_size(256)
fn box_h(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
         @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = vec4<f32>(0.0);
    for (var d = -p.radius; d <= p.radius; d++) { sum += sa[at(x + d, y)]; }
    sb[i] = sum / f32(2 * p.radius + 1);
}

@compute @workgroup_size(256)
fn box_v(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
         @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var sum = vec4<f32>(0.0);
    for (var d = -p.radius; d <= p.radius; d++) { sum += sb[at(x, y + d)]; }
    sa[i] = sum / f32(2 * p.radius + 1);
}

// sa holds box means of (I, p, I·p, I·I): the linear coefficients (a, b).
@compute @workgroup_size(256)
fn gf_ab(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
         @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let m = sa[i];
    let a = (m.z - m.x * m.y) / (m.w - m.x * m.x + p.eps);
    sa[i] = vec4<f32>(a, m.y - a * m.x, 0.0, 0.0);
}

// ----------------------------------------------------------------- dehaze

@compute @workgroup_size(256)
fn dh_min_h(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var m = 1e30;
    for (var d = -p.window; d <= p.window; d++) { m = min(m, min3(lin[at(x + d, y)].xyz)); }
    sb[i].x = m;
}

@compute @workgroup_size(256)
fn dh_min_v(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let x = i32(i % p.w);
    let y = i32(i / p.w);
    var m = 1e30;
    for (var d = -p.window; d <= p.window; d++) { m = min(m, sb[at(x, y + d)].x); }
    m = max(m, 0.0);
    sa[i].x = m;
    if p.seed == 1u {
        // Non-negative floats order like their bits.
        atomicMax(&air[0], bitcast<u32>(m));
    }
}

fn airlight() -> f32 {
    return max(bitcast<f32>(atomicLoad(&air[0])), 1e-6);
}

@compute @workgroup_size(256)
fn dh_prep(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let t = 1.0 - p.omega * sa[i].x / airlight();
    let g = dot(lin[i].xyz, LUMA);
    sa[i] = vec4<f32>(g, t, g * t, g * g);
}

@compute @workgroup_size(256)
fn dh_apply(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) n: vec3<u32>,
            @builtin(local_invocation_index) lid: u32) {
    let i = index(wg, n, lid);
    if i >= p.w * p.h { return; }
    let c = lin[i].xyz;
    let a = airlight();
    let t = max(sa[i].x * dot(c, LUMA) + sa[i].y, 0.1);
    let out = (c - a) / t + a;
    lin[i] = vec4<f32>(max(out, vec3<f32>(0.0)), lin[i].w);
}

