#!/usr/bin/env python3
"""Generate 16 isometric 3D mounting position SVG illustrations."""

import math
import os

POSITIONS = ["top", "bottom", "left", "right"]
ROTATIONS = [0, 90, -90, 180]

# Colors
CAM_FRONT = "#44484e"
CAM_TOP = "#55595f"
CAM_SIDE = "#35383d"
CAM_STROKE = "#5a5e64"
LENS_OUTER = "#2a2d32"
LENS_INNER = "#1e2024"
LENS_RING = "#3a3e44"
DEV_FRAME = "#9a9da2"
DEV_FACE = "#2a2a2e"
DEV_DISPLAY = "#1a1a2e"
DEV_DISPLAY_SHEEN = "rgba(120,100,180,0.15)"
DEV_BTN = "#444"
DEV_BTN_ICON = "#ccc"
LED_COLOR = "#4ADE80"
LABEL_COLOR = "rgba(255,255,255,0.35)"


def make_camera_front_right(pos, rot):
    """Camera viewed from upper-right-front. Shows front + top + right."""
    cx, cy = 175, 140  # center of front face
    fw, fh = 150, 105  # front face size
    dx, dy = 45, -26   # depth offset

    # Front face corners
    f_tl = (cx - fw/2, cy - fh/2)
    f_tr = (cx + fw/2, cy - fh/2)
    f_br = (cx + fw/2, cy + fh/2)
    f_bl = (cx - fw/2, cy + fh/2)

    # Top face
    t_fl = f_tl
    t_fr = f_tr
    t_br = (f_tr[0] + dx, f_tr[1] + dy)
    t_bl = (f_tl[0] + dx, f_tl[1] + dy)

    # Right face
    r_ft = f_tr
    r_bt = t_br
    r_bb = (f_br[0] + dx, f_br[1] + dy)
    r_fb = f_br

    # Lens position on front face
    lens_cx = cx
    lens_cy = cy + 2

    return {
        "front": [f_tl, f_tr, f_br, f_bl],
        "top": [t_fl, t_fr, t_br, t_bl],
        "side": [r_ft, r_bt, r_bb, r_fb],
        "side_label": "right",
        "lens": (lens_cx, lens_cy),
        "front_center": (cx, cy),
        # Viewfinder bump on top face
        "vf": [(f_tl[0] + 8, f_tl[1]), (f_tl[0] + 42, f_tl[1]),
               (f_tl[0] + 42 + dx*0.3, f_tl[1] + dy*0.3 - 12),
               (f_tl[0] + 8 + dx*0.3, f_tl[1] + dy*0.3 - 12)],
    }


def make_camera_front_left(pos, rot):
    """Camera viewed from upper-left-front. Shows front + top + left."""
    cx, cy = 225, 140
    fw, fh = 150, 105
    dx, dy = -45, -26

    f_tl = (cx - fw/2, cy - fh/2)
    f_tr = (cx + fw/2, cy - fh/2)
    f_br = (cx + fw/2, cy + fh/2)
    f_bl = (cx - fw/2, cy + fh/2)

    t_fl = f_tl
    t_fr = f_tr
    t_br = (f_tr[0] + dx, f_tr[1] + dy)
    t_bl = (f_tl[0] + dx, f_tl[1] + dy)

    l_ft = f_tl
    l_bt = t_bl
    l_bb = (f_bl[0] + dx, f_bl[1] + dy)
    l_fb = f_bl

    lens_cx = cx
    lens_cy = cy + 2

    return {
        "front": [f_tl, f_tr, f_br, f_bl],
        "top": [t_fl, t_fr, t_br, t_bl],
        "side": [l_ft, l_bt, l_bb, l_fb],
        "side_label": "left",
        "lens": (lens_cx, lens_cy),
        "front_center": (cx, cy),
        "vf": [(f_tr[0] - 8, f_tr[1]), (f_tr[0] - 42, f_tr[1]),
               (f_tr[0] - 42 + dx*0.3, f_tr[1] + dy*0.3 - 12),
               (f_tr[0] - 8 + dx*0.3, f_tr[1] + dy*0.3 - 12)],
    }


