import AppKit
import SwiftUI

/// Hosts a plain `NSView` whose pointer is handed to the GStreamer video sink
/// via `playerAttachSurface`. The sink (osxvideosink/glimagesink) renders into
/// this view; we re-send the handle + bounds whenever layout changes.
struct VideoSurfaceView: NSViewRepresentable {
    let onAttach: (UInt64, CGRect) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = TrackingView()
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.black.cgColor
        view.onLayout = onAttach
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        guard let view = nsView as? TrackingView else { return }
        view.onLayout = onAttach
        view.reattach()
    }

    /// Reports its native handle + bounds on every layout pass.
    final class TrackingView: NSView {
        var onLayout: ((UInt64, CGRect) -> Void)?

        override func layout() {
            super.layout()
            reattach()
        }

        func reattach() {
            let pointer = Unmanaged.passUnretained(self).toOpaque()
            let handle = UInt64(UInt(bitPattern: pointer))
            onLayout?(handle, bounds)
        }
    }
}
