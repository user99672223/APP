import SwiftUI
import UniformTypeIdentifiers

/// Transfers in and out (SPEC §12). Keep the app in the foreground while a
/// transfer runs.
struct FilesView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var picking = false
    @State private var chosen: Set<String> = []

    private var peers: [PeerState] { model.peers.values.sorted { $0.deviceId < $1.deviceId } }

    var body: some View {
        NavigationStack {
            List {
                Section("Send to") {
                    if peers.isEmpty { Text("Nobody connected yet").foregroundStyle(.secondary) }
                    ForEach(peers) { peer in
                        Toggle(model.nameOfDevice(peer.deviceId), isOn: Binding(
                            get: { chosen.contains(peer.deviceId) },
                            set: { on in if on { chosen.insert(peer.deviceId) } else { chosen.remove(peer.deviceId) } }))
                    }
                    Button { picking = true } label: { Label("Choose a file…", systemImage: "doc.badge.plus") }
                        .disabled(chosen.isEmpty)
                }
                Section("Transfers") {
                    if model.transfers.isEmpty { Text("None yet").foregroundStyle(.secondary) }
                    ForEach(model.transfers.reversed(), id: \.fileId) { t in
                        TransferRow(transfer: t)
                    }
                }
            }
            .navigationTitle("Files")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .onAppear { chosen = Set(peers.map(\.deviceId)) }
            .fileImporter(isPresented: $picking, allowedContentTypes: [.item], allowsMultipleSelection: false) { result in
                if case .success(let urls) = result, let url = urls.first {
                    model.sendFile(url: url, to: Array(chosen))
                }
            }
        }
    }
}

struct TransferRow: View {
    @EnvironmentObject private var model: AppModel
    let transfer: FileTransferInfo

    private var stateText: String {
        switch transfer.state {
        case .offered: return transfer.outgoing ? "waiting for them to accept" : "offered to you"
        case .transferring: return "transferring"
        case .paused: return "paused, resumes when they are back"
        case .done: return "done"
        case .failed(let reason): return "failed: \(reason)"
        case .rejected: return "rejected"
        case .cancelled: return "cancelled"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: transfer.outgoing ? "arrow.up.doc" : "arrow.down.doc")
                Text(transfer.name).lineLimit(1)
                Spacer()
                Text(ByteCountFormatter.string(fromByteCount: Int64(transfer.size), countStyle: .file))
                    .font(.footnote).foregroundStyle(.secondary)
            }
            Text("\(transfer.outgoing ? "to" : "from") \(model.displayName(transfer.userId)) · \(stateText)")
                .font(.footnote).foregroundStyle(.secondary)
            if transfer.state == .transferring || transfer.state == .paused {
                ProgressView(value: Double(transfer.doneBytes), total: Double(max(transfer.size, 1)))
            }
            HStack {
                if !transfer.outgoing && transfer.state == .offered {
                    Button("Accept") { model.accept(transfer: transfer) }.buttonStyle(.borderedProminent)
                    Button("Reject", role: .destructive) { model.reject(transfer: transfer) }.buttonStyle(.bordered)
                }
                if transfer.state == .transferring || transfer.state == .paused || (transfer.outgoing && transfer.state == .offered) {
                    Button("Cancel", role: .destructive) { model.cancel(transfer: transfer) }.buttonStyle(.bordered)
                }
                if transfer.state == .done, let path = transfer.path {
                    ShareLink(item: URL(fileURLWithPath: path)) { Label("Open", systemImage: "square.and.arrow.up") }
                        .buttonStyle(.bordered)
                }
            }
            .font(.footnote)
        }
        .padding(.vertical, 4)
    }
}

/// Per-peer numbers from SPEC §15, drawn over the call.
struct DiagnosticsOverlay: View {
    let stats: EngineStats

    private func row(_ p: PeerStats) -> String {
        let link = p.link == .direct ? "direct" : p.link == .relay ? "relay" : "…"
        return """
        \(p.deviceId.prefix(8)) \(link) rtt \(Int(p.rttMs)) ms loss \(p.lossPermille)‰
          audio in \(Int(p.audioInKbps)) out \(Int(p.audioOutKbps)) kbps jitter \(Int(p.jitterDepthMs))/\(Int(p.jitterTargetMs)) ms concealed \(p.audioConcealed)
          video in \(Int(p.videoInFps)) fps \(Int(p.videoInKbps)) kbps out \(Int(p.videoOutFps)) fps \(Int(p.videoOutKbps)) kbps
          enc \(String(format: "%.1f", p.encodeMs)) dec \(String(format: "%.1f", p.decodeMs)) ms delay \(String(format: "%.1f", p.frameDelayMs)) drift \(String(format: "%.1f", p.clockDriftMs)) ms
          dropped \(p.droppedFrames) resets \(p.streamResets) target \(p.targetVideoKbps) kbps \(p.targetFps) fps \(p.targetHeight)p audio \(p.targetAudioKbps) kbps
        """
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("server rtt \(Int(stats.serverRttMs)) ms · adapt level \(stats.adaptLevel) · mic \(Int(stats.micLevel * 100))%")
            ForEach(stats.peers, id: \.deviceId) { p in
                Text(row(p))
            }
        }
        .font(.system(size: 10, design: .monospaced))
        .foregroundStyle(.white)
        .padding(8)
        .background(Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 8))
        .padding(8)
        .allowsHitTesting(false)
    }
}
