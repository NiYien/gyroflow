// SPDX-License-Identifier: GPL-3.0-or-later
// Package the approved FCP artwork for legacy, adaptive and themed launchers.
import Foundation
import CoreGraphics
import ImageIO

func fail(_ message: String) -> Never {
    fputs(message + "\n", stderr)
    exit(1)
}

guard CommandLine.arguments.count == 3,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL, nil),
      let artwork = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
    fail("Usage: swift prepare_android_icons.swift <FCP artwork.png> <Android res directory>")
}
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
let background = CGColor(red: 244/255, green: 245/255, blue: 247/255, alpha: 1)

func canvas(_ size: Int) -> CGContext {
    guard let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
        bytesPerRow: size * 4, space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue) else {
        fail("Could not allocate icon canvas")
    }
    ctx.interpolationQuality = .high
    return ctx
}

// Keep every visible source pixel inside the protected 66 dp diameter circle.
let sourceSize = max(artwork.width, artwork.height)
let probe = canvas(sourceSize)
let sourceRect = CGRect(x: CGFloat(sourceSize - artwork.width) / 2,
    y: CGFloat(sourceSize - artwork.height) / 2,
    width: CGFloat(artwork.width), height: CGFloat(artwork.height))
probe.draw(artwork, in: sourceRect)
let pixels = probe.data!.assumingMemoryBound(to: UInt8.self)
var radius: Double = 0
for y in 0..<sourceSize {
    for x in 0..<sourceSize where pixels[(y * sourceSize + x) * 4 + 3] > 0 {
        radius = max(radius, hypot(Double(x) + 0.5 - Double(sourceSize) / 2,
            Double(y) + 0.5 - Double(sourceSize) / 2))
    }
}
guard radius > 0 else { fail("The source artwork is empty") }
let adaptiveScale = 31.5 / radius

func write(_ ctx: CGContext, _ path: URL) {
    guard let image = ctx.makeImage(),
          let dest = CGImageDestinationCreateWithURL(path as CFURL, "public.png" as CFString, 1, nil) else {
        fail("Could not create " + path.path)
    }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fail("Could not write " + path.path) }
}

for (density, scale) in [("mdpi", 1.0), ("hdpi", 1.5), ("xhdpi", 2.0), ("xxhdpi", 3.0), ("xxxhdpi", 4.0)] {
    let folder = output.appendingPathComponent("mipmap-" + density, isDirectory: true)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let adaptiveSize = Int(108 * scale)
    let frame = CGRect(x: 0, y: 0, width: adaptiveSize, height: adaptiveSize)
    let width = Double(artwork.width) * adaptiveScale * scale
    let height = Double(artwork.height) * adaptiveScale * scale
    let rect = CGRect(x: (Double(adaptiveSize) - width) / 2,
        y: (Double(adaptiveSize) - height) / 2, width: width, height: height)
    let foreground = canvas(adaptiveSize)
    foreground.draw(artwork, in: rect)
    write(foreground, folder.appendingPathComponent("icon_foreground.png"))

    let mono = canvas(adaptiveSize)
    mono.draw(foreground.makeImage()!, in: frame)
    mono.setBlendMode(.sourceIn)
    mono.setFillColor(CGColor(gray: 1, alpha: 1))
    mono.fill(frame)
    write(mono, folder.appendingPathComponent("icon_monochrome.png"))

    let back = canvas(adaptiveSize)
    back.setFillColor(background)
    back.fill(frame)
    write(back, folder.appendingPathComponent("icon_background.png"))

    let legacySize = Int(48 * scale)
    let legacy = canvas(legacySize)
    legacy.setFillColor(background)
    legacy.fill(CGRect(x: 0, y: 0, width: legacySize, height: legacySize))
    let legacyScale = Double(legacySize) * 864 / 1024 / Double(sourceSize)
    let legacyWidth = Double(artwork.width) * legacyScale
    let legacyHeight = Double(artwork.height) * legacyScale
    legacy.draw(artwork, in: CGRect(x: (Double(legacySize) - legacyWidth) / 2,
        y: (Double(legacySize) - legacyHeight) / 2, width: legacyWidth, height: legacyHeight))
    write(legacy, folder.appendingPathComponent("icon.png"))
}
print("Generated 20 Android icon assets from the approved FCP artwork.")
