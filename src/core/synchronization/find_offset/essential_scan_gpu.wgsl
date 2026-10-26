// SPDX-License-Identifier: GPL-3.0-or-later
// Each invocation bounds one original 1 ms offset; final scores remain CPU f64.

struct Observation {
    rate: vec4<f32>,
    base: vec2<i32>,
    padding: vec2<u32>,
}
struct Params {
    start: u32,
    count: u32,
    gyro_count: u32,
    observation_count: u32,
    bucket_count: u32,
    tolerance_us: i32,
    error_factor: f32,
    padding: u32,
}
@group(0) @binding(0) var<storage, read> times: array<vec2<i32>>;
@group(0) @binding(1) var<storage, read> rates: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> buckets: array<u32>;
@group(0) @binding(3) var<storage, read> observations: array<Observation>;
@group(0) @binding(4) var<storage, read_write> bounds: array<f32>;
@group(0) @binding(5) var<uniform> params: Params;

fn earlier(a: vec2<i32>, b: vec2<i32>) -> bool {
    return a.x < b.x || (a.x == b.x && a.y < b.y);
}
fn shift_us(t: vec2<i32>, delta: i32) -> vec2<i32> {
    var result = vec2<i32>(t.x, t.y + delta);
    if result.y < 0 { result.x -= 1; result.y += 1000; }
    if result.y >= 1000 { result.x += 1; result.y -= 1000; }
    return result;
}
fn lookup(t: vec2<i32>) -> u32 {
    let bucket = min(u32(max(t.x, 0)) / 4u, params.bucket_count - 1u);
    var index = buckets[bucket];
    loop {
        if index >= params.gyro_count { break; }
        if !earlier(times[index], t) { break; }
        index += 1u;
    }
    return index;
}

@compute @workgroup_size(64)
fn scan(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let local_index = invocation.x;
    if local_index >= params.count { return; }
    let offset_index = params.start + local_index;
    var total = 0.0;
    var scale = 0.0;
    var measured = 0u;
    let weights = vec3<f32>(70.0, 70.0, 100.0);
    for (var i = 0u; i < params.observation_count; i += 1u) {
        let observation = observations[i];
        let time = vec2<i32>(observation.base.x - i32(offset_index), observation.base.y);
        let first = lookup(shift_us(time, -params.tolerance_us));
        let last = lookup(shift_us(time, params.tolerance_us));
        if first >= params.gyro_count { continue; }
        // Uncertain coverage or too many samples cannot safely exclude this offset.
        if last >= params.gyro_count || last - first > 8u { bounds[local_index] = 0.0; return; }
        var best = 3.402823466e+38;
        var largest_scale = 0.0;
        for (var j = first; j <= last; j += 1u) {
            let g = rates[j].xyz;
            let o = observation.rate.xyz;
            let difference = g - o;
            let size = abs(g) + abs(o);
            best = min(best, dot(difference * difference, weights));
            largest_scale = max(largest_scale, dot(size * size, weights));
        }
        total += best;
        scale += largest_scale;
        measured += 1u;
    }
    if measured <= params.observation_count / 2u {
        bounds[local_index] = 3.402823466e+38;
        return;
    }
    // Covers input conversion, subtraction, squares, weighted sums and division.
    // The host restricts magnitudes/counts to avoid overflow and excessive gamma_n.
    bounds[local_index] = max(0.0, (total - params.error_factor * scale) / f32(measured) - 1e-20);
}
