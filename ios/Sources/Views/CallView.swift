import SwiftUI

/// The room: everyone's video, our mirrored self-view, controls, chat and file
/// sheets, and the diagnostics overlay (SPEC §10, §15).
struct CallView: View {
    @EnvironmentObject private var model: AppModel
    @State private var showChat = false
    @State private var showFiles = false
    @State private var showPeople = false

    private var peerList: [PeerState] {
        model.peers.values.sorted { $0.deviceId < $1.deviceId }
    }

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: peerList.count > 1 ? 160 : 320), spacing: 8)]
    }

    var body: some View {
        ZStack(alignment: .bottom) {
            Color.black.ignoresSafeArea()
            ScrollView {
                LazyVGrid(columns: columns, spacing: 8) {
                    ForEach(peerList) { peer in
                        PeerTileView(name: model.nameOfDevice(peer.deviceId),
                                     layer: peer.hasVideo ? model.media?.renderer(for: peer.deviceId).layer : nil,
                                     muted: peer.audioMuted, link: peer.link, mirrored: false)
                            .aspectRatio(peerList.count > 1 ? 3 / 4 : 9 / 16, contentMode: .fit)
                    }
                    if peerList.isEmpty {
                        VStack(spacing: 8) {
                            ProgressView().tint(.white)
                            Text(model.outgoingCall != nil ? "Calling…" : "Waiting for others").foregroundStyle(.white)
                            if let code = model.room?.code {
                                Text("Room code \(code)").font(.title2.monospaced()).foregroundStyle(.white)
                                Text("Share it, or invite someone from People.").font(.footnote).foregroundStyle(.gray)
                            }
                        }
                        .frame(maxWidth: .infinity, minHeight: 240)
                    }
                }
                .padding(8)
                .padding(.bottom, 120)
            }
            if model.showDiagnostics, let stats = model.stats {
                DiagnosticsOverlay(stats: stats)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            if let media = model.media, media.cameraOn {
                LayerHostView(layer: media.previewLayer)
                    .frame(width: 110, height: 160)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .padding()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
            }
            controls
        }
        .sheet(isPresented: $showChat) {
            if let roomId = model.room?.roomId {
                ChatView(key: .room(roomId: roomId))
            }
        }
        .sheet(isPresented: $showFiles) { FilesView() }
        .sheet(isPresented: $showPeople) { RoomPeopleView() }
        .statusBarHidden(true)
    }

    private var controls: some View {
        HStack(spacing: 18) {
            ControlButton(system: model.audioMuted ? "mic.slash.fill" : "mic.fill", active: model.audioMuted) { model.toggleMute() }
            ControlButton(system: (model.media?.cameraOn ?? false) ? "video.fill" : "video.slash.fill", active: model.media?.cameraOn ?? false) { model.toggleCamera() }
            ControlButton(system: "arrow.triangle.2.circlepath.camera", active: false) { model.switchCamera() }
            ControlButton(system: "bubble.left.fill", active: showChat) { showChat = true }
            ControlButton(system: "doc.fill", active: showFiles) { showFiles = true }
            ControlButton(system: "person.2.fill", active: showPeople) { showPeople = true }
            ControlButton(system: "waveform.path.ecg", active: model.showDiagnostics) { model.showDiagnostics.toggle() }
            Button { model.hangUp() } label: {
                Image(systemName: "phone.down.fill")
                    .font(.title2)
                    .frame(width: 56, height: 56)
                    .background(Color.red, in: Circle())
                    .foregroundStyle(.white)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial, in: Capsule())
        .padding(.bottom, 24)
    }
}

struct ControlButton: View {
    let system: String
    let active: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: system)
                .font(.title3)
                .frame(width: 44, height: 44)
                .background(active ? Color.accentColor.opacity(0.5) : Color.white.opacity(0.15), in: Circle())
                .foregroundStyle(.white)
        }
    }
}

/// Members of the room with per-peer volume (SPEC §9) and invite.
struct RoomPeopleView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("In the room") {
                    ForEach(model.peers.values.sorted { $0.deviceId < $1.deviceId }) { peer in
                        VStack(alignment: .leading) {
                            HStack {
                                Text(model.nameOfDevice(peer.deviceId))
                                Spacer()
                                Text(peer.link == .direct ? "direct" : peer.link == .relay ? "relayed" : "…")
                                    .font(.footnote).foregroundStyle(.secondary)
                            }
                            Slider(value: Binding(
                                get: { Double(model.settings?.audio.peerVolumes[peer.deviceId] ?? 1.0) },
                                set: { model.setPeerVolume(deviceId: peer.deviceId, volume: Float($0)) }
                            ), in: 0...2) { Text("Volume") }
                        }
                    }
                }
                Section("Invite") {
                    ForEach(model.users.filter { u in u.account.userId != model.account?.userId && !model.peers.values.contains { $0.userId == u.account.userId } }, id: \.account.userId) { user in
                        Button { model.invite(user: user) } label: {
                            Label(user.account.displayName, systemImage: "person.badge.plus")
                        }
                    }
                }
            }
            .navigationTitle(model.room.map { "Room \($0.code)" } ?? "Room")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}
