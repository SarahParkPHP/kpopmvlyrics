import AppKit
import Foundation
import UniformTypeIdentifiers

struct AppError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

let autoQuality = "auto"

/// The app's single observable view-model. It talks to the Rust core through
/// `CoreClient`.
@MainActor
final class AppState: ObservableObject {
    @Published var url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    @Published var query = ""
    @Published var metadata: VideoMetadata?
    @Published var songPackage: SongPackage?
    @Published var captions: [CaptionLine] = []
    @Published var alignment: [AlignmentLine] = []
    @Published var languages: Set<LanguageKey> = [.original, .english]
    @Published var availableFormats: [VideoFormat] = []
    @Published var selectedFormatId = autoQuality

    @Published var editorOpen = false
    @Published var settingsOpen = false
    @Published var manualLyrics = "Nayeon: Tell me what you want\nMomo: Tell me what you need\nSana: A to Z da malhaebwa"
    @Published var manualCaptions = "WEBVTT\n\n00:00:01.000 --> 00:00:02.400\nTell me what you want"

    @Published var busy: String?
    @Published var message: String?
    @Published var error: String?
    @Published var playerLoaded = false
    @Published var buffering = false
    @Published var syncRunning = false
    @Published var currentMs = 0

    private let client: CoreClient?
    private var observer: PlayerObserver?

    init() {
        client = try? CoreClient()
        if client == nil {
            error = "Failed to open the application core"
            return
        }
        let observer = PlayerObserver(
            onPosition: { [weak self] position in
                self?.currentMs = Int(position.ms)
                self?.buffering = position.buffering
                self?.syncRunning = position.playing
            },
            onError: { [weak self] message in self?.error = message }
        )
        self.observer = observer
        client?.setObserver(observer)
    }

    // MARK: - Derived

    var activeIndex: Int {
        if let found = alignment.first(where: { currentMs >= $0.startMs && currentMs <= $0.endMs }) {
            return found.lyricIndex
        }
        let previous = alignment
            .filter { currentMs >= $0.startMs }
            .max(by: { $0.startMs < $1.startMs })
        return previous?.lyricIndex ?? 0
    }

    var activeMembers: Set<String> {
        guard let member = songPackage?.lines.first(where: { $0.index == activeIndex })?.member else {
            return []
        }
        return [member]
    }

    var reviewCount: Int { alignment.filter { $0.needsReview }.count }

    // MARK: - Actions

    func resolveVideo() {
        let url = self.url
        Task {
            busy = "Video"; error = nil; message = nil
            defer { busy = nil }
            do {
                let metadata = try await background { try $0.resolveVideoMetadata(url) }
                self.metadata = metadata
                let lyricQuery = cleanVideoTitle(metadata.title ?? metadata.originalUrl)
                query = lyricQuery
                captions = []; alignment = []; syncRunning = false; currentMs = 0
                playerLoaded = false; availableFormats = []; selectedFormatId = autoQuality

                busy = "Video formats"
                availableFormats = (try? await background { try $0.listVideoFormats(url) }) ?? []

                guard !lyricQuery.isEmpty else { message = "Video complete"; return }

                _ = await loadPlayer()

                busy = "Lyrics"
                let lyrics = try await background { try $0.fetchLyrics(lyricQuery) }
                await applySongPackage(lyrics)

                busy = "Captions"
                let fetched = try await background { try $0.fetchCaptions(videoId: metadata.videoId) }
                captions = fetched

                if let songId = lyrics.song.id, !fetched.isEmpty {
                    busy = "Alignment"
                    alignment = try await background { try $0.alignLyrics(songId: songId, videoId: metadata.videoId) }
                    message = "Video, lyrics, captions, and alignment complete"
                } else {
                    message = "Lyrics complete"
                }
            } catch {
                self.error = describe(error)
            }
        }
    }

    @discardableResult
    func loadPlayer(formatId: String? = nil) async -> Bool {
        let chosen = formatId ?? (selectedFormatId == autoQuality ? nil : selectedFormatId)
        let url = self.url
        buffering = true; error = nil
        defer { buffering = false }
        do {
            let spec = try await background { try $0.resolveStreamJSON(url: url, formatId: chosen) }
            try await background { try $0.playerLoad(streamSpecJSON: spec) }
            playerLoaded = true
            return true
        } catch {
            self.error = describe(error)
            return false
        }
    }

    func fetchLyrics() {
        let query = self.query
        Task {
            if let result = await run("Lyrics", { try $0.fetchLyrics(query) }) {
                await applySongPackage(result)
            }
        }
    }

    func importLyrics() {
        let query = self.query
        let raw = self.manualLyrics
        Task {
            let title = query.isEmpty ? "Imported Song" : query
            let artist = query.split(separator: " ").first.map(String.init) ?? "Imported Group"
            if let result = await run("Lyric import", { try $0.importLyrics(rawText: raw, title: title, artist: artist) }) {
                songPackage = result
            }
        }
    }

    func fetchCaptions() {
        guard let videoId = metadata?.videoId else { error = "Resolve a YouTube URL first"; return }
        Task {
            if let result = await run("Captions", { try $0.fetchCaptions(videoId: videoId) }) {
                captions = result
            }
        }
    }

