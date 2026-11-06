// Draws the DMG window background: CRT black with phosphor green, matching
// the app icon. Icons sit at (180, 210) and (480, 210) in a 660x420 window
// (see settings.py); the arrow points from one to the other.
//
// Usage: background <out.png> <scale>
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let scale = Double(args[2]) else {
    FileHandle.standardError.write("usage: background <out.png> <scale>\n".data(using: .utf8)!)
    exit(2)
}
let size = CGSize(width: 660, height: 420)

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: a)
}
let phosphor = rgb(0x33F36A)

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale),
                           pixelsHigh: Int(size.height * scale), bitsPerSample: 8, samplesPerPixel: 4,
                           hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                           bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = size
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext

// Flip so y grows downward like Finder's icon coordinates.
ctx.translateBy(x: 0, y: size.height)
ctx.scaleBy(x: 1, y: -1)

// Background: vertical CRT gradient plus faint scan lines.
NSGradient(starting: rgb(0x131D16), ending: rgb(0x050806))!.draw(in: CGRect(origin: .zero, size: size), angle: 90)
rgb(0x000000, 0.18).setFill()
var y: CGFloat = 0
while y < size.height {
    CGRect(x: 0, y: y, width: size.width, height: 1).fill()
    y += 3
}

// Soft glow behind the app icon slot.
let glow = NSGradient(colors: [rgb(0x33F36A, 0.16), rgb(0x33F36A, 0)])!
glow.draw(fromCenter: CGPoint(x: 180, y: 210), radius: 0, toCenter: CGPoint(x: 180, y: 210), radius: 150, options: [])

func text(_ s: String, _ font: NSFont, _ color: NSColor, centerX: CGFloat, top: CGFloat, kern: CGFloat = 0) {
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color, .kern: kern]
    let str = NSAttributedString(string: s, attributes: attrs)
    let w = str.size().width
    // Drawing in a flipped CTM: flip locally so glyphs are upright.
    NSGraphicsContext.saveGraphicsState()
    let t = NSAffineTransform()
    t.translateX(by: centerX - w / 2, yBy: top + font.ascender)
    t.scaleX(by: 1, yBy: -1)
    t.concat()
    str.draw(at: .zero)
    NSGraphicsContext.restoreGraphicsState()
}

text("sudopen", NSFont.monospacedSystemFont(ofSize: 30, weight: .semibold), phosphor, centerX: 330, top: 38, kern: 1)
text("open any file, fast", NSFont.monospacedSystemFont(ofSize: 12, weight: .regular), rgb(0xA6FFBC, 0.55), centerX: 330, top: 82)

// Arrow from the app slot to the Applications slot.
let arrow = NSBezierPath()
arrow.move(to: CGPoint(x: 268, y: 210))
arrow.line(to: CGPoint(x: 384, y: 210))
arrow.lineWidth = 4
arrow.lineCapStyle = .round
let dashes: [CGFloat] = [2, 10]
arrow.setLineDash(dashes, count: 2, phase: 0)
phosphor.withAlphaComponent(0.85).setStroke()
arrow.stroke()
let head = NSBezierPath()
head.move(to: CGPoint(x: 380, y: 197))
head.line(to: CGPoint(x: 396, y: 210))
head.line(to: CGPoint(x: 380, y: 223))
head.lineWidth = 4
head.lineCapStyle = .round
head.lineJoinStyle = .round
head.stroke()

text("drag sudopen into Applications", NSFont.systemFont(ofSize: 13, weight: .medium), rgb(0xDCE8DF, 0.75), centerX: 330, top: 340)

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[1]))
