// SPDX-License-Identifier: GPL-3.0-or-later
// One workgroup per frame pair: independent robust layer fits, with no CPU round trip between iterations.
struct LayerParams { count:u32, pad0:u32, pad1:u32, pad2:u32, }
struct LayerJob { input:u32, count:u32, workspace:u32, pad:u32, }
@group(0) @binding(0) var<uniform> L:LayerParams;
@group(0) @binding(1) var<storage,read> points:array<f64>;
@group(0) @binding(2) var<storage,read> jobs:array<LayerJob>;
@group(0) @binding(3) var<storage,read_write> work:array<f64>;
@group(0) @binding(4) var<storage,read_write> output:array<f64>;
var<workgroup> sums:array<f64,512>;
var<workgroup> order:array<u32,MAX_POINTS*2u>;
var<workgroup> estimate:array<f64,3>;
var<workgroup> valid:u32;
var<workgroup> median_value:f64;

fn finite(value:f64)->bool {return abs(value)<=1.7976931348623157e308lf;}
fn less(a:u32,b:u32,offset:u32)->bool {
    if (a==0xffffffffu) {return false;}if (b==0xffffffffu) {return true;}
    let x=work[offset+a];let y=work[offset+b];
    return x<y || (x==y && a<b);
}
fn weighted_median(job:LayerJob,length:u32,lane:u32)->f64 {
    let count=job.count*2u;let offset=job.workspace+count;
    storageBarrier();
    for (var i=lane;i<length;i+=64u) {order[i]=select(0xffffffffu,i,i<count);}
    workgroupBarrier();
    for (var block=2u;block<=length;block*=2u) {
        for (var distance=block/2u;distance>0u;distance/=2u) {
            for (var i=lane;i<length;i+=64u) {
                let j=i^distance;
                if (j>i) {
                    let a=order[i];let b=order[j];let ascending=(i&block)==0u;
                    if (select(less(a,b,offset),less(b,a,offset),ascending)) {order[i]=b;order[j]=a;}
                }
            }
            workgroupBarrier();
        }
    }
    if (lane==0u) {
        var total=0.0lf;
        for (var i=0u;i<count;i++) {total+=points[job.input+(order[i]/2u)*6u+5u];}
        let cutoff=total*0.5lf;var mass=0.0lf;
        median_value=work[offset+order[count-1u]];
        for (var i=0u;i<count;i++) {
            mass+=points[job.input+(order[i]/2u)*6u+5u];
            if (mass>=cutoff) {median_value=work[offset+order[i]];break;}
        }
    }
    workgroupBarrier();
    return workgroupUniformLoad(&median_value);
}

@compute @workgroup_size(64,1,1)
fn fit_layers(@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
    if (group.x>=L.count) {return;}
    let job=jobs[group.x];let n=job.count;
    if (n<12u || n>MAX_POINTS) {return;}
    var length=1u;loop {if (length>=2u*n) {break;}length*=2u;}
    for (var i=lane;i<2u*n;i+=64u) {work[job.workspace+i]=1.0lf;}
    storageBarrier();
    for (var iteration=0u;iteration<6u;iteration++) {
        var acc:array<f64,8>;
        for (var k=0u;k<8u;k++) {acc[k]=0.0lf;}
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;
            let x=points[at];let y=points[at+1u];let dx=points[at+2u];let dy=points[at+3u];let beta=points[at+4u];let area=points[at+5u];
            let wx=work[job.workspace+2u*i]*area;let wy=work[job.workspace+2u*i+1u]*area;
            let bx=beta*x;let by=beta*y;
            acc[0]+=beta*beta*wx;acc[1]+=beta*beta*wy;
            acc[2]+=beta*bx*wx;acc[3]+=beta*by*wy;
            acc[4]+=bx*bx*wx+by*by*wy;
            acc[5]+=beta*(dx*wx);acc[6]+=beta*(dy*wy);acc[7]+=bx*(dx*wx)+by*(dy*wy);
        }
        for (var k=0u;k<8u;k++) {sums[k*64u+lane]=acc[k];}
        workgroupBarrier();
        for (var step=32u;step>0u;step/=2u) {
            if (lane<step) {for (var k=0u;k<8u;k++) {sums[k*64u+lane]+=sums[k*64u+lane+step];}}
            workgroupBarrier();
        }
        if (lane==0u) {
            valid=0u;
            let xx=sums[0u];let yy=sums[64u];let xz=sums[128u];let yz=sums[192u];let zz=sums[256u];
            let gx=sums[320u];let gy=sums[384u];let gz=sums[448u];
            let denominator=zz-xz*xz/xx-yz*yz/yy;
            if (xx>0.0lf && yy>0.0lf && denominator>0.0lf) {
                let z=(gz-xz*gx/xx-yz*gy/yy)/denominator;
                let x=(gx-xz*z)/xx;let y=(gy-yz*z)/yy;
                if (finite(x) && finite(y) && finite(z)) {estimate[0]=x;estimate[1]=y;estimate[2]=z;valid=1u;}
            }
        }
        workgroupBarrier();
        if (workgroupUniformLoad(&valid)==0u) {return;}
        if (iteration==5u) {break;}
        let sx=workgroupUniformLoad(&estimate[0]);let sy=workgroupUniformLoad(&estimate[1]);let sz=workgroupUniformLoad(&estimate[2]);
        let offset=job.workspace+2u*n;
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;let beta=points[at+4u];
            work[offset+2u*i]=points[at+2u]-(sx+points[at]*sz)*beta;
            work[offset+2u*i+1u]=points[at+3u]-(sy+points[at+1u]*sz)*beta;
        }
        let centre=weighted_median(job,length,lane);
        for (var i=lane;i<2u*n;i+=64u) {work[offset+i]=abs(work[offset+i]-centre);}
        let deviation=weighted_median(job,length,lane);
        let scale=max(2.5lf*1.4826lf*deviation,1e-9lf);
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;let beta=points[at+4u];
            let rx=(points[at+2u]-(sx+points[at]*sz)*beta)/scale;
            let ry=(points[at+3u]-(sy+points[at+1u]*sz)*beta)/scale;
            work[job.workspace+2u*i]=1.0lf/(1.0lf+rx*rx);work[job.workspace+2u*i+1u]=1.0lf/(1.0lf+ry*ry);
        }
        storageBarrier();
    }
    if (lane==0u) {output[group.x*4u]=estimate[0];output[group.x*4u+1u]=estimate[1];output[group.x*4u+2u]=estimate[2];output[group.x*4u+3u]=1.0lf;}
}