    func importCaptions() {
        guard let videoId = metadata?.videoId else { error = "Resolve a YouTube URL first"; return }
        let raw = manualCaptions
        Task {
            if let result = await run("Caption import", { try $0.importCaptions(videoId: videoId, rawText: raw) }) {
                captions = result
            }
        }
    }

    func align() {
        guard let songId = songPackage?.song.id, let videoId = metadata?.videoId else {
            error = "Load lyrics and resolve a video first"; return
        }
        Task {
            if let result = await run("Alignment", { try $0.alignLyrics(songId: songId, videoId: videoId) }) {
                alignment = result
            }
        }
    }

    func saveEdits() {
        guard let songId = songPackage?.song.id, let videoId = metadata?.videoId else { return }
        let lines = alignment
        Task { await run("Save", { try $0.saveAlignmentEdits(songId: songId, videoId: videoId, lines: lines) }) }
    }

    func changeQuality(_ formatId: String) {
        selectedFormatId = formatId
        guard playerLoaded else { return }
        let resumeAt = UInt64(max(0, currentMs))
        Task {
            if await loadPlayer(formatId: formatId == autoQuality ? nil : formatId) {
                try? await background { try $0.playerSeek(ms: resumeAt) }
            }
        }
    }

    func toggleSync() {
        Task {
            if syncRunning {
                try? await background { try $0.playerPause() }
            } else {
                try? await background { try $0.playerPlay() }
            }
        }
    }

    func resetSync() {
        Task {
            try? await background { try $0.playerPause() }
            try? await background { try $0.playerSeek(ms: 0) }
            syncRunning = false
            currentMs = 0
        }
    }

    func shiftAlignment(_ deltaMs: Int) {
        alignment = alignment.map {
            var line = $0
            line.startMs = max(0, line.startMs + deltaMs)
            line.endMs = max(0, line.endMs + deltaMs)
            line.needsReview = true
            return line
        }
    }

    func setMember(lineIndex: Int, member: String) {
        guard var pkg = songPackage else { return }
        pkg.lines = pkg.lines.map { line in
            var copy = line
            if line.index == lineIndex { copy.member = member.isEmpty ? nil : member }
            return copy
        }
        songPackage = pkg
    }

    func updateAlignment(lyricIndex: Int, startMs: Int? = nil, endMs: Int? = nil) {
        alignment = alignment.map { line in
            guard line.lyricIndex == lyricIndex else { return line }
            var copy = line
            if let startMs { copy.startMs = startMs }
            if let endMs { copy.endMs = endMs }
            copy.needsReview = true
            return copy
        }
    }

    func toggleLanguage(_ key: LanguageKey) {
        if languages.contains(key) { languages.remove(key) } else { languages.insert(key) }
    }

    func pickMemberImage(_ member: MemberProfile) {
        guard let pkg = songPackage else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = imageContentTypes()
        guard panel.runModal() == .OK, let path = panel.url?.path else { return }
        var updated = member
        updated.localImagePath = path
        let groupName = pkg.song.groupName ?? pkg.song.artist
        Task {
            _ = try? await background { try $0.saveMemberOverride(groupName: groupName, member: updated) }
            var copy = pkg
            copy.members = copy.members.map { $0.stageName == member.stageName ? updated : $0 }
            songPackage = copy
        }
    }

    func exportJson() {
        guard let metadata, let songPackage else { error = "Load lyrics and resolve a video first"; return }
        let alignment = self.alignment
        Task {
            do {
                let json = try await background { try $0.buildExport(metadata: metadata, songPackage: songPackage, alignment: alignment) }
                let panel = NSSavePanel()
                panel.allowedContentTypes = [.json]
                panel.nameFieldStringValue = "\(safeName(songPackage.song.artist))-\(safeName(songPackage.song.title))-\(safeName(metadata.videoId)).json"
                guard panel.runModal() == .OK, let target = panel.url else { return }
                try (json + "\n").write(to: target, atomically: true, encoding: .utf8)
                message = "JSON export complete"; error = nil
            } catch {
                self.error = describe(error)
            }
        }
    }

    func attachSurface(handle: UInt64, rect: CGRect) {
        Task {
            try? await background {
                try $0.playerAttachSurface(
                    handle: handle,
                    x: 0,
                    y: 0,
                    width: Int32(rect.width),
                    height: Int32(rect.height)
                )
            }
        }
    }

    // MARK: - Plumbing

    @discardableResult
    private func run<T: Sendable>(_ label: String, _ work: @escaping @Sendable (CoreClient) throws -> T) async -> T? {
        busy = label; error = nil; message = nil
        defer { busy = nil }
        do {
            let result = try await background(work)
            message = "\(label) complete"
            return result
        } catch {
            self.error = describe(error)
            return nil
        }
    }

    private func background<T: Sendable>(_ work: @escaping @Sendable (CoreClient) throws -> T) async throws -> T {
        guard let client else { throw AppError(message: "Application core is unavailable") }
        return try await Task.detached { try work(client) }.value
    }

    private func applySongPackage(_ result: SongPackage) async {
        songPackage = result
        guard let groupName = result.song.groupName else { return }
        if let profiles = try? await background({ try $0.searchMemberProfiles(groupName: groupName) }), !profiles.isEmpty {
            var merged = result
            merged.members = mergeMembers(result.members, profiles)
            songPackage = merged
        }
    }

    private func describe(_ error: Error) -> String {
        (error as? LocalizedError)?.errorDescription ?? "\(error)"
    }
}
