import SwiftUI
import WidgetKit
import OSLog

private let appGroup = "group.com.minchenlee.c9watch"
private let snapshotFile = "widget.json"
private let localSnapshotDirectory = "c9watch"
private let widgetLogger = Logger(subsystem: "com.minchenlee.c9watch.widget", category: "snapshot")

struct C9WatchSnapshot: Codable {
    let hasSession: Bool
    let provider: String
    let title: String
    let status: String
    let project: String
    let latestMessage: String
    let workingCount: Int
    let approvalCount: Int
    let waitingCount: Int

    init(
        hasSession: Bool,
        provider: String,
        title: String,
        status: String,
        project: String,
        latestMessage: String,
        workingCount: Int = 0,
        approvalCount: Int = 0,
        waitingCount: Int = 0
    ) {
        self.hasSession = hasSession
        self.provider = provider
        self.title = title
        self.status = status
        self.project = project
        self.latestMessage = latestMessage
        self.workingCount = workingCount
        self.approvalCount = approvalCount
        self.waitingCount = waitingCount
    }

    enum CodingKeys: String, CodingKey {
        case hasSession, provider, title, status, project, latestMessage
        case workingCount, approvalCount, waitingCount
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        hasSession = try container.decode(Bool.self, forKey: .hasSession)
        provider = try container.decode(String.self, forKey: .provider)
        title = try container.decode(String.self, forKey: .title)
        status = try container.decode(String.self, forKey: .status)
        project = try container.decode(String.self, forKey: .project)
        latestMessage = try container.decode(String.self, forKey: .latestMessage)
        workingCount = try container.decodeIfPresent(Int.self, forKey: .workingCount) ?? 0
        approvalCount = try container.decodeIfPresent(Int.self, forKey: .approvalCount) ?? 0
        waitingCount = try container.decodeIfPresent(Int.self, forKey: .waitingCount) ?? 0
    }
}

struct C9WatchEntry: TimelineEntry {
    let date: Date
    let snapshot: C9WatchSnapshot
}

struct C9WatchProvider: TimelineProvider {
    private let placeholderSnapshot = C9WatchSnapshot(
        hasSession: true,
        provider: "CHATGPT / CODEX",
        title: "Building something useful",
        status: "Working now",
        project: "c9watch",
        latestMessage: "The active AI session will appear here.",
        workingCount: 1,
        approvalCount: 0,
        waitingCount: 0
    )

    func placeholder(in context: Context) -> C9WatchEntry {
        C9WatchEntry(date: .now, snapshot: placeholderSnapshot)
    }

    func getSnapshot(in context: Context, completion: @escaping (C9WatchEntry) -> Void) {
        completion(C9WatchEntry(date: .now, snapshot: readSnapshot()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<C9WatchEntry>) -> Void) {
        let entry = C9WatchEntry(date: .now, snapshot: readSnapshot())
        // Keep the dashboard responsive while the app is watching sessions.
        // WidgetKit may still apply its own refresh budget, but this prevents
        // an old zero-count timeline from living for a full minute.
        completion(Timeline(entries: [entry], policy: .after(.now.addingTimeInterval(15))))
    }

    private func readSnapshot() -> C9WatchSnapshot {
        for url in snapshotURLs() {
            guard let data = try? Data(contentsOf: url) else { continue }
            guard let snapshot = try? JSONDecoder().decode(C9WatchSnapshot.self, from: data) else {
                widgetLogger.error("Unable to decode snapshot file: \(url.path, privacy: .public)")
                continue
            }

            widgetLogger.info(
                "Loaded snapshot from \(url.path, privacy: .public): hasSession=\(snapshot.hasSession, privacy: .public), working=\(snapshot.workingCount, privacy: .public), approval=\(snapshot.approvalCount, privacy: .public), waiting=\(snapshot.waitingCount, privacy: .public)"
            )
            return snapshot
        }

        widgetLogger.error("Unable to read snapshot from App Group or local widget container")
        return fallbackSnapshot()
    }

    private func snapshotURLs() -> [URL] {
        var urls: [URL] = []
        if let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroup
        ) {
            urls.append(container.appendingPathComponent(snapshotFile))
        } else {
            widgetLogger.error("Unable to resolve app-group container: \(appGroup, privacy: .public)")
        }

        if let applicationSupport = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first {
            urls.append(
                applicationSupport
                    .appendingPathComponent(localSnapshotDirectory, isDirectory: true)
                    .appendingPathComponent(snapshotFile)
            )
        }
        return urls
    }

    private func fallbackSnapshot() -> C9WatchSnapshot {
        C9WatchSnapshot(
            hasSession: false,
            provider: "c9watch",
            title: "No active session",
            status: "Waiting",
            project: "",
            latestMessage: "Start a Codex or Claude Code session.",
            workingCount: 0,
            approvalCount: 0,
            waitingCount: 0
        )
    }
}

struct C9WatchWidgetView: View {
    let entry: C9WatchEntry
    @Environment(\.widgetFamily) private var family

    private var snapshot: C9WatchSnapshot { entry.snapshot }
    private var accent: Color {
        snapshot.hasSession
            ? Color(red: 0.44, green: 0.76, blue: 1.0)
            : Color.white.opacity(0.45)
    }

