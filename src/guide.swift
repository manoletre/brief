// brief's install guide: a small panel that floats over the browser's extensions page while you
// load brief's extension, with the steps, the folder to copy or drag, and a live "connected"
// check. It closes itself once the extension starts sending to brief. Built by macapp.rs.
//
//   BriefGuide --browser Dia --bundle company.thebrowser.dia --page chrome://extensions
//              --folder ~/.brief/chrome-extension --watch ~/.brief/chrome.json [--icon icon.png]

import AppKit
import SwiftUI

var opts: [String: String] = [:]
var it = CommandLine.arguments.dropFirst().makeIterator()
while let a = it.next() {
    if a.hasPrefix("--") { opts[String(a.dropFirst(2))] = it.next() ?? "" }
}
let browser = opts["browser"] ?? "your browser"
let bundle = opts["bundle"] ?? ""
let page = opts["page"] ?? "chrome://extensions"
let folder = opts["folder"] ?? ""
let watch = opts["watch"] ?? ""
let started = Date()

final class Model: ObservableObject {
    @Published var connected = false
    @Published var copied = false
}
let model = Model()

func copyFolder() {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(folder, forType: .string)
    model.copied = true
    DispatchQueue.main.asyncAfter(deadline: .now() + 2) { model.copied = false }
}

func openPage() {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    p.arguments = bundle.isEmpty ? [page] : ["-b", bundle, page]
    try? p.run()
}

struct Step: View {
    let n: Int
    let text: String
    var detail: String? = nil
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("\(n)")
                .font(.system(size: 11, weight: .bold))
                .frame(width: 20, height: 20)
                .background(Circle().fill(Color.orange.opacity(0.9)))
                .foregroundColor(.white)
            VStack(alignment: .leading, spacing: 2) {
                Text(.init(text))
                if let d = detail { Text(d).font(.caption).foregroundColor(.secondary) }
            }
        }
    }
}

struct Guide: View {
    @ObservedObject var m: Model
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                if let path = opts["icon"], let img = NSImage(contentsOfFile: path) {
                    Image(nsImage: img).resizable().frame(width: 20, height: 20)
                }
                Text(m.connected ? "brief is connected" : "Add brief to \(browser)").font(.headline)
                Spacer()
                Button(action: { NSApp.terminate(nil) }) { Image(systemName: "xmark.circle.fill").foregroundColor(.secondary) }
                    .buttonStyle(.plain)
                    .help("Close")
            }
            if m.connected {
                HStack(spacing: 10) {
                    Image(systemName: "checkmark.circle.fill").font(.system(size: 28)).foregroundColor(.green)
                    Text("\(browser) is sending to brief. Tomorrow's brief will know what you read. Updates install themselves from now on.")
                        .fixedSize(horizontal: false, vertical: true)
                }
            } else {
                Text("On the extensions page behind this box:").foregroundColor(.secondary)
                Step(n: 1, text: "Turn on **Developer mode**", detail: "the switch at the top right of the page")
                Step(n: 2, text: "Click **Load unpacked**")
                Step(n: 3, text: "Press **⌘⇧G**, paste, press **⏎**, then click **Select**", detail: "the folder path is already copied")
                HStack(spacing: 8) {
                    Image(nsImage: NSWorkspace.shared.icon(forFile: folder))
                        .resizable()
                        .frame(width: 28, height: 28)
                        .onDrag { NSItemProvider(object: URL(fileURLWithPath: folder) as NSURL) }
                        .help("Or drag this folder onto the extensions page")
                    Text((folder as NSString).abbreviatingWithTildeInPath)
                        .font(.system(.caption, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button(m.copied ? "Copied" : "Copy path", action: copyFolder).controlSize(.small)
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.06)))
                Divider()
                HStack {
                    ProgressView().controlSize(.small)
                    Text("Waiting for \(browser)…").foregroundColor(.secondary)
                    Spacer()
                    Button("Open the page again", action: openPage).buttonStyle(.link)
                }
                .font(.callout)
            }
        }
        .padding(16)
        .frame(width: 350)
    }
}

/// When the watched file (chrome.json, rewritten each time the extension sends) changed after we started.
func extensionConnected() -> Bool {
    guard !watch.isEmpty,
          let attrs = try? FileManager.default.attributesOfItem(atPath: watch),
          let modified = attrs[.modificationDate] as? Date else { return false }
    return modified > started
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let host = NSHostingView(rootView: Guide(m: model))
let panel = NSPanel(
    contentRect: NSRect(origin: .zero, size: host.fittingSize),
    styleMask: [.titled, .fullSizeContentView, .nonactivatingPanel, .utilityWindow],
    backing: .buffered,
    defer: false
)
panel.titlebarAppearsTransparent = true
panel.titleVisibility = .hidden
panel.standardWindowButton(.closeButton)?.isHidden = true
panel.standardWindowButton(.miniaturizeButton)?.isHidden = true
panel.standardWindowButton(.zoomButton)?.isHidden = true
panel.isMovableByWindowBackground = true
panel.level = .floating
panel.hidesOnDeactivate = false
panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
let effect = NSVisualEffectView()
effect.material = .popover
effect.state = .active
effect.blendingMode = .behindWindow
host.translatesAutoresizingMaskIntoConstraints = false
effect.addSubview(host)
NSLayoutConstraint.activate([
    host.leadingAnchor.constraint(equalTo: effect.leadingAnchor),
    host.trailingAnchor.constraint(equalTo: effect.trailingAnchor),
    host.topAnchor.constraint(equalTo: effect.topAnchor),
    host.bottomAnchor.constraint(equalTo: effect.bottomAnchor),
])
panel.contentView = effect

func place() {
    let size = host.fittingSize
    let screen = (NSScreen.main ?? NSScreen.screens[0]).visibleFrame
    panel.setFrame(NSRect(x: screen.maxX - size.width - 20, y: screen.maxY - size.height - 70, width: size.width, height: size.height), display: true, animate: false)
}
place()
panel.alphaValue = 0
panel.orderFrontRegardless()
NSAnimationContext.runAnimationGroup { ctx in
    ctx.duration = 0.25
    panel.animator().alphaValue = 1
}

Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { timer in
    if extensionConnected() {
        timer.invalidate()
        withAnimation { model.connected = true }
        DispatchQueue.main.async {
            let old = panel.frame, size = host.fittingSize
            panel.setFrame(NSRect(x: old.maxX - size.width, y: old.maxY - size.height, width: size.width, height: size.height), display: true, animate: true)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 6) { NSApp.terminate(nil) }
    } else if Date().timeIntervalSince(started) > 30 * 60 {
        NSApp.terminate(nil)
    }
}

app.run()