def make_camera_bottom_right(pos, rot):
    """Camera viewed from lower-right-front. Shows front + bottom + right."""
    cx, cy = 175, 135
    fw, fh = 150, 105
    dx, dy = 45, 26  # depth goes DOWN for bottom view

    f_tl = (cx - fw/2, cy - fh/2)
    f_tr = (cx + fw/2, cy - fh/2)
    f_br = (cx + fw/2, cy + fh/2)
    f_bl = (cx - fw/2, cy + fh/2)

    # Bottom face (below front)
    b_fl = f_bl
    b_fr = f_br
    b_br = (f_br[0] + dx, f_br[1] + dy)
    b_bl = (f_bl[0] + dx, f_bl[1] + dy)

    # Right face
    r_ft = f_tr
    r_bt = (f_tr[0] + dx, f_tr[1] + dy)
    r_bb = b_br
    r_fb = f_br

    lens_cx = cx
    lens_cy = cy - 2

    return {
        "front": [f_tl, f_tr, f_br, f_bl],
        "top": [b_fl, b_fr, b_br, b_bl],  # reuse "top" key for bottom face
        "side": [r_ft, r_bt, r_bb, r_fb],
        "side_label": "right",
        "lens": (lens_cx, lens_cy),
        "front_center": (cx, cy),
        "vf": None,
    }


def pts(points):
    """Convert list of (x,y) tuples to SVG points string."""
    return " ".join(f"{p[0]:.1f},{p[1]:.1f}" for p in points)


def lerp(a, b, t):
    return (a[0] + (b[0]-a[0])*t, a[1] + (b[1]-a[1])*t)


def face_center(corners):
    x = sum(c[0] for c in corners) / len(corners)
    y = sum(c[1] for c in corners) / len(corners)
    return (x, y)


def face_axes(corners):
    """Get U (horizontal) and V (depth) axes of a parallelogram face."""
    # corners: [front-left, front-right, back-right, back-left]
    u = ((corners[1][0] - corners[0][0]), (corners[1][1] - corners[0][1]))
    v = ((corners[3][0] - corners[0][0]), (corners[3][1] - corners[0][1]))
    return u, v


def device_on_face(face_corners, rotation, is_side=False):
    """Compute device polygon and LED position on a face.

    Returns: (device_corners, led_pos, btn_positions, display_corners)
    Device is drawn with features based on rotation.
    """
    center = face_center(face_corners)
    u, v = face_axes(face_corners)

    # Normalize to face size
    u_len = math.sqrt(u[0]**2 + u[1]**2)
    v_len = math.sqrt(v[0]**2 + v[1]**2)

    # Device size relative to face (device is ~40% of face width, ~35% of depth)
    if is_side:
        dw, dh = 0.35, 0.40  # narrower on side face
    else:
        dw, dh = 0.40, 0.55

    # Device corners in face-local UV space (-0.5 to 0.5)
    # Rotation rotates the device in UV space
    rad = math.radians(rotation)
    cos_r, sin_r = math.cos(rad), math.sin(rad)

    # Device rectangle corners in local space (before rotation)
    local_corners = [
        (-dw/2, -dh/2), (dw/2, -dh/2), (dw/2, dh/2), (-dw/2, dh/2)
    ]

    # LED at "front" of device (negative V direction in local space = towards camera front)
    led_local = (0.12, -dh/2 + 0.04)

    # Button positions (left side of device)
    btn_locals = [
        (-dw/2 + 0.06, -dh/4),
        (-dw/2 + 0.06, 0),
        (-dw/2 + 0.06, dh/4),
    ]

    # Display area
    disp_locals = [
        (0.0, -dh/2 + 0.06),
        (dw/2 - 0.06, -dh/2 + 0.06),
        (dw/2 - 0.06, dh/2 - 0.06),
        (0.0, dh/2 - 0.06),
    ]

    def rotate_point(px, py):
        return (px * cos_r - py * sin_r, px * sin_r + py * cos_r)

    def to_world(lx, ly):
        wx = center[0] + lx * u[0] + ly * v[0]
        wy = center[1] + lx * u[1] + ly * v[1]
        return (wx, wy)

    dev_world = [to_world(*rotate_point(lx, ly)) for lx, ly in local_corners]
    led_world = to_world(*rotate_point(*led_local))
    btn_world = [to_world(*rotate_point(*b)) for b in btn_locals]
    disp_world = [to_world(*rotate_point(*d)) for d in disp_locals]

    return dev_world, led_world, btn_world, disp_world


