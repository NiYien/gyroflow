// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick

Canvas {
    id: root
    property string name: ""
    property color color: "#0071e3"
    implicitWidth: 24
    implicitHeight: 24
    onNameChanged: requestPaint()
    onColorChanged: requestPaint()
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()
    onPaint: {
        const c = getContext("2d");
        c.reset(); c.scale(width / 24, height / 24);
        c.strokeStyle = color; c.fillStyle = color; c.lineWidth = 1.7;
        c.lineCap = "round"; c.lineJoin = "round";
        function line(points) {
            c.beginPath(); c.moveTo(points[0], points[1]);
            for (let i = 2; i < points.length; i += 2) c.lineTo(points[i], points[i + 1]);
            c.stroke();
        }
        function circle(x, y, r, fill) { c.beginPath(); c.arc(x, y, r, 0, 2 * Math.PI); fill ? c.fill() : c.stroke(); }
        if (name === "plus") { line([5,12,19,12]); line([12,5,12,19]); }
        else if (name === "close") { line([7,7,17,17]); line([7,17,17,7]); }
        else if (name === "check") line([5,12,10,17,19,7]);
        else if (name === "folder") { line([3,7,3,19,21,19,21,7,12,7,10,4,3,4,3,7]); }
        else if (name === "file") { line([5,3,14,3,19,8,19,21,5,21,5,3]); line([14,3,14,8,19,8]); line([9,13,15,13]); line([9,17,15,17]); }
        else if (name === "photos") { line([3,4,21,4,21,20,3,20,3,4]); circle(8,9,1.5,false); line([3,17,9,12,13,16,17,11,21,15]); }
        else if (name === "search") { circle(10,10,6,false); line([15,15,21,21]); }
        else if (name === "trash") { line([4,6,20,6]); line([9,6,9,3,15,3,15,6]); line([6,6,7,21,17,21,18,6]); line([10,10,10,17]); line([14,10,14,17]); }
        else if (name === "reset" || name === "update") { c.beginPath(); c.arc(12,12,8,-2.4,2.5); c.stroke(); line([4,4,4,10,10,10]); }
        else if (name === "task") { line([9,5,21,5]); line([9,12,21,12]); line([9,19,21,19]); circle(3,5,1,true); circle(3,12,1,true); circle(3,19,1,true); }
        else if (name === "message") { line([3,4,21,4,21,17,9,17,4,21,4,17,3,17,3,4]); line([7,9,17,9]); line([7,13,14,13]); }
        else if (name === "back") line([15,4,7,12,15,20]);
        else if (name === "up") { line([5,11,12,4,19,11]); line([12,4,12,20]); }
        else if (name === "chevron") line([9,6,15,12,9,18]);
        else if (name === "more") { circle(5,12,1.5,true); circle(12,12,1.5,true); circle(19,12,1.5,true); }
        else if (name === "info") { circle(12,12,9,false); circle(12,7.5,1,true); line([12,11,12,17]); }
        else if (name === "settings") {
            circle(12,12,3.4,false);
            c.beginPath();
            for (let i = 0; i <= 48; ++i) {
                const a = i * Math.PI / 24;
                const r = i % 6 >= 2 && i % 6 <= 4 ? 9.8 : 8;
                const x = 12 + Math.sin(a) * r, y = 12 + Math.cos(a) * r;
                i ? c.lineTo(x,y) : c.moveTo(x,y);
            }
            c.closePath(); c.stroke();
        } else if (name === "pause") { c.fillRect(6,5,4,14); c.fillRect(14,5,4,14); }
        else if (name === "play") { c.beginPath(); c.moveTo(7,4); c.lineTo(20,12); c.lineTo(7,20); c.closePath(); c.fill(); }
        else if (name === "previous" || name === "next") {
            if (name === "previous") { c.translate(24,0); c.scale(-1,1); }
            c.beginPath(); c.moveTo(4,5); c.lineTo(16,12); c.lineTo(4,19); c.closePath(); c.fill(); c.fillRect(18,5,2,14);
        }
    }
}
