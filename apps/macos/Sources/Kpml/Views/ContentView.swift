import SwiftUI

struct ContentView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            VideoSurfaceView(onAttach: state.attachSurface)
                .frame(minHeight: 220)
                .background(Color.black)

            StageBand()

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    AddressRow()
                    if state.settingsOpen { SettingsPanel() }
                    if state.editorOpen { EditorBand() }
                }
                .padding(12)
            }
        }
    }
}

private struct StageBand: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(spacing: 8) {
            HStack(alignment: .top) {
                MemberStripView(
                    members: state.songPackage?.members ?? [],
                    active: state.activeMembers,
                    onPick: state.pickMemberImage
                )
                Spacer()
                LanguageToggles()
            }
            LyricStageView(
                lines: state.songPackage?.lines ?? [],
                alignment: state.alignment,
                activeIndex: state.activeIndex,
                currentMs: state.currentMs,
                languages: state.languages
            )
        }
        .padding(12)
    }
}

private struct LanguageToggles: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "character.bubble")
            ForEach(LanguageKey.allCases) { key in
                Button(key.label) { state.toggleLanguage(key) }
                    .buttonStyle(.bordered)
                    .tint(state.languages.contains(key) ? .accentColor : .gray)
            }
        }
    }
}

private struct AddressRow: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "link")
            TextField("Paste a YouTube MV URL", text: $state.url)
                .textFieldStyle(.roundedBorder)

            Picker("", selection: Binding(
                get: { state.selectedFormatId },
                set: { state.changeQuality($0) }
            )) {
                Text("Auto").tag(autoQuality)
                ForEach(state.availableFormats) { format in
                    Text(format.label).tag(format.formatId)
                }
            }
            .frame(width: 140)
            .disabled(state.busy != nil || state.metadata == nil)

            Button { state.resolveVideo() } label: { Label("Open", systemImage: "magnifyingglass") }
                .disabled(state.busy != nil)
            Button { Task { await state.loadPlayer() } } label: { Label("Stream", systemImage: "play.rectangle") }
                .disabled(state.busy != nil)
            Button { state.editorOpen.toggle() } label: { Label("Editor", systemImage: "pencil") }
            Button { state.settingsOpen.toggle() } label: { Image(systemName: "gearshape") }
        }
    }
}

private struct SettingsPanel: View {
    @EnvironmentObject var state: AppState

    private let columns = [GridItem(.adaptive(minimum: 150), spacing: 8)]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: "magnifyingglass")
                TextField("Artist and song title", text: $state.query)
                    .textFieldStyle(.roundedBorder)
            }
            LazyVGrid(columns: columns, alignment: .leading, spacing: 8) {
                Button { state.fetchLyrics() } label: { Label("Fetch Lyrics", systemImage: "arrow.down.circle") }
                    .disabled(state.busy != nil)
                Button { state.fetchCaptions() } label: { Label("Fetch Captions", systemImage: "clock") }
                    .disabled(state.busy != nil)
                Button { state.align() } label: { Label("Align", systemImage: "checkmark.circle") }
                    .disabled(state.busy != nil || state.songPackage == nil || state.captions.isEmpty)
                Button { state.saveEdits() } label: { Label("Save", systemImage: "square.and.arrow.down") }
                    .disabled(state.busy != nil || state.alignment.isEmpty)
                Button { state.toggleSync() } label: {
                    Label(state.syncRunning ? "Pause Sync" : "Start Sync",
                          systemImage: state.syncRunning ? "pause.fill" : "play.fill")
                }
                .disabled(state.alignment.isEmpty || !state.playerLoaded)
                Button { state.resetSync() } label: { Label("Reset Sync", systemImage: "arrow.counterclockwise") }
                    .disabled(state.alignment.isEmpty || !state.playerLoaded)
            }
            StatusLine()
            MetaGrid()
        }
        .padding(10)
        .background(Color.gray.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

private struct StatusLine: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 6) {
            if let busy = state.busy {
                ProgressView().controlSize(.small)
                Text("\(busy) running")
            } else if state.buffering {
                ProgressView().controlSize(.small)
                Text("Buffering video")
            } else if let message = state.message {
                Image(systemName: "checkmark.circle").foregroundStyle(.green)
                Text(message)
            }
            if let error = state.error {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(.red)
                Text(error).foregroundStyle(.red)
            }
        }
        .font(.callout)
    }
}

private struct MetaGrid: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        let song = state.songPackage.map { "\($0.song.artist) - \($0.song.title)" } ?? "No song"
        HStack(spacing: 16) {
            Text(state.metadata?.videoId ?? "No video")
            Text(song)
            Text("\(state.captions.count) captions")
            Text(state.playerLoaded ? "player ready" : "no stream")
            Text("\(state.reviewCount) review")
            Text(state.syncRunning ? "sync running" : "sync paused")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}

private struct EditorBand: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading) {
                    Text("Manual lyrics").font(.caption)
                    TextEditor(text: $state.manualLyrics).frame(height: 90).border(.gray.opacity(0.3))
                }
                VStack(alignment: .leading) {
                    Text("Manual captions").font(.caption)
                    TextEditor(text: $state.manualCaptions).frame(height: 90).border(.gray.opacity(0.3))
                }
            }
            HStack {
                Button { state.importLyrics() } label: { Label("Import Lyrics", systemImage: "square.and.arrow.up") }
                Button { state.importCaptions() } label: { Label("Import Captions", systemImage: "square.and.arrow.up") }
                Button("-0.5s") { state.shiftAlignment(-500) }
                Button("+0.5s") { state.shiftAlignment(500) }
                Button { state.exportJson() } label: { Label("Export JSON", systemImage: "doc.badge.arrow.up") }
            }
            AlignmentEditorView()
        }
    }
}
