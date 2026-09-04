import Foundation
import SwiftUI
import UIKit

/// A conversation key the UI can hash (mirrors the engine's ChatScope).
enum ChatKey: Hashable {
    case dm(userId: UInt64)
    case room(roomId: UInt64)

    var scope: ChatScope {
        switch self {
        case .dm(let userId): return .dm(userId: userId)
        case .room(let roomId): return .room(roomId: roomId)
        }
    }

    init(_ scope: ChatScope) {
        switch scope {
        case .dm(let userId): self = .dm(userId: userId)
        case .room(let roomId): self = .room(roomId: roomId)
        }
    }
}

/// What we know about a device in the current room.
struct PeerState: Identifiable {
    var deviceId: String
    var userId: UInt64
    var link: LinkType = .connecting
    var audioMuted = false
    var videoOn = false
    var hasVideo = false
    var id: String { deviceId }
}

/// Bridge between SwiftUI and the Rust engine. Engine events arrive on engine
/// threads and are hopped onto the main actor here.
@MainActor
final class AppModel: ObservableObject {
    @Published var startupError: String?
    @Published var hello = ""
    @Published var deviceId = ""
    @Published var ntfyTopic = ""
    @Published var serverState: ServerState = .disconnected
    @Published var serverConfig: ServerConfig?
    @Published var account: AccountInfo?
    @Published var users: [UserInfo] = []
    @Published var room: RoomInfo?
    @Published var peers: [String: PeerState] = [:]
    @Published var incomingCalls: [CallInfo] = []
    @Published var outgoingCall: CallInfo?
    @Published var messages: [ChatKey: [HistoryEntry]] = [:]
    @Published var unread: [ChatKey: Int] = [:]
    @Published var transfers: [FileTransferInfo] = []
    @Published var devices: [DeviceInfo] = []
    @Published var settings: Settings?
    @Published var stats: EngineStats?
    @Published var toast: String?
    @Published var activeChat: ChatKey?
    @Published var showDiagnostics = false
    @Published var audioMuted = false
    @Published var roomInvite: RoomInfo?
    var roomInviteFrom: UInt64 = 0

    private(set) var engine: Engine?
    private(set) var media: MediaController?
    private var statsTimer: Timer?
    private var toastTask: Task<Void, Never>?

    init() {
        start()
    }

    private func start() {
        do {
            let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("engine", isDirectory: true)
            try FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
            let config = EngineConfig(
                dataDir: dataDir.path,
                storageKey: StorageKey.loadOrCreate(),
                deviceName: UIDevice.current.name,
                platform: .ios,
                appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0",
                logToStderr: true,
                decodeCaps: [.h264, .hevc]
            )
            let relay = EventRelay()
            let engine = try Engine(config: config, listener: relay)
            relay.model = self
            self.engine = engine
            let media = MediaController(engine: engine)
            relay.media = media
            self.media = media
            hello = engine.hello()
            deviceId = engine.deviceId()
            ntfyTopic = (try? engine.ntfyTopic()) ?? ""
            serverConfig = engine.serverConfig()
            settings = engine.settings()
            serverState = engine.serverState()
            account = engine.account()
            users = engine.directory().sorted { $0.account.displayName < $1.account.displayName }
            statsTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
                Task { @MainActor in self?.refreshStats() }
            }
        } catch {
            startupError = "\(error)"
        }
    }

    // MARK: helpers

    func show(_ text: String) {
        toast = text
        toastTask?.cancel()
        toastTask = Task {
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            if !Task.isCancelled { self.toast = nil }
        }
    }

    /// Runs an engine call off the main actor and reports failures as a toast.
    func run(_ label: String, _ work: @escaping () async throws -> Void) {
        Task {
            do {
                try await work()
            } catch {
                self.show("\(label): \(error)")
            }
        }
    }

    func user(_ id: UInt64) -> UserInfo? {
        users.first { $0.account.userId == id }
    }

    func displayName(_ id: UInt64) -> String {
        if let me = account, me.userId == id { return "You" }
        return user(id)?.account.displayName ?? "user \(id)"
    }

    func nameOfDevice(_ deviceId: String) -> String {
        if let p = peers[deviceId] { return displayName(p.userId) }
        return String(deviceId.prefix(8))
    }

    var inRoom: Bool { room != nil }

    private func refreshStats() {
        guard let engine = engine, inRoom || showDiagnostics else { return }
        stats = engine.stats()
    }

    // MARK: server and account

    func configureServer(id: String, relayUrl: String, direct: String) {
        guard let engine = engine else { return }
        let directs = direct.split(whereSeparator: { $0 == "," || $0 == " " }).map(String.init).filter { !$0.isEmpty }
        let cfg = ServerConfig(id: id.trimmingCharacters(in: .whitespacesAndNewlines),
                               relayUrl: relayUrl.isEmpty ? nil : relayUrl, direct: directs)
        do {
            try engine.setServer(server: cfg)
            serverConfig = cfg
        } catch {
            show("server: \(error)")
        }
    }

    func register(username: String, password: String, displayName: String, inviteCode: String) {
        guard let engine = engine else { return }
        run("register") {
            let acct = try await engine.register(username: username, password: password, displayName: displayName, inviteCode: inviteCode)
            await MainActor.run { self.account = acct }
        }
    }

    func login(username: String, password: String) {
        guard let engine = engine else { return }
        run("login") {
            let acct = try await engine.login(username: username, password: password)
            await MainActor.run { self.account = acct }
        }
    }

    func logout() {
        guard let engine = engine else { return }
        run("logout") { try await engine.logout() }
    }

    func refreshDirectory() {
        guard let engine = engine else { return }
        run("directory") { _ = try await engine.refreshDirectory() }
    }

    func loadDevices() {
        guard let engine = engine else { return }
        run("devices") {
            let list = try await engine.devices()
            await MainActor.run { self.devices = list }
        }
    }

    func revoke(device: DeviceInfo) {
        guard let engine = engine else { return }
        run("revoke") {
            try await engine.revokeDevice(deviceId: device.deviceId)
            let list = try await engine.devices()
            await MainActor.run { self.devices = list }
        }
    }

    func renameDevice(_ name: String) {
        guard let engine = engine else { return }
        run("rename") { try await engine.renameDevice(name: name) }
    }
}

