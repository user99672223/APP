import SwiftUI

/// Conversations: one per person, plus the current room.
struct ChatsListView: View {
    @EnvironmentObject private var model: AppModel

    private var others: [UserInfo] {
        model.users.filter { $0.account.userId != model.account?.userId }
    }

    var body: some View {
        NavigationStack {
            List {
                if let room = model.room {
                    Section("Room") {
                        NavigationLink(value: ChatKey.room(roomId: room.roomId)) {
                            Label("Room \(room.code)", systemImage: "person.3")
                                .badge(model.unread[.room(roomId: room.roomId)] ?? 0)
                        }
                    }
                }
                Section("Direct messages") {
                    ForEach(others, id: \.account.userId) { user in
                        NavigationLink(value: ChatKey.dm(userId: user.account.userId)) {
                            HStack {
                                Circle().fill(user.online ? .green : .gray).frame(width: 10, height: 10)
                                Text(user.account.displayName)
                            }
                            .badge(model.unread[.dm(userId: user.account.userId)] ?? 0)
                        }
                    }
                }
            }
            .navigationTitle("Chats")
            .navigationDestination(for: ChatKey.self) { key in ChatView(key: key) }
            .navigationDestination(item: $model.activeChat) { key in ChatView(key: key) }
        }
    }
}

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    let key: ChatKey
    @State private var draft = ""

    private var title: String {
        switch key {
        case .dm(let userId): return model.displayName(userId)
        case .room(let roomId): return model.room?.roomId == roomId ? "Room \(model.room?.code ?? "")" : "Room"
        }
    }

    private var entries: [HistoryEntry] { model.messages[key] ?? [] }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(entries, id: \.msgId) { entry in
                            MessageBubble(entry: entry, senderName: model.displayName(entry.fromUser))
                                .id(entry.msgId)
                        }
                    }
                    .padding()
                }
                .onChange(of: entries.count) { _, _ in
                    if let last = entries.last { proxy.scrollTo(last.msgId, anchor: .bottom) }
                }
                .onAppear {
                    if let last = entries.last { proxy.scrollTo(last.msgId, anchor: .bottom) }
                }
            }
            Divider()
            HStack {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...4)
                    .textFieldStyle(.roundedBorder)
                Button {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    model.send(text: text, to: key)
                    draft = ""
                } label: {
                    Image(systemName: "paperplane.fill")
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding()
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("Clear history", role: .destructive) { model.clearHistory(key) }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .onAppear { model.loadHistory(key); model.unread[key] = 0 }
        .onDisappear { if model.activeChat == key { model.activeChat = nil } }
    }
}

struct MessageBubble: View {
    let entry: HistoryEntry
    let senderName: String

    private var time: String {
        let date = Date(timeIntervalSince1970: TimeInterval(entry.sentMs) / 1000)
        return date.formatted(date: .omitted, time: .shortened)
    }

    var body: some View {
        HStack {
            if entry.outgoing { Spacer(minLength: 40) }
            VStack(alignment: entry.outgoing ? .trailing : .leading, spacing: 2) {
                if !entry.outgoing {
                    Text(senderName).font(.caption2).foregroundStyle(.secondary)
                }
                Text(entry.text)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(entry.outgoing ? Color.accentColor : Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 14))
                    .foregroundStyle(entry.outgoing ? .white : .primary)
                HStack(spacing: 4) {
                    Text(time)
                    if entry.outgoing {
                        Image(systemName: entry.delivered ? "checkmark.circle" : "clock")
                    }
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
            if !entry.outgoing { Spacer(minLength: 40) }
        }
    }
}
