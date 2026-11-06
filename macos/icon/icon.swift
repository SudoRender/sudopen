// sudopen app icon, drawn with Core Graphics (no fonts, no external assets).
//
//   swift macos/icon/icon.swift iconset <dir.iconset> [variant]
//       writes the 10 PNGs iconutil expects (16..512 at 1x and 2x)
//   swift macos/icon/icon.swift png <size> <out.png> [variant]
//       one PNG, for previews
//
// Variants: root-page (the app icon), crt-prompt, speed-page.
//
// Layout follows the macOS (Big Sur+) template: 1024 canvas, 824x824 body
// centered, continuous-corner radius 185.4, soft shadow below. Everything is
// drawn in a 1024-unit space and rasterised directly at each target size, so
// small sizes stay sharp; below 64 px the details (scan lines, glow) are cut
// back and the prompt gets bolder.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

// MARK: palette (SudoRender: phosphor green on CRT black)

let ink: UInt32 = 0x07140B          // near-black green, the "#_" on the page
let crtTop: UInt32 = 0x131D16
let crtBottom: UInt32 = 0x040605
let phosphorHi: UInt32 = 0xA6FFBC
let phosphor: UInt32 = 0x33F36A
let phosphorLo: UInt32 = 0x1FCB52

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: a)
}

let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

// MARK: geometry

let bodyRect = CGRect(x: 100, y: 100, width: 824, height: 824)
let bodyRadius: CGFloat = 185.4

/// Rounded rect with continuous ("squircle") corners, the shape of macOS app icons.
func squircle(_ r: CGRect, _ c: CGFloat) -> CGPath {
    // One corner, expressed for the top-right; the others are mirrored.
    // (dx from the right edge, dy from the top edge), in units of c.
    let k: [(CGFloat, CGFloat)] = [
        (1.52866483, 0), (1.08849323, 0), (0.86840689, 0), (0.63149399, 0.07491100),
        (0.37282610, 0.16905788), (0.16905788, 0.37282610), (0.07491100, 0.63149399),
        (0, 0.86840689), (0, 1.08849323), (0, 1.52866483),
    ]
    func corner(_ p: CGMutablePath, _ map: (CGFloat, CGFloat) -> CGPoint, first: Bool) {
        let pts = k.map { map($0.0 * c, $0.1 * c) }
        if first { p.move(to: pts[0]) } else { p.addLine(to: pts[0]) }
        p.addCurve(to: pts[3], control1: pts[1], control2: pts[2])
        p.addCurve(to: pts[6], control1: pts[4], control2: pts[5])
        p.addCurve(to: pts[9], control1: pts[7], control2: pts[8])
    }
    let p = CGMutablePath()
    corner(p, { dx, dy in CGPoint(x: r.maxX - dx, y: r.minY + dy) }, first: true)   // top right
    corner(p, { dx, dy in CGPoint(x: r.maxX - dy, y: r.maxY - dx) }, first: false)  // bottom right
    corner(p, { dx, dy in CGPoint(x: r.minX + dx, y: r.maxY - dy) }, first: false)  // bottom left
    corner(p, { dx, dy in CGPoint(x: r.minX + dy, y: r.minY + dx) }, first: false)  // top left
    p.closeSubpath()
    return p
}

/// "#" from four bars, verticals slanted like a monospace glyph.
func hashPath(_ b: CGRect, t: CGFloat, slant: CGFloat) -> CGPath {
    let p = CGMutablePath()
    for fx in [0.29, 0.71] as [CGFloat] {
        let cx = b.minX + b.width * fx
        p.addLines(between: [
            CGPoint(x: cx - t / 2 + slant, y: b.minY), CGPoint(x: cx + t / 2 + slant, y: b.minY),
            CGPoint(x: cx + t / 2 - slant, y: b.maxY), CGPoint(x: cx - t / 2 - slant, y: b.maxY),
        ])
        p.closeSubpath()
    }
    for fy in [0.34, 0.66] as [CGFloat] {
        let cy = b.minY + b.height * fy
        p.addRect(CGRect(x: b.minX + slant * (1 - 2 * fy), y: cy - t / 2, width: b.width, height: t))
    }
    return p
}

