// SPDX-License-Identifier: GPL-3.0-or-later

struct Params {
    output_width: u32, output_height: u32, low_offset: u32, high_offset: u32,
    low_width: u32, low_height: u32, high_width: u32, high_height: u32,
    destination: u32, vertical: u32, high_first: u32, low_source: u32,
    high_source: u32, raw_width: u32, raw_height: u32, crop_x: u32,
    crop_y: u32, display_width: u32, display_height: u32, component0: u32,
    component1: u32, component2: u32, odd_green: u32, detail: u32,
    output_mode: u32, padding0: u32, padding1: u32, padding2: u32,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> coefficients: array<i32>;
@group(0) @binding(2) var<storage, read_write> work: array<i32>;
@group(0) @binding(3) var<storage, read> tables: array<u32>;
@group(0) @binding(4) var<storage, read_write> output: array<u32>;

// Split sums before shifting so signed coefficients never require optional i64 support.
fn half_sum(a: i32, b: i32) -> i32 {
    return (a >> 1u) + (b >> 1u) + (((a & 1) + (b & 1)) >> 1u);
}
fn quarter_sum(a: i32, b: i32, c: i32, d: i32) -> i32 {
    return (a >> 2u) + (b >> 2u) + (c >> 2u) + (d >> 2u)
        + (((a & 3) + (b & 3) + (c & 3) + (d & 3)) >> 2u);
}
fn update_term(a: i32, b: i32) -> i32 {
    return (a >> 2u) + (b >> 2u) + (((a & 3) + (b & 3) + 2) >> 2u);
}
fn source(kind: u32, index: u32) -> i32 {
    if kind == 0u { return coefficients[index]; }
    return work[index];
}
fn low_value(i: u32, line: u32) -> i32 {
    if p.vertical != 0u { return source(p.low_source, p.low_offset + i * p.low_width + line); }
    return source(p.low_source, p.low_offset + line * p.low_width + i);
}
fn high_value(i: u32, line: u32) -> i32 {
    if p.vertical != 0u { return source(p.high_source, p.high_offset + i * p.high_width + line); }
    return source(p.high_source, p.high_offset + line * p.high_width + i);
}
fn updated_low(i: u32, line: u32) -> i32 {
    let count = select(p.high_width, p.high_height, p.vertical != 0u);
    let a = u32(clamp(i32(i) + select(-1, 0, p.high_first != 0u), 0, i32(count) - 1));
    let b = u32(clamp(i32(i) + select(0, 1, p.high_first != 0u), 0, i32(count) - 1));
    return low_value(i, line) - update_term(high_value(a, line), high_value(b, line));
}
@compute @workgroup_size(16, 16)
fn inverse53(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.output_width || id.y >= p.output_height { return; }
    let coordinate = select(id.x, id.y, p.vertical != 0u);
    let line = select(id.y, id.x, p.vertical != 0u);
    let i = coordinate / 2u;
    var value: i32;
    if coordinate % 2u == p.high_first {
        value = updated_low(i, line);
    } else {
        let count = select(p.low_width, p.low_height, p.vertical != 0u);
        let a = u32(clamp(i32(i) + select(0, -1, p.high_first != 0u), 0, i32(count) - 1));
        let b = u32(clamp(i32(i) + select(1, 0, p.high_first != 0u), 0, i32(count) - 1));
        value = high_value(i, line) + half_sum(updated_low(a, line), updated_low(b, line));
    }
    work[p.destination + id.y * p.output_width + id.x] = value;
}
@compute @workgroup_size(16, 16)
fn green(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.output_width || id.y >= p.output_height { return; }
    let previous_y = max(id.y, 1u) - 1u;
    let next_x = min(id.x + 1u, p.output_width - 1u);
    let a = coefficients[p.detail + id.y * p.output_width + id.x];
    let b = coefficients[p.detail + id.y * p.output_width + next_x];
    let c = coefficients[p.detail + previous_y * p.output_width + id.x];
    let d = coefficients[p.detail + previous_y * p.output_width + next_x];
    let prediction = quarter_sum(a, b, c, d);
    work[p.odd_green + id.y * p.output_width + id.x] =
        work[p.component0 + id.y * p.output_width + id.x] - (prediction >> 1u) - (prediction & 1);
}
fn linear(code: i32) -> u32 {
    return tables[u32(clamp(code + 2048, 0, 4095))];
}
fn bayer(x: u32, y: u32) -> vec4<u32> {
    let width = p.raw_width / 2u;
    let height = p.raw_height / 2u;
    let next_y = min(y + 1u, height - 1u);
    let previous_x = max(x, 1u) - 1u;
    let odd = work[p.odd_green + y * width + x];
    let even = coefficients[p.detail + y * width + x] + quarter_sum(
        work[p.odd_green + y * width + previous_x],
        work[p.odd_green + next_y * width + previous_x],
        work[p.odd_green + next_y * width + x], odd);
    let mean = half_sum(clamp(even, -2048, 2047), clamp(odd, -2048, 2047));
    return vec4<u32>(linear(mean + 2 * work[p.component1 + y * width + x]),
        linear(odd), linear(even), linear(mean + 2 * work[p.component2 + y * width + x]));
}
@compute @workgroup_size(16, 16)
fn merge(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.output_width || id.y >= p.output_height { return; }
    if p.output_mode == 1u {
        let values = bayer(id.x, id.y);
        let row = id.y * 2u * p.output_width;
        output[row + id.x] = values.x | (values.y << 16u);
        output[row + p.output_width + id.x] = values.z | (values.w << 16u);
        return;
    }
    let x = ((p.crop_x + id.x * p.display_width / p.output_width) & ~1u) / 2u;
    let y = ((p.crop_y + id.y * p.display_height / p.output_height) & ~1u) / 2u;
    let values = bayer(x, y);
    let r = tables[4096u + values.x];
    let g = tables[4096u + (values.y + values.z) / 2u];
    let b = tables[4096u + values.w];
    output[id.y * p.output_width + id.x] = r | (g << 8u) | (b << 16u) | 0xff000000u;
}
