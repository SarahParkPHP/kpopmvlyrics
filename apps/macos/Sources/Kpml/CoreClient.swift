import Foundation

/// Thin, type-safe Swift wrapper over the UniFFI-generated `Core`.
///
/// Structured data crosses as JSON through the shared command surface. The
/// underlying Rust `Core` is `Send + Sync`, so this is safe to call off the main
/// actor (`@unchecked Sendable`).
final class CoreClient: @unchecked Sendable {
    private let core: Core
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()

    init() throws {
        core = try Core()
    }

    func setObserver(_ observer: PlaybackObserver) {
        core.setObserver(observer: observer)
    }

    // MARK: - Command surface (mirrors AppContext)

    func resolveVideoMetadata(_ url: String) throws -> VideoMetadata {
        try invoke("resolve_video_metadata", ["url": url])
    }

    func listVideoFormats(_ url: String) throws -> [VideoFormat] {
        try invoke("list_video_formats", ["url": url])
    }

    /// Returns the raw `StreamSpec` JSON, ready to hand to `playerLoad`.
    func resolveStreamJSON(url: String, formatId: String?) throws -> String {
        try invokeRaw("resolve_stream", encode(["url": url, "formatId": formatId]))
    }

    func fetchLyrics(_ query: String) throws -> SongPackage {
        try invoke("fetch_lyrics", ["query": query])
    }

    func importLyrics(rawText: String, title: String, artist: String) throws -> SongPackage {
        try invoke("import_lyrics", ["rawText": rawText, "title": title, "artist": artist])
    }

    func fetchCaptions(videoId: String) throws -> [CaptionLine] {
        try invoke("fetch_captions", ["videoId": videoId])
    }

    func importCaptions(videoId: String, rawText: String) throws -> [CaptionLine] {
        try invoke("import_captions", ["videoId": videoId, "rawText": rawText])
    }

    func alignLyrics(songId: Int, videoId: String) throws -> [AlignmentLine] {
        try invoke("align_lyrics", JSONArgs(songId: songId, videoId: videoId))
    }

    func saveAlignmentEdits(songId: Int, videoId: String, lines: [AlignmentLine]) throws {
        try invokeVoid("save_alignment_edits", SaveAlignArgs(songId: songId, videoId: videoId, lines: lines))
    }

    func searchMemberProfiles(groupName: String) throws -> [MemberProfile] {
        try invoke("search_member_profiles", ["groupName": groupName])
    }

    func saveMemberOverride(groupName: String, member: MemberProfile) throws -> MemberProfile {
        try invoke("save_member_override", SaveMemberArgs(groupName: groupName, member: member))
    }

    /// Canonical export JSON (built in the Rust core, shared with GTK/legacy UI).
    func buildExport(metadata: VideoMetadata, songPackage: SongPackage, alignment: [AlignmentLine]) throws -> String {
        try invokeRaw("build_export", encode(ExportArgs(metadata: metadata, songPackage: songPackage, alignment: alignment)))
    }

    // MARK: - Player surface

    func playerAttachSurface(handle: UInt64, x: Int32, y: Int32, width: Int32, height: Int32) throws {
        try core.playerAttachSurface(handle: handle, x: x, y: y, width: width, height: height)
    }

    func playerLoad(streamSpecJSON: String) throws { try core.playerLoad(streamSpecJson: streamSpecJSON) }
    func playerPlay() throws { try core.playerPlay() }
    func playerPause() throws { try core.playerPause() }
    func playerSeek(ms: UInt64) throws { try core.playerSeek(ms: ms) }
    func playerSnapshot() throws -> VideoPosition {
        try decoder.decode(VideoPosition.self, from: Data(try core.playerSnapshot().utf8))
    }

    // MARK: - JSON plumbing

    private func invokeRaw(_ command: String, _ argsJSON: String) throws -> String {
        try core.invoke(command: command, argsJson: argsJSON)
    }

    private func encode<A: Encodable>(_ args: A) throws -> String {
        String(data: try encoder.encode(args), encoding: .utf8) ?? "{}"
    }

    private func invoke<R: Decodable, A: Encodable>(_ command: String, _ args: A) throws -> R {
        let json = try invokeRaw(command, try encode(args))
        return try decoder.decode(R.self, from: Data(json.utf8))
    }

    private func invokeVoid<A: Encodable>(_ command: String, _ args: A) throws {
        _ = try invokeRaw(command, try encode(args))
    }
}

// Encodable argument payloads. Simple string maps use `[String: String?]`;
// the rest use small typed structs so nested records encode cleanly.

private struct JSONArgs: Encodable {
    let songId: Int
    let videoId: String
}

private struct SaveAlignArgs: Encodable {
    let songId: Int
    let videoId: String
    let lines: [AlignmentLine]
}

private struct SaveMemberArgs: Encodable {
    let groupName: String
    let member: MemberProfile
}

private struct ExportArgs: Encodable {
    let metadata: VideoMetadata
    let songPackage: SongPackage
    let alignment: [AlignmentLine]
}
