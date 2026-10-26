// SPDX-License-Identifier: GPL-3.0-or-later
struct LayerParams { count:u32, pad0:u32, pad1:u32, pad2:u32, }
struct LayerJob { input:u32, count:u32, workspace:u32, pad:u32, }
@group(0) @binding(0) var<uniform> L:LayerParams;
fn precision_guard()->f32 { return f32(L.pad0); }
@group(0) @binding(1) var<storage,read> points:array<D>;
@group(0) @binding(2) var<storage,read> jobs:array<LayerJob>;
@group(0) @binding(3) var<storage,read_write> work:array<D>;
@group(0) @binding(4) var<storage,read_write> output:array<D>;
var<workgroup> sums:array<D,512>;
var<workgroup> order:array<u32,MAX_POINTS*2u>;
var<workgroup> estimate:array<D,3>;
var<workgroup> valid:u32;
var<workgroup> median_value:D;
fn less(a:u32,b:u32,offset:u32)->bool {
    if (a==0xffffffffu) {return false;}if (b==0xffffffffu) {return true;}
    let x=work[offset+a];let y=work[offset+b];
    return dlt(x,y) || (all(x==y) && a<b);
}
fn weighted_median(job:LayerJob,length:u32,lane:u32)->D {
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
        var total=ds(0.0);
        for (var i=0u;i<count;i++) {total=dadd(total,points[job.input+(order[i]/2u)*6u+5u]);}
        let cutoff=dmul(total,ds(0.5));var mass=ds(0.0);
        median_value=work[offset+order[count-1u]];
        for (var i=0u;i<count;i++) {
            mass=dadd(mass,points[job.input+(order[i]/2u)*6u+5u]);
            if (!dlt(mass,cutoff)) {median_value=work[offset+order[i]];break;}
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
    for (var i=lane;i<2u*n;i+=64u) {work[job.workspace+i]=ds(1.0);}
    storageBarrier();
    for (var iteration=0u;iteration<6u;iteration++) {
        var acc:array<D,8>;
        for (var k=0u;k<8u;k++) {acc[k]=ds(0.0);}
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;
            let x=points[at];let y=points[at+1u];let dx=points[at+2u];let dy=points[at+3u];let beta=points[at+4u];let area=points[at+5u];
            let wx=dmul(work[job.workspace+2u*i],area);let wy=dmul(work[job.workspace+2u*i+1u],area);
            let bx=dmul(beta,x);let by=dmul(beta,y);
            acc[0]=dadd(acc[0],dmul(dmul(beta,beta),wx));acc[1]=dadd(acc[1],dmul(dmul(beta,beta),wy));
            acc[2]=dadd(acc[2],dmul(dmul(beta,bx),wx));acc[3]=dadd(acc[3],dmul(dmul(beta,by),wy));
            acc[4]=dadd(acc[4],dadd(dmul(dmul(bx,bx),wx),dmul(dmul(by,by),wy)));
            acc[5]=dadd(acc[5],dmul(beta,dmul(dx,wx)));acc[6]=dadd(acc[6],dmul(beta,dmul(dy,wy)));
            acc[7]=dadd(acc[7],dadd(dmul(bx,dmul(dx,wx)),dmul(by,dmul(dy,wy))));
        }
        for (var k=0u;k<8u;k++) {sums[k*64u+lane]=acc[k];}
        workgroupBarrier();
        for (var step=32u;step>0u;step/=2u) {
            if (lane<step) {for (var k=0u;k<8u;k++) {sums[k*64u+lane]=dadd(sums[k*64u+lane],sums[k*64u+lane+step]);}}
            workgroupBarrier();
        }
        if (lane==0u) {
            valid=0u;
            let xx=sums[0u];let yy=sums[64u];let xz=sums[128u];let yz=sums[192u];let zz=sums[256u];
            let gx=sums[320u];let gy=sums[384u];let gz=sums[448u];
            let denominator=dsub(dsub(zz,ddiv(dmul(xz,xz),xx)),ddiv(dmul(yz,yz),yy));
            if (dlt(ds(0.0),xx) && dlt(ds(0.0),yy) && dlt(ds(0.0),denominator)) {
                let z=ddiv(dsub(dsub(gz,ddiv(dmul(xz,gx),xx)),ddiv(dmul(yz,gy),yy)),denominator);
                let x=ddiv(dsub(gx,dmul(xz,z)),xx);let y=ddiv(dsub(gy,dmul(yz,z)),yy);
                if (dfinite(x) && dfinite(y) && dfinite(z)) {estimate[0]=x;estimate[1]=y;estimate[2]=z;valid=1u;}
            }
        }
        workgroupBarrier();
        if (workgroupUniformLoad(&valid)==0u) {return;}
        if (iteration==5u) {break;}
        let sx=workgroupUniformLoad(&estimate[0]);let sy=workgroupUniformLoad(&estimate[1]);let sz=workgroupUniformLoad(&estimate[2]);
        let offset=job.workspace+2u*n;
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;let beta=points[at+4u];
            work[offset+2u*i]=dsub(points[at+2u],dmul(dadd(sx,dmul(points[at],sz)),beta));
            work[offset+2u*i+1u]=dsub(points[at+3u],dmul(dadd(sy,dmul(points[at+1u],sz)),beta));
        }
        let centre=weighted_median(job,length,lane);
        for (var i=lane;i<2u*n;i+=64u) {work[offset+i]=dabs(dsub(work[offset+i],centre));}
        let deviation=weighted_median(job,length,lane);
        let scale=dmax(dmul(D(3.7065, -5.3405762e-8),deviation),ds(1e-9));
        for (var i=lane;i<n;i+=64u) {
            let at=job.input+i*6u;let beta=points[at+4u];
            let rx=ddiv(dsub(points[at+2u],dmul(dadd(sx,dmul(points[at],sz)),beta)),scale);
            let ry=ddiv(dsub(points[at+3u],dmul(dadd(sy,dmul(points[at+1u],sz)),beta)),scale);
            work[job.workspace+2u*i]=ddiv(ds(1.0),dadd(ds(1.0),dmul(rx,rx)));
            work[job.workspace+2u*i+1u]=ddiv(ds(1.0),dadd(ds(1.0),dmul(ry,ry)));
        }
        storageBarrier();
    }
    if (lane==0u) {output[group.x*4u]=estimate[0];output[group.x*4u+1u]=estimate[1];output[group.x*4u+2u]=estimate[2];output[group.x*4u+3u]=ds(1.0);}
}
