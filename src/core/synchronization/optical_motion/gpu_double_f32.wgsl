// SPDX-License-Identifier: GPL-3.0-or-later
// High/low pairs retain the low bits of host inputs and compensate arithmetic roundoff.
alias D = vec2<f32>;
fn ds(v:f32)->D { return D(v,0.0); }
// Keep an observable rounding boundary so shader optimizers cannot cancel the compensation.
fn dround(v:f32)->f32 { return v*precision_guard(); }
fn dadd(a:D,b:D)->D {
    let s=dround(a.x+b.x); let v=dround(s-a.x);
    let e=dround(dround(a.x-dround(s-v))+dround(b.x-v))+dround(a.y+b.y);
    let hi=dround(s+e); return D(hi,dround(e-dround(hi-s)));
}
fn dsub(a:D,b:D)->D { return dadd(a,-b); }
fn dmul(a:D,b:D)->D {
    let p=dround(a.x*b.x);
    let e=fma(a.x,b.x,-p)+a.x*b.y+a.y*b.x+a.y*b.y;
    let hi=dround(p+e); return D(hi,dround(e-dround(hi-p)));
}
fn ddiv(a:D,b:D)->D {
    let q=a.x/b.x;
    let r=dsub(a,dmul(b,ds(q)));
    return dadd(ds(q),ds((r.x+r.y)/b.x));
}
fn dlt(a:D,b:D)->bool { return a.x<b.x || (a.x==b.x && a.y<b.y); }
fn dabs(a:D)->D { return select(a,-a,dlt(a,ds(0.0))); }
fn dmax(a:D,b:D)->D { return select(a,b,dlt(a,b)); }
fn dfinite(a:D)->bool { return all(abs(a)<=vec2<f32>(3.402823466e38)); }
