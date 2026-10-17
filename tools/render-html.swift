// Render an HTML file to a PNG with WebKit — the macOS stand-in for
// `chromium --headless --screenshot` (docs/mockups/README.md) where no Chromium
// is installed. Used for the README's contact sheet, docs/img/readme.html.
//
//     swiftc -O tools/render-html.swift -o /tmp/render-html   # once; `swift` JIT is ~30 s a run
//     /tmp/render-html page.html out.png [width]
//
// `width` is CSS pixels; the PNG is at the screen's scale (2x on a Retina Mac).
// The height is the page's own scroll height, so the image is the whole page.
// Read access is the whole filesystem so a page can reach `assets/fonts/`.
import AppKit
import WebKit

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: render-html page.html out.png [width]\n".data(using: .utf8)!)
    exit(2)
}
let page = URL(fileURLWithPath: args[1]).standardizedFileURL
let out = URL(fileURLWithPath: args[2])
let width = args.count > 3 ? Double(args[3])! : 1600

final class Renderer: NSObject, WKNavigationDelegate {
    let view: WKWebView
    init(width: Double) {
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: 800))
        super.init()
        view.navigationDelegate = self
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Give images and fonts a moment to decode before measuring.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
            webView.evaluateJavaScript("document.documentElement.scrollHeight") { h, _ in
                let height = (h as? Double) ?? 800
                webView.frame = NSRect(x: 0, y: 0, width: width, height: height)
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                    let config = WKSnapshotConfiguration()
                    config.rect = NSRect(x: 0, y: 0, width: width, height: height)
                    config.snapshotWidth = NSNumber(value: width)
                    webView.takeSnapshot(with: config) { image, error in
                        guard let image, let tiff = image.tiffRepresentation,
                              let rep = NSBitmapImageRep(data: tiff),
                              let png = rep.representation(using: .png, properties: [:])
                        else {
                            FileHandle.standardError.write("snapshot failed: \(String(describing: error))\n".data(using: .utf8)!)
                            exit(1)
                        }
                        try! png.write(to: out)
                        print("\(out.path) \(Int(width))x\(Int(height))")
                        exit(0)
                    }
                }
            }
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let renderer = Renderer(width: width)
renderer.view.loadFileURL(page, allowingReadAccessTo: URL(fileURLWithPath: "/"))
app.run()