    var body: some View {
        Group {
            switch family {
            case .systemSmall:
                smallLayout
            case .systemLarge:
                largeLayout
            default:
                mediumLayout
            }
        }
        .containerBackground(for: .widget) {
            ZStack {
                LinearGradient(
                    colors: [
                        Color(red: 0.07, green: 0.10, blue: 0.18),
                        Color(red: 0.12, green: 0.08, blue: 0.24)
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )

                Circle()
                    .fill(accent.opacity(0.20))
                    .frame(width: 180, height: 180)
                    .blur(radius: 34)
                    .offset(x: 92, y: -92)
            }
        }
    }

    private var smallLayout: some View {
        VStack(alignment: .leading, spacing: 12) {
            header

            Spacer(minLength: 0)

            Text(snapshot.title)
                .font(.title3.weight(.semibold))
                .foregroundStyle(.white)
                .lineLimit(3)

            statusBadge

            if !snapshot.project.isEmpty {
                projectLabel
            }
        }
        .padding(16)
    }

    private var mediumLayout: some View {
        VStack(alignment: .leading, spacing: 14) {
            header

            Text(snapshot.title)
                .font(.title2.weight(.semibold))
                .foregroundStyle(.white)
                .lineLimit(2)

            Text(snapshot.latestMessage)
                .font(.subheadline)
                .foregroundStyle(.white.opacity(0.72))
                .lineLimit(2)

            Spacer(minLength: 0)

            HStack {
                statusBadge
                Spacer()
                projectLabel
            }
        }
        .padding(18)
    }

    private var largeLayout: some View {
        VStack(alignment: .leading, spacing: 10) {
            header

            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(snapshot.title)
                    .font(.title2.weight(.bold))
                    .foregroundStyle(.white)
                    .lineLimit(2)

                Spacer(minLength: 0)

                statusBadge
            }

            Text(snapshot.latestMessage)
                .font(.caption)
                .foregroundStyle(.white.opacity(0.78))
                .lineLimit(2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))

            statusOverview

            Spacer(minLength: 0)

            HStack {
                projectLabel
                Spacer()
                Text("Live session overview")
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.white.opacity(0.42))
            }
        }
        .padding(14)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 11) {
            Image(nsImage: NSImage(named: NSImage.applicationIconName) ?? NSImage())
                .resizable()
                .scaledToFill()
                .frame(width: 42, height: 42)
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .overlay {
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(.white.opacity(0.18), lineWidth: 1)
                }

            VStack(alignment: .leading, spacing: 3) {
                Text(snapshot.provider)
                    .font(.caption.weight(.bold))
                    .tracking(0.5)
                    .foregroundStyle(.white.opacity(0.68))
                    .lineLimit(1)

                Text("c9watch")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.white)
            }

            Spacer(minLength: 0)

            Image(systemName: snapshot.hasSession ? "waveform" : "moon.zzz.fill")
                .font(.title3.weight(.semibold))
                .foregroundStyle(accent)
        }
    }

    private var statusBadge: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(accent)
                .frame(width: 7, height: 7)
                .shadow(color: accent, radius: snapshot.hasSession ? 5 : 0)

            Text(snapshot.status)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.white.opacity(0.84))
                .lineLimit(1)
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
        .background(.white.opacity(0.10), in: Capsule())
    }

    private var statusOverview: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Text("SESSION STATUS")
                    .font(.caption2.weight(.bold))
                    .tracking(1.0)
                    .foregroundStyle(.white.opacity(0.5))

                Spacer()

                Text("\(snapshot.workingCount + snapshot.approvalCount + snapshot.waitingCount) total")
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.white.opacity(0.45))
            }

            statusRow(
                label: "WORKING",
                count: snapshot.workingCount,
                icon: "bolt.fill",
                color: Color(red: 0.34, green: 0.82, blue: 0.72)
            )
            statusRow(
                label: "APPROVAL",
                count: snapshot.approvalCount,
                icon: "hand.raised.fill",
                color: Color(red: 1.0, green: 0.65, blue: 0.30)
            )
            statusRow(
                label: "WAITING",
                count: snapshot.waitingCount,
                icon: "clock.fill",
                color: Color(red: 0.60, green: 0.68, blue: 1.0)
            )
        }
        .padding(10)
        .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 16))
        .overlay {
            RoundedRectangle(cornerRadius: 16)
                .stroke(.white.opacity(0.08), lineWidth: 1)
        }
    }

    private func statusRow(label: String, count: Int, icon: String, color: Color) -> some View {
        HStack(spacing: 9) {
            Image(systemName: icon)
                .font(.caption2.weight(.bold))
                .foregroundStyle(color)
                .frame(width: 17)

            Text(label)
                .font(.caption2.weight(.bold))
                .tracking(0.8)
                .foregroundStyle(.white.opacity(0.78))

            Spacer()

            Text("\(count)")
                .font(.headline.weight(.bold).monospacedDigit())
                .foregroundStyle(.white)
        }
    }

    private var projectLabel: some View {
        Label(snapshot.project.isEmpty ? "No project" : snapshot.project, systemImage: "folder.fill")
            .font(.caption2.weight(.medium))
            .foregroundStyle(.white.opacity(0.58))
            .lineLimit(1)
    }
}

struct C9WatchWidget: Widget {
    let kind = "C9WatchWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: C9WatchProvider()) { entry in
            C9WatchWidgetView(entry: entry)
        }
        .configurationDisplayName("c9watch session")
        .description("See the active Codex or Claude Code session at a glance.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
        .contentMarginsDisabled()
    }
}

@main
struct C9WatchWidgetBundle: WidgetBundle {
    var body: some Widget {
        C9WatchWidget()
    }
}
