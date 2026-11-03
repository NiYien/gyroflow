// SPDX-License-Identifier: LGPL-2.1-only OR CDDL-1.0
// Copyright 2026 LibRaw LLC (info@libraw.org)
// Copyright (C) 2026 Jiang Yaogeng
// LLVC3 primitives translated from LibRaw 2caab57e269cc7fce17c3189e4d4d7e3e5861875.
// Original notices and both licenses are retained under vendor/xocn/.

struct Parameters { records: u32, samples: u32, bytes: u32, padding: u32 }
struct Record {
    packet: vec4<u32>,
    configuration: vec4<u32>,
    plane0: vec4<u32>,
    plane1: vec4<u32>,
    plane2: vec4<u32>,
    padding: vec4<u32>,
}
struct Bits {
    packet: u32, length: u32, payload: u32, limit: u32,
    position: u32, status: u32,
}
@group(0) @binding(0) var<uniform> parameters: Parameters;
@group(0) @binding(1) var<storage, read> compressed: array<u32>;
@group(0) @binding(2) var<storage, read> records: array<Record>;
@group(0) @binding(3) var<storage, read_write> coefficients: array<i32>;
@group(0) @binding(4) var<storage, read_write> status: array<u32>;

fn big_endian_word(bits: ptr<function, Bits>, relative: u32) -> u32 {
    // Packets are aligned, but individual record payloads can start at any byte.
    // Extra allowed words beyond this packet are zero, even if another packet follows.
    if relative >= (*bits).length { return 0u; }
    let index = ((*bits).packet + relative) / 4u;
    let shift = (relative & 3u) * 8u;
    var value = compressed[index] >> shift;
    if shift != 0u && (relative & ~3u) + 4u < (*bits).length {
        value |= compressed[index + 1u] << (32u - shift);
    }
    return (value << 24u) | ((value & 0x0000ff00u) << 8u)
        | ((value & 0x00ff0000u) >> 8u) | (value >> 24u);
}
fn read_bits(bits: ptr<function, Bits>, count: u32) -> u32 {
    if count == 0u { return 0u; }
    if (*bits).position + count > (*bits).limit {
        (*bits).status = 2u;
        return 0u;
    }
    let offset = (*bits).position & 31u;
    let word = (*bits).payload + ((*bits).position / 32u) * 4u;
    var value = big_endian_word(bits, word) << offset;
    if offset + count > 32u { value |= big_endian_word(bits, word + 4u) >> (32u - offset); }
    (*bits).position += count;
    if count == 32u { return value; }
    return value >> (32u - count);
}
fn unary_width(bits: ptr<function, Bits>) -> u32 {
    var zeros = 0u;
    loop {
        let value = read_bits(bits, 1u);
        if (*bits).status != 0u || value != 0u { return zeros + 1u; }
        zeros += 1u;
        // A width greater than 19 is invalid in the CPU decoder too.
        if zeros >= 19u { (*bits).status = 1u; return 20u; }
    }
    return 0u;
}
fn update_width(bits: ptr<function, Bits>, width: u32) -> u32 {
    let same = read_bits(bits, 1u);
    if same == 0u || (*bits).status != 0u { return width; }
    let increasing = read_bits(bits, 1u);
    if increasing == 0u || (*bits).status != 0u { return width + unary_width(bits); }
    let decreases = max(width, 1u) - 1u;
    for (var zeros = 0u; zeros < decreases; zeros += 1u) {
        if read_bits(bits, 1u) != 0u { return width - (zeros + 1u); }
    }
    return 0u;
}
fn zero_run(bits: ptr<function, Bits>, remaining: u32) -> u32 {
    if remaining <= 1u { return remaining; }
    let max_prefix = 32u - countLeadingZeros(remaining - 1u);
    var zeros = 0u;
    loop {
        if zeros >= max_prefix { break; }
        if read_bits(bits, 1u) != 0u { break; }
        if (*bits).status != 0u { return remaining; }
        zeros += 1u;
    }
    if zeros >= max_prefix { return remaining; }
    let base = 1u << zeros;
    if base >= remaining { return remaining; }
    var extra = 0u;
    if zeros != 0u { extra = read_bits(bits, zeros); }
    return min(base + extra, remaining);
}
fn store_value(offset: u32, value: i32, integrate: bool, accumulator: ptr<function, i32>) {
    if integrate {
        let low = u32(value) & 0xffffu;
        let signed = select(i32(low), i32(low) - 65536, low >= 32768u);
        *accumulator += signed;
        coefficients[offset] = *accumulator;
    } else { coefficients[offset] = value; }
}
fn decode_row(bits: ptr<function, Bits>, offset: u32, columns: u32, shift: u32, integrate: bool) {
    let groups = (columns + 3u) / 4u;
    var width = update_width(bits, 0u);
    var group = 0u;
    var out = 0u;
    var accumulator = 0;
    loop {
        if group >= groups { break; }
        if (*bits).status != 0u || width > 19u {
            if (*bits).status == 0u { (*bits).status = 1u; }
            break;
        }
        if width == 0u {
            let count = zero_run(bits, groups - group);
            if !integrate {
                out = min(out + count * 4u, columns);
            } else {
                for (var n = 0u; n < count; n += 1u) {
                    for (var k = 0u; k < 4u && out < columns; k += 1u) {
                        store_value(offset + out, 0, integrate, &accumulator);
                        out += 1u;
                    }
                }
            }
            group += count;
            if group >= groups { break; }
            width = unary_width(bits);
            continue;
        }
        var values: array<i32, 4>;
        for (var k = 0u; k < 4u; k += 1u) {
            let encoded = read_bits(bits, width);
            var expanded = encoded;
            if shift > 0u && encoded > 0u {
                expanded = ((2u * encoded + 1u) << (shift - 1u)) - (encoded & 1u);
            }
            values[k] = bitcast<i32>(expanded);
        }
        var next_width = width;
        if group + 1u < groups { next_width = update_width(bits, width); }
        for (var k = 0u; k < 4u; k += 1u) {
            if values[k] > 0 && read_bits(bits, 1u) != 0u { values[k] = -values[k]; }
        }
        for (var k = 0u; k < 4u && out < columns; k += 1u) {
            store_value(offset + out, values[k], integrate, &accumulator);
            out += 1u;
        }
        width = next_width;
        group += 1u;
    }
    while out < columns {
        if integrate { store_value(offset + out, 0, true, &accumulator); }
        out += 1u;
    }
}
@compute @workgroup_size(64)
fn entropy(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= parameters.records { return; }
    let record = records[id.x];
    // The host clears coefficients once before this pass, including absent records.
    if record.packet.w == 0u { status[id.x] = 0u; return; }
    var bits = Bits(record.packet.x, record.packet.y, record.packet.z, record.packet.w * 64u, 0u, 0u);
    let planes = array<vec4<u32>, 3>(record.plane0, record.plane1, record.plane2);
    for (var orientation = 0u; orientation < record.configuration.x; orientation += 1u) {
        let plane = planes[orientation];
        for (var row = 0u; row < plane.z; row += 1u) {
            let offset = plane.x + row * plane.y;
            decode_row(&bits, offset, plane.y, plane.w, record.configuration.y != 0u);
        }
    }
    status[id.x] = bits.status;
}
