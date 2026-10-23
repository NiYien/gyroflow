# SPDX-License-Identifier: GPL-3.0-or-later
"""Reference fixture for `optical_analysis::residual_rotation` (openspec change fused-residual-rotation, task 1.2).

The estimator is the offline prototype validated in target/unified-path-lab (a3_estimate.assemble / prior / evidence,
a4_estimate selection and robust final solve), copied here verbatim in its numerics so the fixture does not depend on
the lab. Production semantics where the prototype ran per half: one node grid for all points, the selection averages
both directions (half 0 estimates scored on half 1's held-out motion and the other way round), the final estimate
uses all points.

A synthetic scene with a known bounded error d, a slow smooth picture-model error eta and a dominant-layer motion is
written together with the expected intermediate and final results. Run with numpy + scipy:
    python make_residual_rotation_fixture.py   (writes residual_rotation_fixture.bin next to this file)
"""
import struct
from pathlib import Path

import numpy as np
from scipy.linalg import cholesky_banded, cho_solve_banded
from scipy.signal import butter, sosfiltfilt

H_S = 0.005
NOISE_FLOOR_PX = 0.3
ROBUST_K = 2.5
Q_ETA = 1e-12
TAUS = [0.03, 0.1, 0.3, 1.0]
SIGMAS = np.radians([0.0, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5])
R_DIAG = float(np.hypot(480, 270))
BANDS = [(.4, 3.), (1., 3.), (3., 10.)]

FPS = 25.0
FRAMES = 150
POINTS_PER_PAIR = 20
FOCAL = 800.0
SIZE = (960, 540)
READOUT_MS = 20.0


def qmat(q):
    w, x, y, z = np.moveaxis(q, -1, 0)
    return np.stack([
        np.stack([1-2*(y*y+z*z), 2*(x*y-w*z), 2*(x*z+w*y)], -1),
        np.stack([2*(x*y+w*z), 1-2*(x*x+z*z), 2*(y*z-w*x)], -1),
        np.stack([2*(x*z-w*y), 2*(y*z+w*x), 1-2*(x*x+y*y)], -1)], -2)


def qexp(v):
    th = np.linalg.norm(v, axis=-1, keepdims=True)
    s = np.where(th > 1e-12, np.sin(th/2)/np.maximum(th, 1e-300), 0.5)
    return np.concatenate([np.cos(th/2), v*s], -1)


def rotvec_mat(v):
    return qmat(qexp(v))


# ---- the prototype's numerics (target/unified-path-lab/a3_estimate.py), unchanged ----

def assemble(G, p, res, w, Rb, nodes, coef, Pw):
    span = int(np.max(nodes.max(1) - nodes.min(1))) + 1
    u = 6*max(span, 3) + 5
    ab = np.zeros((u + 1, 6*G))
    eta_vec = np.zeros(6*G)
    gw = np.einsum('nij,nj->ni', Rb, np.cross(p, res))*w[:, None]
    WP = Pw*w[:, None, None]
    for a in range(4):
        for s in (0, 3):
            np.add.at(eta_vec.reshape(G, 6)[:, s:s + 3], nodes[:, a], coef[:, a, None]*gw)
        for b in range(4):
            blk = (coef[:, a]*coef[:, b])[:, None, None]*WP
            na, nb = nodes[:, a], nodes[:, b]
            m = na <= nb
            for si in (0, 3):
                for sj in (0, 3):
                    for i in range(3):
                        for j in range(3):
                            rows = 6*na[m] + si + i
                            cols = 6*nb[m] + sj + j
                            sel = rows <= cols
                            np.add.at(ab, (u + rows[sel] - cols[sel], cols[sel]), blk[m][sel, i, j])
    return ab, eta_vec, u


def prior(G, u, sig, tau, q):
    ab = np.zeros((u + 1, 6*G))
    phi = np.exp(-H_S/tau)
    qd = sig**2*(1 - phi**2)
    dd = np.full(G, (1 + phi**2)/qd); dd[0] = dd[-1] = 1/qd
    for ax in range(3):
        ab[u, ax::6] += dd
        ab[u - 6, 6 + ax::6] += -phi/qd
    g0 = np.zeros(G); g1 = np.zeros(G - 1); g2 = np.zeros(G - 2)
    for r in range(G - 2):
        g0[r] += 1; g0[r + 1] += 4; g0[r + 2] += 1
        g1[r] += -2; g1[r + 1] += -2
        g2[r] += 1
    g0[:2] += 1e-6
    for ax in range(3):
        ab[u, 3 + ax::6] += g0/q
        ab[u - 6, 6 + 3 + ax::6][:G - 1] += g1/q
        ab[u - 12, 12 + 3 + ax::6][:G - 2] += g2/q
    return ab


