import Foundation

// Swift mirrors of the Rust `models.rs` records. Field names match the core's
// serde camelCase JSON exactly, so the default JSONDecoder/Encoder round-trips
// them with no key strategy.

struct VideoMetadata: Codable, Equatable {
    var videoId: String
    var title: String?
    var artistHint: String?
    var originalUrl: String
}

struct VideoFormat: Codable, Equatable, Identifiable {
    var formatId: String
    var label: String
    var height: Int?
    var ext: String?
    var id: String { formatId }
}

struct VideoPosition: Codable, Equatable {
    var ms: UInt64
    var durationMs: UInt64?
    var playing: Bool
    var buffering: Bool
}

struct Song: Codable, Equatable {
    var id: Int?
    var title: String
    var artist: String
    var groupName: String?
    var sourceUrl: String?
}

struct LyricSegment: Codable, Equatable {
    var language: String
    var text: String
    var member: String?
    var color: String?
}

struct LyricLine: Codable, Equatable {
    var id: Int?
    var songId: Int?
    var index: Int
    var member: String?
    var original: String
    var romanization: String?
    var english: String?
    var layer: String?
    var segments: [LyricSegment]?
}

struct CaptionLine: Codable, Equatable {
    var id: Int?
    var videoId: String
    var index: Int
    var startMs: Int
    var endMs: Int
    var text: String
}

struct AlignmentLine: Codable, Equatable {
    var lyricIndex: Int
    var captionIndex: Int?
    var startMs: Int
    var endMs: Int
    var confidence: Double
    var needsReview: Bool
}

struct MemberProfile: Codable, Equatable {
    var id: Int?
    var stageName: String
    var realName: String?
    var color: String
    var imageUrl: String?
    var localImagePath: String?
    var provider: String?
}

struct SongPackage: Codable, Equatable {
    var song: Song
    var lines: [LyricLine]
    var members: [MemberProfile]
    var provider: String
}

enum LanguageKey: String, CaseIterable, Identifiable {
    case original, romanization, english
    var id: String { rawValue }
    var label: String {
        switch self {
        case .original: return "Original"
        case .romanization: return "Roman"
        case .english: return "English"
        }
    }
}