def generate_svg(position, rotation):
    """Generate one SVG for a specific position and rotation."""

    # Choose camera view based on position
    if position == "left":
        cam = make_camera_front_left(position, rotation)
        dev_face_key = "side"
        is_side = True
    elif position == "bottom":
        cam = make_camera_bottom_right(position, rotation)
        dev_face_key = "top"  # "top" key holds bottom face in this view
        is_side = False
    elif position == "right":
        cam = make_camera_front_right(position, rotation)
        dev_face_key = "side"
        is_side = True
    else:  # top
        cam = make_camera_front_right(position, rotation)
        dev_face_key = "top"
        is_side = False

    dev_face_corners = cam[dev_face_key]
    dev_corners, led_pos, btn_pos, disp_corners = device_on_face(
        dev_face_corners, rotation, is_side
    )

    # Determine face colors based on position
    if position == "bottom":
        top_face_color = CAM_SIDE  # bottom face is darker
        front_color = CAM_FRONT
        side_color = CAM_SIDE
    else:
        top_face_color = CAM_TOP
        front_color = CAM_FRONT
        side_color = CAM_SIDE

    # Build SVG
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 300" fill="none">
  <defs>
    <radialGradient id="lens-grad" cx="45%" cy="42%">
      <stop offset="0%" stop-color="#555"/>
      <stop offset="60%" stop-color="#1e2024"/>
      <stop offset="100%" stop-color="#111"/>
    </radialGradient>
    <linearGradient id="display-grad" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#1a1a3a"/>
      <stop offset="100%" stop-color="#2a2040"/>
    </linearGradient>
  </defs>