def solve(ab, eta_vec, u, G, sig, tau):
    if sig <= 0:
        return np.zeros((G, 3)), None
    cb = cholesky_banded(ab + prior(G, u, sig, tau, Q_ETA), lower=False)
    X = cho_solve_banded((cb, False), eta_vec).reshape(G, 6)
    return X[:, :3], X[:, 3:]


def band(x, lo, hi, fps):
    return sosfiltfilt(butter(3, [lo, hi], btype='bandpass', fs=fps, output='sos'), x, axis=0)


# ---- synthetic scene ----

def camera(t):
    """Gyro orientation (camera to world) at video time t (s)."""
    a = np.stack([0.05*np.sin(2*np.pi*0.7*t), 0.08*np.sin(2*np.pi*0.3*t + 1.0), 0.02*np.sin(2*np.pi*1.3*t)], -1)
    return rotvec_mat(a)


def true_error(t):
    """Bounded fast error d (world frame, rad) and slow smooth picture-model error eta."""
    d = np.stack([4e-4*np.sin(2*np.pi*3.1*t) + 2e-4*np.sin(2*np.pi*5.3*t + 0.4),
                  3e-4*np.sin(2*np.pi*2.3*t + 1.1) + 2e-4*np.sin(2*np.pi*6.7*t),
                  2e-4*np.sin(2*np.pi*4.1*t + 2.0)], -1)
    eta = np.stack([2e-3*np.sin(2*np.pi*0.11*t), -1.5e-3*np.sin(2*np.pi*0.07*t + 0.5), 1e-3*np.sin(2*np.pi*0.13*t)], -1)
    return d, eta


