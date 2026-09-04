import SwiftUI

/// Chooses the screen: server setup → login/register → main app.
struct RootView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ZStack {
            if let error = model.startupError {
                ContentUnavailableView("Engine failed to start", systemImage: "exclamationmark.triangle", description: Text(error))
            } else if model.serverConfig == nil {
                ServerSetupView()
            } else if model.account == nil {
                LoginView()
            } else {
                MainTabView()
            }
            if let call = model.incomingCalls.first {
                IncomingCallView(call: call)
                    .transition(.move(edge: .top))
            }
        }
        .overlay(alignment: .bottom) {
            if let toast = model.toast {
                Text(toast)
                    .font(.footnote)
                    .padding(10)
                    .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 10))
                    .padding()
                    .transition(.opacity)
            }
        }
        .animation(.default, value: model.toast)
        .animation(.default, value: model.incomingCalls.count)
    }
}

struct ServerSetupView: View {
    @EnvironmentObject private var model: AppModel
    @State private var id = ""
    @State private var relay = ""
    @State private var direct = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Server endpoint id (64 hex characters)", text: $id)
                        .font(.system(.footnote, design: .monospaced))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Relay URL (optional)", text: $relay)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    TextField("Direct addresses ip:port (optional)", text: $direct)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Server")
                } footer: {
                    Text("The server prints its endpoint id when it starts. The id alone is enough; the relay URL and direct addresses only make the first connection faster.")
                }
                Section("This device") {
                    LabeledContent("Device id") {
                        Text(model.deviceId.prefix(16) + "…").font(.system(.footnote, design: .monospaced))
                    }
                    Text(model.hello).font(.footnote).foregroundStyle(.secondary)
                }
                Button("Connect") {
                    model.configureServer(id: id, relayUrl: relay, direct: direct)
                }
                .disabled(id.trimmingCharacters(in: .whitespaces).count != 64)
            }
            .navigationTitle("Set up")
        }
    }
}

struct LoginView: View {
    @EnvironmentObject private var model: AppModel
    @State private var registering = false
    @State private var username = ""
    @State private var password = ""
    @State private var displayName = ""
    @State private var invite = ""

    private var serverLine: String {
        switch model.serverState {
        case .disconnected: return "Not connected to the server"
        case .connecting: return "Connecting to the server…"
        case .connected: return "Connected"
        case .authenticated: return "Logged in"
        }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(serverLine).foregroundStyle(model.serverState == .connected ? .green : .secondary)
                }
                Section {
                    Picker("", selection: $registering) {
                        Text("Log in").tag(false)
                        Text("Create account").tag(true)
                    }
                    .pickerStyle(.segmented)
                    TextField("Username", text: $username)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("Password", text: $password)
                    if registering {
                        TextField("Display name", text: $displayName)
                        TextField("Invite code", text: $invite)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                } footer: {
                    if registering {
                        Text("Usernames: 3–32 lower-case letters, digits or underscore. Passwords: at least 8 characters. After this, the device key is the credential; the password is never sent again.")
                    }
                }
                Button(registering ? "Create account" : "Log in") {
                    if registering {
                        model.register(username: username, password: password, displayName: displayName, inviteCode: invite)
                    } else {
                        model.login(username: username, password: password)
                    }
                }
                .disabled(model.serverState != .connected || username.isEmpty || password.isEmpty)
                Section {
                    Button("Change server", role: .destructive) {
                        if let engine = model.engine { try? engine.setServer(server: nil) }
                        model.serverConfig = nil
                    }
                }
            }
            .navigationTitle("APP")
        }
    }
}

struct MainTabView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        TabView {
            DirectoryView()
                .tabItem { Label("People", systemImage: "person.2") }
            ChatsListView()
                .tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
                .badge(model.unread.values.reduce(0, +))
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
        }
        .fullScreenCover(isPresented: Binding(get: { model.inRoom }, set: { _ in })) {
            CallView()
        }
    }
}
