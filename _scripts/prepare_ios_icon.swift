// SPDX-License-Identifier: GPL-3.0-or-later
// Convert the supplied FCP artwork to an opaque, square iOS asset.
import Foundation
import CoreGraphics
import ImageIO

guard CommandLine.arguments.count == 3,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL, nil),
      let artwork = CGImageSourceCreateImageAtIndex(source, 0, nil),
      let context = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
          bytesPerRow: 4096, space: CGColorSpaceCreateDeviceRGB(),
          bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { exit(1) }
context.setFillColor(CGColor(red: 244/255, green: 245/255, blue: 247/255, alpha: 1))
context.fill(CGRect(x: 0, y: 0, width: 1024, height: 1024))
context.interpolationQuality = .high
context.draw(artwork, in: CGRect(x: 80, y: 80, width: 864, height: 864))
guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(URL(fileURLWithPath: CommandLine.arguments[2]) as CFURL,
          "public.png" as CFString, 1, nil) else { exit(1) }
CGImageDestinationAddImage(destination, image, nil)
if !CGImageDestinationFinalize(destination) { exit(1) }
