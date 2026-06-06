import Foundation
import SwiftUI
import UniformTypeIdentifiers

func formatMs(_ ms: Int) -> String {
    let safe = max(0, ms)
    let minutes = safe / 60_000
    let seconds = (safe % 60_000) / 1_000
    let millis = String(format: "%03d", safe % 1_000)
    return "\(minutes):" + String(format: "%02d", seconds) + ".\(millis)"
}

func initials(_ name: String) -> String {
    name.split(whereSeparator: { $0.isWhitespace })
        .prefix(2)
        .compactMap { $0.first.map { String($0).uppercased() } }
        .joined()
}

func safeName(_ value: String) -> String {
    let trimmed = value.trimmingCharacters(in: .whitespaces)
    let replaced = trimmed.replacingOccurrences(
        of: "[^a-z0-9._-]+", with: "-", options: [.regularExpression, .caseInsensitive])
    let dashes = CharacterSet(charactersIn: "-")
    let limited = String(replaced.trimmingCharacters(in: dashes).prefix(60))
    return limited.isEmpty ? "export" : limited
}

func cleanVideoTitle(_ title: String) -> String {
    var cleaned = title
    let substitutions: [(String, String)] = [
        ("\\s+-\\s+YouTube$", " "),
        ("\\s*\\[[^\\]]*(official|mv|m/v|music video)[^\\]]*\\]\\s*", " "),
        ("\\s*\\((official\\s*)?(mv|m/v|music video|official video)\\)\\s*", " "),
        ("\\s+(official\\s*)?(mv|m/v|music video|official video)$", ""),
    ]
    for (pattern, replacement) in substitutions {
        cleaned = cleaned.replacingOccurrences(
            of: pattern, with: replacement, options: [.regularExpression, .caseInsensitive])
    }
    cleaned = cleaned.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
    return cleaned.trimmingCharacters(in: .whitespaces)
}

func mergeMembers(_ primary: [MemberProfile], _ secondary: [MemberProfile]) -> [MemberProfile] {
    if primary.isEmpty { return secondary }
    var byName: [String: MemberProfile] = [:]
    var order: [String] = []
    for member in primary {
        let key = member.stageName.lowercased()
        byName[key] = member
        order.append(key)
    }
    for member in secondary {
        let key = member.stageName.lowercased()
        let existing = byName[key] ?? byName.values.first { namesMatch($0.stageName, member.stageName) }
        guard let existing else { continue }
        var merged = existing
        if merged.imageUrl == nil { merged.imageUrl = member.imageUrl }
        byName[existing.stageName.lowercased()] = merged
    }
    return order.compactMap { byName[$0] }
}

private func namesMatch(_ left: String, _ right: String) -> Bool {
    func normalize(_ value: String) -> String {
        var lowered = value.lowercased()
        for prefix in ["kim ", "huh ", "hong ", "miyawaki ", "nakamura "] {
            lowered = lowered.replacingOccurrences(of: prefix, with: "")
        }
        return String(lowered.unicodeScalars.filter { CharacterSet.lowercaseLetters.contains($0) })
    }
    let a = normalize(left)
    let b = normalize(right)
    return !a.isEmpty && !b.isEmpty && (a == b || a.contains(b) || b.contains(a))
}

func imageContentTypes() -> [UTType] {
    [.jpeg, .png, .gif, .webP]
}

/// Parse a `#rrggbb`/`#rgb` hex string into a SwiftUI Color (member accents).
func color(fromHex hex: String?) -> Color? {
    guard let hex else { return nil }
    var value = hex.trimmingCharacters(in: .whitespaces)
    guard value.hasPrefix("#") else { return nil }
    value.removeFirst()
    if value.count == 3 {
        value = value.map { "\($0)\($0)" }.joined()
    }
    guard value.count == 6, let int = UInt32(value, radix: 16) else { return nil }
    return Color(
        red: Double((int >> 16) & 0xff) / 255,
        green: Double((int >> 8) & 0xff) / 255,
        blue: Double(int & 0xff) / 255
    )
}