// MARK: rooms, calls and media controls

extension AppModel {
    func createRoom() {
        guard let engine = engine else { return }
        run("create room") { _ = try await engine.createRoom() }
    }

    func joinRoom(code: String) {
        guard let engine = engine, !code.isEmpty else { return }
        run("join room") { _ = try await engine.joinRoom(code: code) }
    }

    func joinRoom(id: UInt64) {
        guard let engine = engine else { return }
        run("join room") { _ = try await engine.joinRoomById(roomId: id) }
    }

    func invite(user: UserInfo) {
        guard let engine = engine else { return }
        run("invite") {
            try await engine.inviteToRoom(userId: user.account.userId)
            await MainActor.run { self.show("Invited \(user.account.displayName)") }
        }
    }

    func call(user: UserInfo) {
        guard let engine = engine else { return }
        run("call") {
            let call = try await engine.call(userId: user.account.userId)
            await MainActor.run { self.outgoingCall = call }
        }
    }

    func answer(call: CallInfo) {
        guard let engine = engine else { return }
        incomingCalls.removeAll { $0.callId == call.callId }
        run("answer") { _ = try await engine.answerCall(callId: call.callId) }
    }

    func decline(call: CallInfo) {
        guard let engine = engine else { return }
        incomingCalls.removeAll { $0.callId == call.callId }
        run("decline") { try await engine.declineCall(callId: call.callId) }
    }

    func hangUp() {
        guard let engine = engine else { return }
        outgoingCall = nil
        run("hang up") { try await engine.hangUp() }
    }

    func toggleMute() {
        guard let engine = engine else { return }
        audioMuted.toggle()
        engine.setAudioMuted(muted: audioMuted)
    }

    func toggleCamera() {
        guard let media = media, let settings = settings else { return }
        if media.cameraOn {
            media.stopCamera()
        } else {
            media.startCamera(facing: settings.video.camera, mirror: settings.video.mirrorSelfView)
        }
    }

    func switchCamera() {
        guard var s = settings else { return }
        s.video.camera = s.video.camera == .front ? .back : .front
        updateSettings(s)
        media?.switchCamera(to: s.video.camera, mirror: s.video.mirrorSelfView)
    }

    fileprivate func enteredRoom(_ room: RoomInfo) {
        self.room = room
        peers = Dictionary(uniqueKeysWithValues: room.members.map { ($0.deviceId, PeerState(deviceId: $0.deviceId, userId: $0.userId)) })
        audioMuted = false
        engine?.setAudioMuted(muted: false)
        media?.startAudio(voiceProcessing: settings?.audio.voiceProcessing ?? true)
        loadHistory(.room(roomId: room.roomId))
    }

