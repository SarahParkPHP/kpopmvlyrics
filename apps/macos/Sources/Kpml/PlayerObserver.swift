import Foundation

/// Bridges the core's `PlaybackObserver` callback interface to Swift handlers.
/// The core invokes these from its player thread, so we hop to the main actor
/// before touching UI state.
final class PlayerObserver: PlaybackObserver {
    private let positionHandler: (VideoPosition) -> Void
    private let errorHandler: (String) -> Void
    private let decoder = JSONDecoder()

    init(
        onPosition: @escaping (VideoPosition) -> Void,
        onError: @escaping (String) -> Void
    ) {
        positionHandler = onPosition
        errorHandler = onError
    }

    func onPosition(positionJson: String) {
        guard let position = try? decoder.decode(VideoPosition.self, from: Data(positionJson.utf8)) else {
            return
        }
        DispatchQueue.main.async { [positionHandler] in positionHandler(position) }
    }

    func onError(message: String) {
        DispatchQueue.main.async { [errorHandler] in errorHandler(message) }
    }
}