/// Page outline with the top-right corner folded, plus the folded flap.
func pagePaths(_ r: CGRect, fold f: CGFloat, radius rr: CGFloat) -> (page: CGPath, flap: CGPath) {
    let p = CGMutablePath()
    p.move(to: CGPoint(x: r.minX + rr, y: r.minY))
    p.addLine(to: CGPoint(x: r.maxX - f, y: r.minY))
    p.addLine(to: CGPoint(x: r.maxX, y: r.minY + f))
    p.addArc(tangent1End: CGPoint(x: r.maxX, y: r.maxY), tangent2End: CGPoint(x: r.minX, y: r.maxY), radius: rr)
    p.addArc(tangent1End: CGPoint(x: r.minX, y: r.maxY), tangent2End: CGPoint(x: r.minX, y: r.minY), radius: rr)
    p.addArc(tangent1End: CGPoint(x: r.minX, y: r.minY), tangent2End: CGPoint(x: r.maxX, y: r.minY), radius: rr)
    p.closeSubpath()

    let q = CGMutablePath()
    let k = rr * 0.7
    q.move(to: CGPoint(x: r.maxX - f, y: r.minY))
    q.addLine(to: CGPoint(x: r.maxX - f, y: r.minY + f - k))
    q.addQuadCurve(to: CGPoint(x: r.maxX - f + k, y: r.minY + f), control: CGPoint(x: r.maxX - f, y: r.minY + f))
    q.addLine(to: CGPoint(x: r.maxX, y: r.minY + f))
    q.closeSubpath()
    return (p, q)
}

// MARK: drawing helpers

struct Pen {
    let ctx: CGContext
    let px: CGFloat           // output size in pixels
    var s: CGFloat { px / 1024 }
    var small: Bool { px <= 32 }

    func gradient(_ colors: [UInt32], _ locs: [CGFloat]? = nil) -> CGGradient {
        let l = locs ?? colors.indices.map { CGFloat($0) / CGFloat(max(colors.count - 1, 1)) }
        return CGGradient(colorsSpace: srgb, colors: colors.map { rgb($0) } as CFArray, locations: l)!
    }