    fileprivate func leftRoom() {
        room = nil
        peers = [:]
        outgoingCall = nil
        showDiagnostics = false
        media?.stopAll()
    }

    // MARK: chat

    func loadHistory(_ key: ChatKey) {
        guard let engine = engine else { return }
        messages[key] = (try? engine.history(scope: key.scope, limit: 200)) ?? []
        unread[key] = 0
    }

    func send(text: String, to key: ChatKey) {
        guard let engine = engine else { return }
        run("send") {
            let entry = try await engine.sendMessage(scope: key.scope, text: text)
            await MainActor.run {
                if !(self.messages[key] ?? []).contains(where: { $0.msgId == entry.msgId }) {
                    self.messages[key, default: []].append(entry)
                }
            }
        }
    }

    func clearHistory(_ key: ChatKey) {
        guard let engine = engine else { return }
        try? engine.clearHistory(scope: key.scope)
        messages[key] = []
    }

    // MARK: files

    func sendFile(url: URL, to deviceIds: [String]) {
        guard let engine = engine else { return }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let staging = FileManager.default.temporaryDirectory.appendingPathComponent("outgoing", isDirectory: true)
        let copy = staging.appendingPathComponent(url.lastPathComponent)
        do {
            try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
            if FileManager.default.fileExists(atPath: copy.path) { try FileManager.default.removeItem(at: copy) }
            try FileManager.default.copyItem(at: url, to: copy)
        } catch {
            show("file: \(error.localizedDescription)")
            return
        }
        run("send file") { _ = try await engine.sendFile(path: copy.path, peers: deviceIds) }
    }

    var receivedDir: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Received", isDirectory: true)
    }

    func accept(transfer: FileTransferInfo) {
        guard let engine = engine else { return }
        let dir = receivedDir
        run("accept file") { _ = try await engine.acceptFile(fileId: transfer.fileId, destDir: dir.path) }
    }

    func reject(transfer: FileTransferInfo) {
        guard let engine = engine else { return }
        run("reject file") { try await engine.rejectFile(fileId: transfer.fileId) }
    }

    func cancel(transfer: FileTransferInfo) {
        guard let engine = engine else { return }
        run("cancel file") { try await engine.cancelFile(fileId: transfer.fileId) }
    }

    // MARK: settings, logs, lifecycle

    func updateSettings(_ s: Settings) {
        guard let engine = engine else { return }
        do {
            try engine.updateSettings(settings: s)
            settings = s
        } catch {
            show("settings: \(error)")
        }
    }

    func setPeerVolume(deviceId: String, volume: Float) {
        guard let engine = engine else { return }
        try? engine.setPeerVolume(deviceId: deviceId, volume: volume)
        settings = engine.settings()
    }

    func exportLogs() -> URL? {
        guard let engine = engine, let path = try? engine.exportLogs() else { return nil }
        return URL(fileURLWithPath: path)
    }

    func handleDeepLink(_ url: URL) {
        guard let engine = engine else { return }
        Task {
            let outcome = await engine.handleDeepLink(url: url.absoluteString)
            await MainActor.run { self.apply(outcome) }
        }
    }

    private func apply(_ outcome: DeepLinkOutcome) {
        switch outcome {
        case .call:
            break // the engine emits IncomingCall when it is still ringing
        case .callOver(_, let reason):
            show("That call is over (\(reason))")
        case .dm(let userId, _):
            activeChat = .dm(userId: userId)
        case .room(let room):
            joinRoom(id: room.roomId)
        case .roomGone:
            show("That room no longer exists")
        case .invalid(let reason):
            show("Bad link: \(reason)")
        }
    }

    /// On every launch and return to foreground: full inbox sync (SPEC §7).
    func didBecomeActive() {
        guard let engine = engine else { return }
        if account != nil {
            run("sync") { _ = try await engine.syncInbox() }
            refreshDirectory()
        }
        if let media = media, media.cameraOn, let s = settings {
            media.switchCamera(to: s.video.camera, mirror: s.video.mirrorSelfView)
        }
    }
}

// MARK: engine events

extension AppModel {
    nonisolated func receive(_ event: EngineEvent) {
        Task { @MainActor in self.handle(event) }
    }

