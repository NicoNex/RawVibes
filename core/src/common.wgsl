// Prepended to raw.wgsl, denoise.wgsl, detail.wgsl, tone.wgsl and ll.wgsl (raw.rs).

const WG: u32 = 256u;
// Rec.2020 luminance weights (the linear working space).
const LUMA: vec3<f32> = vec3<f32>(0.2627, 0.6780, 0.0593);
// 5-tap binomial (B3 spline) kernel.
const B3: array<f32, 5> = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);

// Item of a 2-D dispatch of 1-D workgroups (`grid` in raw.rs).
fn index(wg: vec3<u32>, n: vec3<u32>, lid: u32) -> u32 {
    return (wg.y * n.x + wg.x) * WG + lid;
}