'''

    # Draw camera body — order matters for proper layering
    if position == "bottom":
        # Front face first, then side, then bottom
        svg += f'  <polygon points="{pts(cam["front"])}" fill="{front_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["side"])}" fill="{side_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["top"])}" fill="{top_face_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
    elif position == "left":
        # Side (left) first, then top, then front
        svg += f'  <polygon points="{pts(cam["side"])}" fill="{side_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["top"])}" fill="{top_face_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["front"])}" fill="{front_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
    else:
        # Side (right) first, then top, then front
        svg += f'  <polygon points="{pts(cam["side"])}" fill="{side_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["top"])}" fill="{top_face_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'
        svg += f'  <polygon points="{pts(cam["front"])}" fill="{front_color}" stroke="{CAM_STROKE}" stroke-width="1.2"/>\n'

    # Viewfinder bump
    if cam["vf"]:
        svg += f'  <polygon points="{pts(cam["vf"])}" fill="#3a3d42" stroke="{CAM_STROKE}" stroke-width="0.8"/>\n'

    # Lens on front face
    lx, ly = cam["lens"]
    svg += f'  <circle cx="{lx:.1f}" cy="{ly:.1f}" r="28" fill="{LENS_OUTER}" stroke="{CAM_STROKE}" stroke-width="1"/>\n'
    svg += f'  <circle cx="{lx:.1f}" cy="{ly:.1f}" r="22" fill="{LENS_RING}" stroke="{LENS_OUTER}" stroke-width="1.5"/>\n'
    svg += f'  <circle cx="{lx:.1f}" cy="{ly:.1f}" r="15" fill="url(#lens-grad)"/>\n'
    svg += f'  <circle cx="{lx:.1f}" cy="{ly:.1f}" r="8" fill="#0a0a0a" opacity="0.8"/>\n'

    # Hot shoe on top (only for top/right views, and not if device is on top)
    if position not in ["top", "bottom"]:
        top_c = face_center(cam["top"])
        u, v = face_axes(cam["top"])
        # Small rectangle centered on top face
        shoe_w, shoe_h = 0.15, 0.12
        shoe_corners = [
            (top_c[0] - shoe_w*u[0] - shoe_h*v[0], top_c[1] - shoe_w*u[1] - shoe_h*v[1]),
            (top_c[0] + shoe_w*u[0] - shoe_h*v[0], top_c[1] + shoe_w*u[1] - shoe_h*v[1]),
            (top_c[0] + shoe_w*u[0] + shoe_h*v[0], top_c[1] + shoe_w*u[1] + shoe_h*v[1]),
            (top_c[0] - shoe_w*u[0] + shoe_h*v[0], top_c[1] - shoe_w*u[1] + shoe_h*v[1]),
        ]
        svg += f'  <polygon points="{pts(shoe_corners)}" fill="#555" stroke="#666" stroke-width="0.5"/>\n'

    # NiYien A1 Device
    svg += f'\n  <!-- NiYien A1 device -->\n'

    # Device frame (silver)
    svg += f'  <polygon points="{pts(dev_corners)}" fill="{DEV_FRAME}" stroke="#777" stroke-width="1.5" stroke-linejoin="round"/>\n'

    # Device face (black, slightly inset)
    inset = 0.85
    dev_center = face_center(dev_corners)
    inset_corners = [(dev_center[0] + (c[0]-dev_center[0])*inset,
                      dev_center[1] + (c[1]-dev_center[1])*inset) for c in dev_corners]
    svg += f'  <polygon points="{pts(inset_corners)}" fill="{DEV_FACE}" stroke="#444" stroke-width="0.5"/>\n'

    # Display
    svg += f'  <polygon points="{pts(disp_corners)}" fill="url(#display-grad)" stroke="#333" stroke-width="0.5" opacity="0.9"/>\n'

    # Buttons
    for bx, by in btn_pos:
        svg += f'  <circle cx="{bx:.1f}" cy="{by:.1f}" r="3" fill="{DEV_BTN}" stroke="#555" stroke-width="0.5"/>\n'

    # LED indicator
    svg += f'  <circle cx="{led_pos[0]:.1f}" cy="{led_pos[1]:.1f}" r="2.5" fill="{LED_COLOR}"/>\n'
    svg += f'  <circle cx="{led_pos[0]:.1f}" cy="{led_pos[1]:.1f}" r="5" fill="{LED_COLOR}" opacity="0.2"/>\n'

    # Labels
    rot_label = f"{rotation}°" if rotation <= 0 else f"+{rotation}°"
    pos_label = {"top": "Top", "bottom": "Bottom", "left": "Left", "right": "Right"}[position]

    # Front direction indicator
    front = cam["front"]
    front_bottom_center = ((front[2][0] + front[3][0])/2, (front[2][1] + front[3][1])/2)
    svg += f'  <text x="{front_bottom_center[0]:.0f}" y="{front_bottom_center[1] + 22:.0f}" '
    svg += f'text-anchor="middle" font-family="sans-serif" font-size="11" fill="{LABEL_COLOR}">'
    svg += f'&#x25B8; Front (Lens)</text>\n'

    svg += '</svg>\n'
    return svg


def main():
    out_dir = os.path.dirname(os.path.abspath(__file__))

    for pos in POSITIONS:
        for rot in ROTATIONS:
            filename = f"mount_{pos}_{rot}.svg"
            filepath = os.path.join(out_dir, filename)
            svg_content = generate_svg(pos, rot)
            with open(filepath, 'w', encoding='utf-8') as f:
                f.write(svg_content)
            print(f"Generated: {filename}")

    print(f"\nDone! 16 SVGs generated in {out_dir}")


if __name__ == "__main__":
    main()
