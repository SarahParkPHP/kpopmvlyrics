import SwiftUI

@main
struct KpmlApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        WindowGroup("K-Pop MV Lyrics") {
            ContentView()
                .environmentObject(state)
                .frame(minWidth: 900, minHeight: 640)
        }
        .windowStyle(.titleBar)
    }
}
