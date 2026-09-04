import AVFoundation
import SwiftUI
import UIKit

/// Hosts an AVSampleBufferDisplayLayer (a peer's video) or the camera preview.
struct LayerHostView: UIViewRepresentable {
    let layer: CALayer

    func makeUIView(context: Context) -> LayerContainer {
        let view = LayerContainer()
        view.backgroundColor = .black
        view.hosted = layer
        view.layer.addSublayer(layer)
        return view
    }

    func updateUIView(_ uiView: LayerContainer, context: Context) {
        if uiView.hosted !== layer {
            uiView.hosted?.removeFromSuperlayer()
            uiView.hosted = layer
            uiView.layer.addSublayer(layer)
        }
        uiView.setNeedsLayout()
    }

    final class LayerContainer: UIView {
        var hosted: CALayer?

        override func layoutSubviews() {
            super.layoutSubviews()
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            hosted?.frame = bounds
            CATransaction.commit()
        }
    }
}

/// One participant tile: video when it flows, otherwise a name badge.
struct PeerTileView: View {
    let name: String
    let layer: CALayer?
    let muted: Bool
    let link: LinkType
    let mirrored: Bool

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            if let layer = layer {
                LayerHostView(layer: layer)
                    .scaleEffect(x: mirrored ? -1 : 1, y: 1)
            } else {
                Rectangle().fill(Color.black.opacity(0.85))
                Text(name.prefix(1).uppercased())
                    .font(.system(size: 44, weight: .semibold))
                    .foregroundStyle(.white.opacity(0.7))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack(spacing: 6) {
                Text(name).font(.caption).bold()
                if muted { Image(systemName: "mic.slash.fill").font(.caption) }
                Image(systemName: link == .direct ? "arrow.left.arrow.right" : link == .relay ? "cloud" : "ellipsis")
                    .font(.caption2)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(.ultraThinMaterial, in: Capsule())
            .padding(8)
        }
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
