// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

.pragma library

// Mounting position presets for NiYien A1 external IMU device.
// 16 combinations: 4 faces (top/bottom/left/right) × 4 rotations (0°/+90°/-90°/180°).
// Orientation strings derived from R_total = Rz(roll) × Ry(yaw).
// Reference: device on top, LED facing camera front (lens direction) = 0°.

var presets = {
    "top_0":      "XYZ",
    "top_90":     "ZYx",
    "top_-90":    "zYX",
    "top_180":    "xYz",

    "bottom_0":   "xyZ",
    "bottom_90":  "zyx",
    "bottom_-90": "ZyX",
    "bottom_180": "Xyz",

    "left_0":     "YxZ",
    "left_90":    "Yzx",
    "left_-90":   "YZX",
    "left_180":   "YXz",

    "right_0":    "yXZ",
    "right_90":   "yZx",
    "right_-90":  "yzX",
    "right_180":  "yxz"
};

// Reverse lookup: orientation string → { face, rotation }
var _reverseMap = {};
(function() {
    var faces = ["top", "bottom", "left", "right"];
    var rotations = [0, 90, -90, 180];
    for (var f = 0; f < faces.length; f++) {
        for (var r = 0; r < rotations.length; r++) {
            var key = faces[f] + "_" + rotations[r];
            _reverseMap[presets[key]] = { face: faces[f], rotation: rotations[r] };
        }
    }
})();

function getOrientationString(face, rotation) {
    var key = face + "_" + rotation;
    return presets[key] || null;
}

function reverseMapping(orientationString) {
    return _reverseMap[orientationString] || null;
}
