import SwiftUI

struct LyricStageView: View {
    let lines: [LyricLine]
    let alignment: [AlignmentLine]
    let activeIndex: Int
    let currentMs: Int
    let languages: Set<LanguageKey>

    private var visible: [LyricLine] {
        lines.filter { abs($0.index - activeIndex) <= 2 }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(formatMs(currentMs))
                .font(.system(.title3, design: .monospaced))
                .foregroundStyle(.secondary)

            if visible.isEmpty {
                Text("Load or import lyrics, then align captions to start synced playback.")
                    .foregroundStyle(.secondary)
            }

            ForEach(visible, id: \.index) { line in
                LyricRow(
                    line: line,
                    timing: alignment.first { $0.lyricIndex == line.index },
                    isActive: line.index == activeIndex,
                    languages: languages
                )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct LyricRow: View {
    let line: LyricLine
    let timing: AlignmentLine?
    let isActive: Bool
    let languages: Set<LanguageKey>

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(line.member ?? "All")
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: 80, alignment: .leading)

            VStack(alignment: .leading, spacing: 2) {
                if languages.contains(.original) {
                    Text(line.original).fontWeight(isActive ? .semibold : .regular)
                }
                if languages.contains(.romanization), let romanization = line.romanization {
                    Text(romanization).font(.callout).foregroundStyle(.secondary)
                }
                if languages.contains(.english), let english = line.english {
                    Text(english).font(.callout).foregroundStyle(.secondary)
                }
            }

            Spacer()

            Text(timing.map { formatMs($0.startMs) } ?? "Unaligned")
                .font(.caption)
                .foregroundStyle(timing?.needsReview == true ? .orange : .secondary)
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 8)
        .background(isActive ? Color.accentColor.opacity(0.12) : .clear)
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}
