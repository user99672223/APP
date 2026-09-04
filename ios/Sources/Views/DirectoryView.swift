import SwiftUI

/// Everyone registered on the server, with presence (SPEC §3): call, message, or
/// pull into the current room. Join a room by code from here too.
struct DirectoryView: View {
    @EnvironmentObject private var model: AppModel
    @State private var code = ""
    @State private var showJoin = false

    private var others: [UserInfo] {
        model.users.filter { $0.account.userId != model.account?.userId }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack {
                        Circle().fill(model.serverState == .authenticated ? .green : .orange).frame(width: 10, height: 10)
                        Text(model.serverState == .authenticated ? "Online as \(model.account?.displayName ?? "")" : "Reconnecting…")
                            .font(.footnote)
                        Spacer()
                        Text(model.account?.handle ?? "").font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("People") {
                    if others.isEmpty {
                        Text("Nobody else has registered yet.").foregroundStyle(.secondary)
                    }
                    ForEach(others, id: \.account.userId) { user in
                        UserRow(user: user)
                    }
                }
                if let call = model.outgoingCall, call.state == .ringing {
                    Section("Calling") {
                        HStack {
                            ProgressView()
                            Text("Calling \(model.displayName(call.toUser))…")
                            Spacer()
                            Button("Cancel", role: .destructive) { model.hangUp() }
                        }
                    }
                }
            }
            .refreshable { model.refreshDirectory() }
            .navigationTitle("People")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button { model.createRoom() } label: { Label("New room", systemImage: "plus.circle") }
                        Button { showJoin = true } label: { Label("Join with code", systemImage: "number") }
                    } label: {
                        Image(systemName: "video.badge.plus")
                    }
                }
            }
            .alert("Join a room", isPresented: $showJoin) {
                TextField("Room code", text: $code)
                    .textInputAutocapitalization(.characters)
                Button("Join") { model.joinRoom(code: code); code = "" }
                Button("Cancel", role: .cancel) { code = "" }
            } message: {
                Text("Six letters or digits, as shown on the other device.")
            }
        }
    }
}

struct UserRow: View {
    @EnvironmentObject private var model: AppModel
    let user: UserInfo

    private var presence: String {
        if user.online { return "online" }
        guard let ms = user.lastSeenMs else { return "never seen" }
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
        return "last seen " + RelativeDateTimeFormatter().localizedString(for: date, relativeTo: Date())
    }

    var body: some View {
        HStack(spacing: 12) {
            ZStack(alignment: .bottomTrailing) {
                Circle().fill(Color.accentColor.opacity(0.2)).frame(width: 40, height: 40)
                    .overlay(Text(user.account.displayName.prefix(1).uppercased()).bold())
                Circle().fill(user.online ? .green : .gray).frame(width: 12, height: 12)
                    .overlay(Circle().stroke(Color(.systemBackground), lineWidth: 2))
            }
            VStack(alignment: .leading) {
                Text(user.account.displayName)
                Text("\(user.account.handle) · \(presence)").font(.footnote).foregroundStyle(.secondary)
            }
            Spacer()
            Button { model.activeChat = .dm(userId: user.account.userId) } label: {
                Image(systemName: "bubble.left")
            }
            .buttonStyle(.borderless)
            if model.inRoom {
                Button { model.invite(user: user) } label: { Image(systemName: "person.badge.plus") }
                    .buttonStyle(.borderless)
            } else {
                Button { model.call(user: user) } label: { Image(systemName: "phone.fill") }
                    .buttonStyle(.borderless)
                    .disabled(!user.online || user.devices.isEmpty)
            }
        }
        .padding(.vertical, 2)
    }
}

/// Ringing overlay (SPEC §6/§7): no native call screen for sideloaded apps.
struct IncomingCallView: View {
    @EnvironmentObject private var model: AppModel
    let call: CallInfo

    var body: some View {
        VStack(spacing: 20) {
            Text("Incoming call").font(.headline)
            Text(model.displayName(call.fromUser)).font(.largeTitle).bold()
            HStack(spacing: 40) {
                Button { model.decline(call: call) } label: {
                    Label("Decline", systemImage: "phone.down.fill")
                        .frame(width: 120, height: 48)
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                Button { model.answer(call: call) } label: {
                    Label("Answer", systemImage: "phone.fill")
                        .frame(width: 120, height: 48)
                }
                .buttonStyle(.borderedProminent)
                .tint(.green)
            }
        }
        .padding(30)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 24))
        .padding()
        .frame(maxHeight: .infinity, alignment: .top)
    }
}
