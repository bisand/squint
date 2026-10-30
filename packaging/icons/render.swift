// Renders an SVG into square PNGs, one per size given, keeping transparency.
//
// qlmanage would be the obvious thumbnailer and is the wrong one: it draws the
// SVG over white, which fills in the corners the rounded tile leaves empty.
import AppKit

let args = CommandLine.arguments
guard args.count >= 4, let image = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write(Data("usage: render.swift SVG OUTDIR SIZE...\n".utf8))
    exit(1)
}
let dir = args[2]
for arg in args.dropFirst(3) {
    guard let size = Int(arg) else { exit(1) }
    guard let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { exit(1) }
    rep.size = NSSize(width: size, height: size)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: size, height: size),
               from: .zero, operation: .copy, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()

    // The corner outside the tile is the one that silently came back white.
    if let corner = rep.colorAt(x: 0, y: 0), corner.alphaComponent > 0.01 {
        FileHandle.standardError.write(Data("render.swift: \(size)px has an opaque corner\n".utf8))
        exit(1)
    }
    guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
    try png.write(to: URL(fileURLWithPath: "\(dir)/\(size).png"))
}
