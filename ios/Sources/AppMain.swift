import SwiftUI

@main
struct APPMain: App {
    @StateObject private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .onOpenURL { url in
                    model.handleDeepLink(url)
                }
                .alert("Room invite", isPresented: Binding(get: { model.roomInvite != nil }, set: { if !$0 { model.roomInvite = nil } })) {
                    Button("Join") {
                        if let room = model.roomInvite { model.joinRoom(id: room.roomId) }
                        model.roomInvite = nil
                    }
                    Button("Ignore", role: .cancel) { model.roomInvite = nil }
                } message: {
                    Text("\(model.displayName(model.roomInviteFrom)) invited you to room \(model.roomInvite?.code ?? "").")
                }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { model.didBecomeActive() }
        }
    }
}

