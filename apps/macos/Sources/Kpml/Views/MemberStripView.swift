import SwiftUI

struct MemberStripView: View {
    let members: [MemberProfile]
    let active: Set<String>
    let onPick: (MemberProfile) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                if members.isEmpty {
                    Text("Members appear after lyrics are loaded")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                ForEach(members, id: \.stageName) { member in
                    MemberChip(
                        member: member,
                        isActive: active.contains(member.stageName),
                        onPick: { onPick(member) }
                    )
                }
            }
        }
    }
}

private struct MemberChip: View {
    let member: MemberProfile
    let isActive: Bool
    let onPick: () -> Void

    private var accent: Color { color(fromHex: member.color) ?? .accentColor }

    var body: some View {
        Button(action: onPick) {
            VStack(spacing: 4) {
                avatar
                    .frame(width: 44, height: 44)
                    .clipShape(Circle())
                    .overlay(Circle().stroke(accent, lineWidth: isActive ? 3 : 1))
                Text(member.stageName).font(.caption2)
            }
        }
        .buttonStyle(.plain)
        .help("Choose member image")
        .opacity(isActive ? 1 : 0.7)
    }

    @ViewBuilder private var avatar: some View {
        if let path = member.localImagePath, let image = NSImage(contentsOfFile: path) {
            Image(nsImage: image).resizable().scaledToFill()
        } else if let urlString = member.imageUrl, let url = URL(string: urlString) {
            AsyncImage(url: url) { image in
                image.resizable().scaledToFill()
            } placeholder: {
                initialsCircle
            }
        } else {
            initialsCircle
        }
    }

    private var initialsCircle: some View {
        ZStack {
            accent.opacity(0.25)
            Text(initials(member.stageName)).font(.caption).bold()
        }
    }
}
