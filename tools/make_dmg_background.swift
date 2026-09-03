// Génère le fond du DMG (660×400 pt, rendu 2x) : glisser Murmure vers Applications.
import AppKit

let W: CGFloat = 660, H: CGFloat = 400, scale: CGFloat = 2
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: W, height: H)
let gctx = NSGraphicsContext(bitmapImageRep: rep)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = gctx
let ctx = gctx.cgContext
// Repère : coordonnées « depuis le haut » converties en bas-gauche (AppKit).
func R(_ x: CGFloat, _ yTop: CGFloat, _ w: CGFloat, _ h: CGFloat) -> NSRect { NSRect(x: x, y: H - yTop - h, width: w, height: h) }
func Pt(_ x: CGFloat, _ yTop: CGFloat) -> NSPoint { NSPoint(x: x, y: H - yTop) }

// Fond : dégradé sombre
let bg = NSGradient(colors: [NSColor(calibratedRed: 0.13, green: 0.13, blue: 0.15, alpha: 1),
                             NSColor(calibratedRed: 0.07, green: 0.07, blue: 0.09, alpha: 1)])!
bg.draw(in: NSRect(x: 0, y: 0, width: W, height: H), angle: -90)

// Halo discret au centre
ctx.saveGState()
let halo = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
                      colors: [NSColor(calibratedRed: 0.20, green: 0.42, blue: 1.0, alpha: 0.16).cgColor,
                               NSColor.clear.cgColor] as CFArray, locations: [0, 1])!
ctx.drawRadialGradient(halo, startCenter: CGPoint(x: W/2, y: H - 210), startRadius: 0,
                       endCenter: CGPoint(x: W/2, y: H - 210), endRadius: 320, options: [])
ctx.restoreGState()

// Marque : barres d'onde + nom
func bar(_ x: CGFloat, _ cy: CGFloat, _ h: CGFloat, _ w: CGFloat, _ c: NSColor) {
    c.setFill()
    NSBezierPath(roundedRect: R(x, cy - h/2, w, h), xRadius: w/2, yRadius: w/2).fill()
}
let heights: [CGFloat] = [8, 14, 22, 14, 8]
let white = NSColor(calibratedWhite: 0.96, alpha: 1)
for (i, h) in heights.enumerated() { bar(30 + CGFloat(i) * 7.5, 42, h, 4, white) }
let titleAttr: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 17, weight: .semibold), .foregroundColor: white]
let t1 = NSAttributedString(string: "Murmure", attributes: titleAttr); t1.draw(at: Pt(76, 31 + t1.size().height))
let subAttr: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 12, weight: .regular),
    .foregroundColor: NSColor(calibratedWhite: 1, alpha: 0.55)]
let t2 = NSAttributedString(string: "Dictée locale, hors-ligne", attributes: subAttr); t2.draw(at: Pt(77, 53 + t2.size().height))

// Emplacements des icônes (cercles très discrets) — doivent correspondre à tauri.conf.json
let appPt = CGPoint(x: 165, y: 200), folderPt = CGPoint(x: 495, y: 200)
for p in [appPt, folderPt] {
    NSColor(calibratedWhite: 1, alpha: 0.05).setFill()
    NSBezierPath(ovalIn: R(p.x - 78, p.y - 78, 156, 156)).fill()
    NSColor(calibratedWhite: 1, alpha: 0.10).setStroke()
    let ring = NSBezierPath(ovalIn: R(p.x - 78, p.y - 78, 156, 156))
    ring.lineWidth = 1; ring.stroke()
}

// Flèche
let arrow = NSBezierPath()
arrow.move(to: Pt(262, 200)); arrow.line(to: Pt(392, 200))
arrow.move(to: Pt(376, 186)); arrow.line(to: Pt(392, 200)); arrow.line(to: Pt(376, 214))
arrow.lineWidth = 3; arrow.lineCapStyle = .round; arrow.lineJoinStyle = .round
NSColor(calibratedWhite: 1, alpha: 0.75).setStroke(); arrow.stroke()

// Consignes
func centered(_ s: String, y: CGFloat, size: CGFloat, weight: NSFont.Weight, alpha: CGFloat) {
    let a: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: size, weight: weight),
                                            .foregroundColor: NSColor(calibratedWhite: 1, alpha: alpha)]
    let str = NSAttributedString(string: s, attributes: a)
    let sz = str.size()
    str.draw(at: Pt((W - sz.width)/2, y + sz.height))
}
centered("Glissez Murmure dans Applications", y: 300, size: 16, weight: .semibold, alpha: 0.92)
centered("Si macOS bloque l’ouverture : clic droit sur l’app, puis « Ouvrir ».", y: 326, size: 11.5, weight: .regular, alpha: 0.5)
centered("Vos dictées restent sur cet appareil. Aucune donnée n’est envoyée.", y: 344, size: 11.5, weight: .regular, alpha: 0.5)

NSGraphicsContext.restoreGraphicsState()
let png = rep.representation(using: .png, properties: [:])!
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "dmg-background.png"
try! png.write(to: URL(fileURLWithPath: out))
print("écrit \(out) \(rep.pixelsWide)x\(rep.pixelsHigh) px pour \(Int(W))x\(Int(H)) pt")
