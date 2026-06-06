import SwiftUI

struct AlignmentEditorView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        let lines = state.songPackage?.lines ?? []
        let members = state.songPackage?.members ?? []

        VStack(spacing: 0) {
            header
            ForEach(lines, id: \.index) { line in
                AlignmentRow(line: line, members: members)
                Divider()
            }
        }
        .font(.callout)
    }

    private var header: some View {
        HStack {
            Text("Line").frame(maxWidth: .infinity, alignment: .leading)
            Text("Member").frame(width: 140, alignment: .leading)
            Text("Start").frame(width: 80, alignment: .leading)
            Text("End").frame(width: 80, alignment: .leading)
            Text("Confidence").frame(width: 110, alignment: .leading)
        }
        .font(.caption.bold())
        .foregroundStyle(.secondary)
        .padding(.vertical, 4)
    }
}

private struct AlignmentRow: View {
    @EnvironmentObject var state: AppState
    let line: LyricLine
    let members: [MemberProfile]

    private var timing: AlignmentLine? {
        state.alignment.first { $0.lyricIndex == line.index }
    }

    var body: some View {
        HStack {
            Text(line.original)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)

            Picker("", selection: memberBinding) {
                Text("All").tag("")
                ForEach(members, id: \.stageName) { Text($0.stageName).tag($0.stageName) }
            }
            .labelsHidden()
            .frame(width: 140)

            TextField("", value: startBinding, format: .number)
                .frame(width: 80)
                .textFieldStyle(.roundedBorder)

            TextField("", value: endBinding, format: .number)
                .frame(width: 80)
                .textFieldStyle(.roundedBorder)

            Text(confidenceLabel)
                .frame(width: 110, alignment: .leading)
                .foregroundStyle(timing?.needsReview == true ? .orange : .secondary)
        }
        .padding(.vertical, 2)
    }

    private var confidenceLabel: String {
        let pct = Int((timing?.confidence ?? 0) * 100)
        return "\(pct)%" + (timing?.needsReview == true ? " review" : "")
    }

    private var memberBinding: Binding<String> {
        Binding(
            get: { line.member ?? "" },
            set: { state.setMember(lineIndex: line.index, member: $0) }
        )
    }

    private var startBinding: Binding<Int> {
        Binding(
            get: { timing?.startMs ?? 0 },
            set: { state.updateAlignment(lyricIndex: line.index, startMs: $0) }
        )
    }

    private var endBinding: Binding<Int> {
        Binding(
            get: { timing?.endMs ?? 1200 },
            set: { state.updateAlignment(lyricIndex: line.index, endMs: $0) }
        )
    }
}