    private func handle(_ event: EngineEvent) {
        switch event {
        case .server(let state):
            serverState = state
        case .authenticated(let account, _):
            self.account = account
        case .loggedOut:
            account = nil
            leftRoom()
        case .revoked:
            account = nil
            leftRoom()
            show("This device was signed out remotely")
        case .directory(let list):
            users = list.sorted { $0.account.displayName < $1.account.displayName }
        case .presence(let userId, let online, let lastSeenMs):
            if let i = users.firstIndex(where: { $0.account.userId == userId }) {
                users[i].online = online
                users[i].lastSeenMs = lastSeenMs
            }
        case .userUpdated(let user):
            if let i = users.firstIndex(where: { $0.account.userId == user.account.userId }) {
                users[i] = user
            } else {
                users.append(user)
                users.sort { $0.account.displayName < $1.account.displayName }
            }
        case .devices(let list):
            devices = list
        case .roomJoined(let room):
            enteredRoom(room)
        case .roomLeft:
            leftRoom()
        case .peerJoined(_, let deviceId, let userId):
            peers[deviceId] = PeerState(deviceId: deviceId, userId: userId)
        case .peerLeft(_, let deviceId):
            peers.removeValue(forKey: deviceId)
            media?.peerLeft(deviceId)
        case .peerLink(let deviceId, let link):
            peers[deviceId]?.link = link
        case .roomInvite(let room, let fromUser):
            roomInviteFrom = fromUser
            roomInvite = room
        case .incomingCall(let call):
            if !incomingCalls.contains(where: { $0.callId == call.callId }) {
                incomingCalls.append(call)
            }
        case .callUpdate(let call):
            if call.state != .ringing {
                incomingCalls.removeAll { $0.callId == call.callId }
            }
            if outgoingCall?.callId == call.callId {
                outgoingCall = call.state == .ringing ? call : nil
                if call.state == .declined || call.state == .missed {
                    show(call.state == .declined ? "Call declined" : "No answer")
                    hangUp()
                }
            }
        case .message(let entry):
            let key = ChatKey(entry.scope)
            if !(messages[key] ?? []).contains(where: { $0.msgId == entry.msgId }) {
                messages[key, default: []].append(entry)
            }
            if !entry.outgoing && activeChat != key {
                unread[key, default: 0] += 1
            }
        case .messageDelivered(let msgId):
            for key in messages.keys {
                if let i = messages[key]?.firstIndex(where: { $0.msgId == msgId }) {
                    messages[key]?[i].delivered = true
                }
            }
        case .fileUpdate(let transfer):
            if let i = transfers.firstIndex(where: { $0.fileId == transfer.fileId }) {
                transfers[i] = transfer
            } else {
                transfers.append(transfer)
            }
        case .peerMedia(let deviceId, let audioMuted, let videoOn):
            peers[deviceId]?.audioMuted = audioMuted
            peers[deviceId]?.videoOn = videoOn
            if !videoOn { peers[deviceId]?.hasVideo = false }
        case .screenShare(let deviceId, let active, _):
            show("\(nameOfDevice(deviceId)) \(active ? "started" : "stopped") sharing their screen")
        case .videoFormat(let deviceId, let family, _, _, _, _):
            if family == .camera {
                peers[deviceId]?.hasVideo = true
                media?.renderer(for: deviceId).reset()
            }
        case .keyframeRequested:
            media?.produceKeyframe()
        case .encoderConfig(let family, let codec, let width, let height, let fps, let bitrateKbps):
            if family == .camera, let media = media, media.cameraOn, let s = settings {
                let cfg = EncoderConfig(family: family, codec: codec, width: width, height: height, fps: fps, bitrateKbps: bitrateKbps)
                media.applyEncoderConfig(cfg, facing: s.video.camera, mirror: s.video.mirrorSelfView)
            }
        case .loopback:
            break
        case .error(let context, let message):
            show("\(context): \(message)")
        }
    }
}

/// Engine → app. Plain class so engine threads can call it without actor hops;
/// frames go straight to the media controller, events to the model.
final class EventRelay: EngineListener, @unchecked Sendable {
    weak var model: AppModel?
    var media: MediaController?

    func onEvent(event: EngineEvent) {
        model?.receive(event)
    }

    func onVideoFrame(from: String, frame: EncodedFrame) {
        media?.receive(from: from, frame: frame)
    }
}
