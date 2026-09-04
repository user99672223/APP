import SwiftUI
import UIKit

extension SettingsView {
    var adaptationSection: some View {
        Section {
            Toggle("Lock video bitrate", isOn: bind(\.adaptation.lockVideoBitrate))
            Toggle("Lock frame rate", isOn: bind(\.adaptation.lockFps))
            Toggle("Lock resolution", isOn: bind(\.adaptation.lockResolution))
            Toggle("Lock audio bitrate", isOn: bind(\.adaptation.lockAudioBitrate))
            Toggle("A/V sync (off = minimum latency)", isOn: bind(\.adaptation.avSync))
        } header: {
            Text("Adaptation")
        } footer: {
            Text("Every quality setting is a ceiling. During congestion the engine lowers video bitrate, then frame rate, then resolution, audio last, and climbs back after 5 s of calm. A lock keeps that setting where you put it.")
        }
    }

    var filesSection: some View {
        Section("Files") {
            Toggle("Auto-accept incoming files", isOn: bind(\.files.autoAccept))
            Toggle("Speed cap", isOn: Binding(
                get: { model.settings!.files.speedCapKbps != nil },
                set: { on in
                    var s = model.settings!
                    s.files.speedCapKbps = on ? 20_000 : nil
                    model.updateSettings(s)
                }))
            if let cap = model.settings!.files.speedCapKbps {
                VStack(alignment: .leading) {
                    Text("Cap: \(cap / 1000) Mbps")
                    Slider(value: Binding(get: { Double(cap) }, set: { v in
                        var s = model.settings!
                        s.files.speedCapKbps = UInt32(v)
                        model.updateSettings(s)
                    }), in: 1000...200_000, step: 1000)
                }
            }
        }
    }

    var notificationsSection: some View {
        Section {
            LabeledContent("ntfy topic") {
                Text(model.ntfyTopic).font(.system(.footnote, design: .monospaced)).textSelection(.enabled)
            }
            Button("Subscribe in the ntfy app") {
                if let url = URL(string: "ntfy://ntfy.sh/\(model.ntfyTopic)") { UIApplication.shared.open(url) }
            }
            Button("Copy topic") { UIPasteboard.general.string = model.ntfyTopic }
            ForEach(model.users.filter { $0.account.userId != model.account?.userId }, id: \.account.userId) { user in
                Toggle("Mute \(user.account.displayName)", isOn: Binding(
                    get: { model.settings!.notifications.mutedUsers.contains(user.account.userId) },
                    set: { on in
                        var s = model.settings!
                        s.notifications.mutedUsers.removeAll { $0 == user.account.userId }
                        if on { s.notifications.mutedUsers.append(user.account.userId) }
                        model.updateSettings(s)
                    }))
            }
        } header: {
            Text("Notifications")
        } footer: {
            Text("Install the ntfy app and subscribe to this topic. Notifications say only \"Incoming call\" or \"New message\"; tapping one opens this app.")
        }
    }

    var accountSection: some View {
        Section("Account") {
            if let account = model.account {
                LabeledContent("Signed in as", value: "\(account.displayName) (\(account.handle))")
            }
            HStack {
                TextField("This device's name", text: $deviceName)
                Button("Rename") { model.renameDevice(deviceName) }
            }
            NavigationLink("Devices") { DevicesView() }
            Button("Log out", role: .destructive) { model.logout() }
        }
    }

    var aboutSection: some View {
        Section("Diagnostics") {
            Toggle("Stats overlay during calls", isOn: $model.showDiagnostics)
            if let url = model.exportLogs() {
                ShareLink("Export logs", item: url)
            }
            LabeledContent("Device id") {
                Text(model.deviceId.prefix(16) + "…").font(.system(.footnote, design: .monospaced))
            }
            LabeledContent("Engine", value: model.hello)
            LabeledContent("Build", value: "\(BuildInfo.commit) \(BuildInfo.date)")
        }
    }
}

/// List and revoke this account's devices (SPEC §3).
struct DevicesView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        List {
            ForEach(model.devices, id: \.deviceId) { device in
                VStack(alignment: .leading) {
                    HStack {
                        Text(device.deviceName)
                        if device.deviceId == model.deviceId { Text("(this device)").foregroundStyle(.secondary) }
                        Spacer()
                        Circle().fill(device.online ? .green : .gray).frame(width: 10, height: 10)
                    }
                    Text("\(device.platform) · \(device.deviceId.prefix(12))…").font(.footnote).foregroundStyle(.secondary)
                }
                .swipeActions {
                    if device.deviceId != model.deviceId {
                        Button("Revoke", role: .destructive) { model.revoke(device: device) }
                    }
                }
            }
        }
        .navigationTitle("Devices")
        .onAppear { model.loadDevices() }
        .refreshable { model.loadDevices() }
    }
}
