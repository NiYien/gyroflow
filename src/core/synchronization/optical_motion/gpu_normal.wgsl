// SPDX-License-Identifier: GPL-3.0-or-later
// Column-major design matrix (including the observed value as the last column), uploaded once.
struct NormalParams { rows:u32, columns:u32, output_offset:u32, pad1:u32, }
@group(0) @binding(0) var<uniform> N:NormalParams;
@group(0) @binding(1) var<storage,read> design:array<f64>;
@group(0) @binding(2) var<storage,read> weights:array<f64>;
@group(0) @binding(3) var<storage,read_write> gram:array<f64>;
var<workgroup> partial:array<f64,64>;
@compute @workgroup_size(64,1,1)
fn normal_matrix(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
    if (group.x>=N.columns || group.y>group.x) {return;}
    var sum=0.0lf;
    for (var row=lane;row<N.rows;row+=64u) {
        let a=design[group.x*N.rows+row];let b=design[group.y*N.rows+row];
        sum+=a*b*weights[row];
    }
    partial[lane]=sum;workgroupBarrier();
    for (var step=32u;step>0u;step/=2u) {
        if (lane<step) {partial[lane]+=partial[lane+step];}
        workgroupBarrier();
    }
    if (lane==0u) {gram[N.output_offset+group.x*N.columns+group.y]=partial[0];}
}