    /// Fill `path` with a vertical gradient between y0 and y1.
    func fill(_ path: CGPath, _ colors: [UInt32], y0: CGFloat, y1: CGFloat) {
        ctx.saveGState()
        ctx.addPath(path)
        ctx.clip()
        ctx.drawLinearGradient(gradient(colors), start: CGPoint(x: 0, y: y0), end: CGPoint(x: 0, y: y1),
                               options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
        ctx.restoreGState()
    }

    func fill(_ path: CGPath, _ color: CGColor) {
        ctx.addPath(path)
        ctx.setFillColor(color)
        ctx.fillPath()
    }

    /// Shadow or glow under `path`. Offsets are in 1024 units, y down.
    func shadow(_ path: CGPath, dy: CGFloat, blur: CGFloat, _ color: CGColor, dx: CGFloat = 0) {
        ctx.saveGState()
        // Shadow offsets live in device space (y up) and ignore the CTM.
        ctx.setShadow(offset: CGSize(width: dx * s, height: -dy * s), blur: blur * s, color: color)
        fill(path, rgb(0, 1))
        ctx.restoreGState()
    }

    func glow(_ path: CGPath, blur: CGFloat, _ color: CGColor) {
        ctx.saveGState()
        ctx.setShadow(offset: .zero, blur: blur * s, color: color)
        fill(path, color.copy(alpha: 1)!)
        ctx.restoreGState()
    }

    func radialGlow(center: CGPoint, radius: CGFloat, _ color: UInt32, _ alpha: CGFloat) {
        ctx.saveGState()
        ctx.addPath(squircle(bodyRect, bodyRadius))
        ctx.clip()
        let g = CGGradient(colorsSpace: srgb, colors: [rgb(color, alpha), rgb(color, 0)] as CFArray, locations: [0, 1])!
        ctx.drawRadialGradient(g, startCenter: center, startRadius: 0, endCenter: center, endRadius: radius, options: [])
        ctx.restoreGState()
    }

    /// The icon body: shadow, dark CRT fill.
    func body(top: UInt32 = crtTop, bottom: UInt32 = crtBottom) {
        let b = squircle(bodyRect, bodyRadius)
        shadow(b, dy: 10, blur: small ? 10 : 22, rgb(0, small ? 0.25 : 0.32))
        fill(b, [top, bottom], y0: bodyRect.minY, y1: bodyRect.maxY)
    }

    /// Faint light rim along the top of the body.
    func rim() {
        let w: CGFloat = small ? 1.0 / s : 3
        let b = squircle(bodyRect.insetBy(dx: w / 2, dy: w / 2), bodyRadius - w / 2)
        ctx.saveGState()
        ctx.addPath(b)
        ctx.setLineWidth(w)
        ctx.replacePathWithStrokedPath()
        ctx.clip()
        let g = CGGradient(colorsSpace: srgb,
                           colors: [rgb(0xFFFFFF, 0.20), rgb(0xFFFFFF, 0.05), rgb(0xFFFFFF, 0)] as CFArray,
                           locations: [0, 0.4, 1])!
        ctx.drawLinearGradient(g, start: CGPoint(x: 0, y: bodyRect.minY), end: CGPoint(x: 0, y: bodyRect.maxY), options: [])
        ctx.restoreGState()
    }

    /// CRT scan lines over the body. Only where they resolve to whole pixels.
    func scanlines(alpha: CGFloat) {
        guard px >= 512 else { return }
        ctx.saveGState()
        ctx.addPath(squircle(bodyRect, bodyRadius))
        ctx.clip()
        ctx.setFillColor(rgb(0, alpha))
        var y = bodyRect.minY
        while y < bodyRect.maxY {
            ctx.fill(CGRect(x: bodyRect.minX, y: y, width: bodyRect.width, height: 4))
            y += 8
        }
        ctx.restoreGState()
    }

    /// "#_" root prompt. `x`,`y`: top-left of the "#"; `h`: glyph height.
    func prompt(x: CGFloat, y: CGFloat, h: CGFloat) -> CGPath {
        let w = h * 0.9, t = h * 0.19
        let p = CGMutablePath()
        p.addPath(hashPath(CGRect(x: x, y: y, width: w, height: h), t: t, slant: h * 0.065))
        p.addRect(CGRect(x: x + w + h * 0.13, y: y + h - t, width: h * 0.6, height: t))
        return p
    }
}

// MARK: variants

/// Pixel-grid version of the page and "#_" for 16 and 32 px, where the
/// vector glyph would blur into a smudge. Rows are drawn top to bottom.
let pixelPrompt: [Int: (page: CGRect, fold: CGFloat, at: CGPoint, rows: [String])] = [
    16: (CGRect(x: 4, y: 3, width: 8, height: 10), 2.5, CGPoint(x: 5, y: 6), [
        ".#.#.",
        "#####",
        ".#.#.",
        "#####",
        ".#.#.",
    ]),
    32: (CGRect(x: 8, y: 6, width: 16, height: 20), 4.5, CGPoint(x: 10, y: 11), [
        ".##..##......",
        ".##..##......",
        "########.....",
        "########.....",
        ".##..##......",
        "########.....",
        "########.....",
        ".##..##..####",
        ".##..##..####",
    ]),
]

/// The app icon: a phosphor-green page on a CRT, with a root prompt "#_" on it.
func rootPage(_ pen: Pen) {
    if let grid = pixelPrompt[Int(pen.px)] {
        return rootPagePixels(pen, grid)
    }
    pen.body()
    pen.radialGlow(center: CGPoint(x: 512, y: 520), radius: 470, phosphor, pen.small ? 0.12 : 0.22)

    let pr = CGRect(x: 272, y: 202, width: 480, height: 620)
    let fold: CGFloat = 150
    let (page, flap) = pagePaths(pr, fold: fold, radius: 36)
    pen.glow(page, blur: pen.small ? 24 : 72, rgb(phosphor, pen.small ? 0.45 : 0.7))
    pen.fill(page, [phosphorHi, phosphor, phosphorLo], y0: pr.minY, y1: pr.maxY)

    // Folded corner: darker underside, shading toward the crease.
    pen.shadow(flap, dy: 8, blur: 16, rgb(0x003314, 0.45), dx: -6)
    let ctx = pen.ctx
    ctx.saveGState()
    ctx.addPath(flap)
    ctx.clip()
    ctx.drawLinearGradient(pen.gradient([0x14A043, 0x45E978, 0x7FF7A0], [0, 0.6, 1]),
                           start: CGPoint(x: pr.maxX - fold, y: pr.minY + fold),
                           end: CGPoint(x: pr.maxX - fold / 2, y: pr.minY + fold / 2), options: [])
    ctx.restoreGState()

    // "#_" centered on the page, a little below middle (the fold takes the top right).
    let h: CGFloat = pen.small ? 250 : 220
    let groupW = h * 0.9 + h * 0.13 + h * 0.6
    pen.fill(pen.prompt(x: 512 - groupW / 2, y: 536 - h / 2, h: h), rgb(ink))

    pen.scanlines(alpha: 0.10)
    pen.rim()
}

func rootPagePixels(_ pen: Pen, _ g: (page: CGRect, fold: CGFloat, at: CGPoint, rows: [String])) {
    pen.body()
    pen.radialGlow(center: CGPoint(x: 512, y: 520), radius: 470, phosphor, 0.14)
    let ctx = pen.ctx
    ctx.saveGState()
    ctx.scaleBy(x: 1 / pen.s, y: 1 / pen.s)   // now in whole pixels, origin top-left
    let (page, _) = pagePaths(g.page, fold: g.fold, radius: 0.75)
    ctx.saveGState()
    ctx.setShadow(offset: .zero, blur: pen.px / 16, color: rgb(phosphor, 0.6))
    pen.fill(page, rgb(phosphor))
    ctx.restoreGState()
    pen.fill(page, [phosphorHi, phosphor, phosphorLo], y0: g.page.minY, y1: g.page.maxY)
    // folded corner as a darker triangle
    let flap = CGMutablePath()
    flap.addLines(between: [CGPoint(x: g.page.maxX - g.fold, y: g.page.minY),
                            CGPoint(x: g.page.maxX - g.fold, y: g.page.minY + g.fold),
                            CGPoint(x: g.page.maxX, y: g.page.minY + g.fold)])
    flap.closeSubpath()
    pen.fill(flap, rgb(0x18A846))
    ctx.setFillColor(rgb(ink))
    for (r, row) in g.rows.enumerated() {
        for (c, ch) in row.enumerated() where ch == "#" {
            ctx.fill(CGRect(x: g.at.x + CGFloat(c), y: g.at.y + CGFloat(r), width: 1, height: 1))
        }
    }
    ctx.restoreGState()
    pen.rim()
}

/// Alternative: no page, just the glowing root prompt on a CRT.
func crtPrompt(_ pen: Pen) {
    pen.body(top: 0x111A14, bottom: 0x030504)
    pen.radialGlow(center: CGPoint(x: 512, y: 512), radius: 460, phosphor, 0.16)
    let h: CGFloat = pen.small ? 380 : 340
    let groupW = h * 0.9 + h * 0.13 + h * 0.6
    let p = pen.prompt(x: 512 - groupW / 2, y: 512 - h / 2, h: h)
    pen.glow(p, blur: pen.small ? 20 : 64, rgb(phosphor, 0.85))
    pen.fill(p, [phosphorHi, phosphor], y0: 512 - h / 2, y1: 512 + h / 2)
    pen.scanlines(alpha: 0.24)
    pen.rim()
}

/// Alternative: white page with a phosphor lightning bolt, for "ultra fast".
func speedPage(_ pen: Pen) {
    pen.body()
    pen.radialGlow(center: CGPoint(x: 512, y: 520), radius: 470, phosphor, pen.small ? 0.10 : 0.20)
    let pr = CGRect(x: 272, y: 202, width: 480, height: 620)
    let fold: CGFloat = 150
    let (page, flap) = pagePaths(pr, fold: fold, radius: 36)
    pen.shadow(page, dy: 14, blur: 36, rgb(0, 0.5))
    pen.fill(page, [0xFFFFFF, 0xE2EBE4], y0: pr.minY, y1: pr.maxY)
    pen.shadow(flap, dy: 8, blur: 16, rgb(0, 0.22), dx: -6)
    let ctx = pen.ctx
    ctx.saveGState()
    ctx.addPath(flap)
    ctx.clip()
    ctx.drawLinearGradient(pen.gradient([0xC4D3C8, 0xF2F7F3, 0xFFFFFF], [0, 0.6, 1]),
                           start: CGPoint(x: pr.maxX - fold, y: pr.minY + fold),
                           end: CGPoint(x: pr.maxX - fold / 2, y: pr.minY + fold / 2), options: [])
    ctx.restoreGState()

    let b = CGMutablePath()
    b.addLines(between: [
        CGPoint(x: 572, y: 300), CGPoint(x: 370, y: 572), CGPoint(x: 498, y: 572),
        CGPoint(x: 446, y: 752), CGPoint(x: 660, y: 466), CGPoint(x: 534, y: 466),
    ])
    b.closeSubpath()
    pen.shadow(b, dy: 6, blur: 14, rgb(0x0A6B2C, 0.35))
    pen.fill(b, [0x3BF070, 0x14B347], y0: 300, y1: 752)
    pen.rim()
}

let variants: [String: (Pen) -> Void] = [
    "root-page": rootPage,
    "crt-prompt": crtPrompt,
    "speed-page": speedPage,
]

// MARK: output

func render(_ px: Int, _ draw: (Pen) -> Void) -> CGImage {
    let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
                        space: srgb, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let s = CGFloat(px) / 1024
    ctx.translateBy(x: 0, y: CGFloat(px))
    ctx.scaleBy(x: s, y: -s)   // 1024 units, origin top-left
    ctx.setShouldAntialias(true)
    draw(Pen(ctx: ctx, px: CGFloat(px)))
    return ctx.makeImage()!
}

func writePNG(_ img: CGImage, _ path: String) {
    let url = URL(fileURLWithPath: path)
    guard let d = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
        fatalError("cannot write \(path)")
    }
    CGImageDestinationAddImage(d, img, nil)
    guard CGImageDestinationFinalize(d) else { fatalError("cannot write \(path)") }
}

func usage() -> Never {
    let me = "swift macos/icon/icon.swift"
    FileHandle.standardError.write("""
    usage: \(me) iconset <dir.iconset> [variant]
           \(me) png <size> <out.png> [variant]
    variants: \(variants.keys.sorted().joined(separator: ", "))

    """.data(using: .utf8)!)
    exit(2)
}

let args = Array(CommandLine.arguments.dropFirst())
func variant(at i: Int) -> (Pen) -> Void {
    let name = args.count > i ? args[i] : "root-page"
    guard let v = variants[name] else { usage() }
    return v
}

switch args.first {
case "iconset" where args.count >= 2:
    let dir = args[1]
    let draw = variant(at: 2)
    try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    for pt in [16, 32, 128, 256, 512] {
        writePNG(render(pt, draw), "\(dir)/icon_\(pt)x\(pt).png")
        writePNG(render(pt * 2, draw), "\(dir)/icon_\(pt)x\(pt)@2x.png")
    }
case "png" where args.count >= 3:
    guard let px = Int(args[1]), px > 0 else { usage() }
    writePNG(render(px, variant(at: 3)), args[2])
default:
    usage()
}