def main():
    rng = np.random.default_rng(20261023)
    frame_ms = 1000.0/FPS
    per_px = READOUT_MS/SIZE[1]
    t_frame = np.arange(FRAMES)*frame_ms                     # frame timestamps, ms
    start = t_frame - READOUT_MS/2                          # first row of each frame

    # dominant layer: one translation sample per frame at its middle, layer motion x auto_beta as a rotation-equivalent
    seg = np.zeros(FRAMES, int); seg[90:] = 1               # a new segment restarts the chain
    lm = np.stack([2e-4*np.sin(2*np.pi*0.9*t_frame/1000), 1.5e-4*np.cos(2*np.pi*1.7*t_frame/1000)], 1)
    beta = 1.0 + 0.2*np.sin(t_frame/700)
    cam_q = np.stack([qexp(np.array([0.05*np.sin(2*np.pi*0.7*t), 0.08*np.sin(2*np.pi*0.3*t + 1.0), 0.02*np.sin(2*np.pi*1.3*t)]))
                      for t in t_frame/1000])
    dom = lm*beta[:, None]
    dw = np.zeros((FRAMES, 3))
    for i in range(1, FRAMES):
        if seg[i] != seg[i - 1]:
            dw[i] = dw[i - 1]
            continue
        om = np.array([-dom[i, 1], -dom[i, 0], 0.0])
        dw[i] = dw[i - 1] - qmat(cam_q[i])@om

    pts = []
    for k in range(FRAMES - 1):
        for j in range(POINTS_PER_PAIR):
            pid = k*1000 + j
            xa, ya = rng.uniform(40, SIZE[0] - 40), rng.uniform(40, SIZE[1] - 40)
            ta = start[k] + per_px*ya
            va = np.array([(xa - SIZE[0]/2)/FOCAL, -(ya - SIZE[1]/2)/FOCAL, -1.0]); va /= np.linalg.norm(va)
            def true_R(tm):
                d, eta = true_error(tm/1000)
                ddom = np.array([np.interp(tm, t_frame, dw[:, c]) for c in range(3)])
                return rotvec_mat(d + eta + ddom)@camera(tm/1000)
            W = true_R(ta)@va
            yb = ya
            for _ in range(4):
                tb = start[k + 1] + per_px*yb
                vb = true_R(tb).T@W
                yb = SIZE[1]/2 - vb[1]/(-vb[2])*FOCAL
            xb = SIZE[0]/2 + vb[0]/(-vb[2])*FOCAL
            xb += rng.normal(0, 0.1); yb += rng.normal(0, 0.1)
            vb = np.array([(xb - SIZE[0]/2)/FOCAL, -(yb - SIZE[1]/2)/FOCAL, -1.0]); vb /= np.linalg.norm(vb)
            Ra, Rb = camera(ta/1000), camera(tb/1000)
            p = Rb.T@Ra@va; p /= np.linalg.norm(p)
            r = vb - p
            cell = min(int(ya/SIZE[1]*9), 8)*16 + min(int(xa/SIZE[0]*16), 15)
            qb = qexp(np.array([0.05*np.sin(2*np.pi*0.7*tb/1000), 0.08*np.sin(2*np.pi*0.3*tb/1000 + 1.0), 0.02*np.sin(2*np.pi*1.3*tb/1000)]))
            pts.append((k, pid % 2, cell, ta, tb, *p, *r, *qb))
    P = np.array(pts, float)
    pair, half, cell = P[:, 0].astype(int), P[:, 1].astype(int), P[:, 2].astype(int)
    ta, tb = P[:, 3], P[:, 4]
    p, res0 = P[:, 5:8], P[:, 8:11]
    Rb = qmat(P[:, 11:15])

    # grid and interpolation terms of d(ta) - d(tb)
    t0 = ta.min() - 2*H_S*1000
    G = int(np.ceil((tb.max() - t0)/(H_S*1000))) + 3
    ua, ub = (ta - t0)/(H_S*1000), (tb - t0)/(H_S*1000)
    ja, fa, jb, fb = np.floor(ua).astype(int), ua - np.floor(ua), np.floor(ub).astype(int), ub - np.floor(ub)
    nodes = np.stack([ja, ja + 1, jb, jb + 1], 1)
    coef = np.stack([1 - fa, fa, -(1 - fb), -fb], 1)

    # dominant layer subtracted (world-frame chain interpolated on the grid, clamped)
    t_grid = t0 + np.arange(G)*H_S*1000
    t_sample = t_frame
    d_dom = np.stack([np.interp(t_grid, t_sample, dw[:, c]) for c in range(3)], 1)
    Delta = np.einsum('nk,nkj->nj', coef, d_dom[nodes])
    res = res0 - np.cross(np.einsum('nji,nj->ni', Rb, Delta), p)

    # equal-area weights per pair and cell, noise floor
    key = pair*1000 + cell
    _, inv, cnt = np.unique(key, return_inverse=True, return_counts=True)
    area = 1.0/cnt[inv]
    area = area/np.mean(area)
    floor = NOISE_FLOOR_PX/FOCAL
    w0 = area/floor**2
    Pm = np.eye(3) - p[:, :, None]*p[:, None, :]
    Pw = np.einsum('nij,njk,nlk->nil', Rb, Pm, Rb)

    # held-out motion each half shows in the render: the local true error at the frames, plus noise (and only noise)
    tf = t_frame[1:]
    d_true, _ = true_error(t_grid/1000)
    cam_grid = camera(t_grid/1000)
    local = np.einsum('nji,nj->ni', cam_grid, d_true - d_true[0])
    local_f = np.stack([np.interp(tf, t_grid, local[:, c]) for c in range(3)], 1)
    out = {h: local_f + rng.normal(0, 2e-5, local_f.shape) for h in (0, 1)}
    out_noise = {h: rng.normal(0, 2e-5, local_f.shape).cumsum(0)*0.05 for h in (0, 1)}

    def proxy(d, outx):
        dl = np.einsum('nji,nj->ni', cam_grid, d - d[0])
        dlf = np.stack([np.interp(tf, t_grid, dl[:, c]) for c in range(3)], 1)
        left = (outx - dlf)*np.array([FOCAL, FOCAL, R_DIAG])
        keep = (tf > tf[0] + 1000) & (tf < tf[-1] - 1000)
        return np.array([np.sqrt(np.mean(band(left, lo, min(hi, .45*FPS), FPS)[keep]**2, 0)) for lo, hi in BANDS])

    halves = {}
    for h in (0, 1):
        m = half == h
        halves[h] = assemble(G, p[m], res[m], w0[m], Rb[m], nodes[m], coef[m], Pw[m])
    u_all = 6*max(int(np.max(nodes.max(1) - nodes.min(1))) + 1, 3) + 5

    def select(outs):
        grid = [(0.0, TAUS[0])] + [(s, t) for t in TAUS for s in SIGMAS[1:]]
        scores = {}
        for sig, tau in grid:
            total = 0.0
            for est, held in ((1, 0), (0, 1)):
                ab, ev, u = halves[est]
                d, _ = solve(ab, ev, u, G, sig, tau)
                ref = proxy(np.zeros((G, 3)), outs[held])
                total += np.mean(np.log(proxy(d, outs[held])/ref))
            scores[(sig, tau)] = total/2
        best = min(scores.items(), key=lambda kv: (kv[1], kv[0][0], kv[0][1]))[0]
        return best, [scores[g] for g in grid]

    (sig, tau), table = select(out)
    (sig0, tau0), table0 = select(out_noise)
    print('selected with the error visible', np.degrees(sig), tau, '; with noise only', np.degrees(sig0), tau0)

    # final estimate from all points, robustly reweighted twice
    w = w0.copy()
    bounds = np.r_[0, np.flatnonzero(np.diff(pair)) + 1, len(pair)]
    for it in range(2):
        ab, ev, u = assemble(G, p, res, w, Rb, nodes, coef, Pw)
        d, eta = solve(ab, ev, u, G, sig, tau)
        if eta is None:
            break
        tot = d + eta
        Dl = np.einsum('nk,nkj->nj', coef, tot[nodes])
        e = res - np.cross(np.einsum('nji,nj->ni', Rb, Dl), p)
        en = np.linalg.norm(e, axis=1)
        for k in range(len(bounds) - 1):
            s = slice(bounds[k], bounds[k + 1])
            sigma = max(1.4826*np.median(en[s]), floor)
            w[s] = area[s]/(sigma**2*(1 + (en[s]/(ROBUST_K*sigma))**2))
    # recovery where d is observable: the interior (1 s off both ends), in d's own band (2-8 Hz); eta's slow part not in d
    interior = (t_grid > t_grid[0] + 1000) & (t_grid < t_grid[-1] - 1000)
    fs = 1000/(H_S*1000)
    for c, name in enumerate(['pitch', 'yaw', 'roll']):
        e = band((d - d_true)[:, c], 2.0, 8.0, fs)[interior]
        t = band(d_true[:, c], 2.0, 8.0, fs)[interior]
        slow = sosfiltfilt(butter(3, 0.2, fs=fs, output='sos'), d[:, c])[interior]
        slow_eta = sosfiltfilt(butter(3, 0.2, fs=fs, output='sos'), eta[:, c])[interior]
        print(f'{name}: 2-8 Hz relative error {np.sqrt(np.mean(e**2)/np.mean(t**2))*100:.1f}%, d below 0.2 Hz '
              f'{np.mean(slow**2)/np.mean(slow_eta**2)*100:.2f}% of eta')

    # band filter check vector
    xb = rng.normal(0, 1, (200, 3)).cumsum(0)
    yb = np.concatenate([band(xb, lo, min(hi, .45*FPS), FPS) for lo, hi in BANDS], 1)

    def f64(a):
        return np.ascontiguousarray(np.asarray(a, '<f8')).tobytes()
    out_bytes = bytearray()
    out_bytes += struct.pack('<8sIIIIIdddd', b'RESROT01', len(P), G, FRAMES, len(table), len(xb), t0, FOCAL, FPS, floor)
    out_bytes += f64(P)
    out_bytes += f64(np.c_[t_sample, seg, lm, beta, cam_q])
    out_bytes += f64(np.c_[t_grid, cam_grid.reshape(G, 9)])
    out_bytes += f64(tf)
    for h in (0, 1):
        out_bytes += f64(out[h])
    for h in (0, 1):
        out_bytes += f64(out_noise[h])
    out_bytes += f64(d_dom)
    out_bytes += f64(res)
    out_bytes += f64(area)
    out_bytes += f64([np.degrees(sig), tau, np.degrees(sig0), tau0, u_all])
    out_bytes += f64(table)
    out_bytes += f64(table0)
    out_bytes += f64(d)
    out_bytes += f64(xb)
    out_bytes += f64(yb)
    path = Path(__file__).with_name('residual_rotation_fixture.bin')
    path.write_bytes(bytes(out_bytes))
    print(path, len(out_bytes), 'bytes')


if __name__ == '__main__':
    main()
