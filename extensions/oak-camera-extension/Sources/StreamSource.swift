import CoreMedia
import CoreMediaIO
import Foundation
import os.log

/// The source stream: what camera consumers open. Frames are pushed into it by
/// `DeviceSource` (relayed sink frames or the placeholder).
final class StreamSource: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private unowned let deviceSource: DeviceSource
    private let streamFormats: [CMIOExtensionStreamFormat]

    /// Set by `DeviceSource` when a relayed frame matches an advertised format.
    var activeFormatIndex = 0

    init(streamID: UUID, kind: CameraKind, deviceName: String, deviceSource: DeviceSource) {
        self.deviceSource = deviceSource
        self.streamFormats = FormatTable.streamFormats(for: kind)
        super.init()
        stream = CMIOExtensionStream(
            localizedName: "\(deviceName) output",
            streamID: streamID,
            direction: .source,
            clockType: .hostTime,
            source: self
        )
    }

    var formats: [CMIOExtensionStreamFormat] { streamFormats }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration]
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionStreamProperties
    {
        let props = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            props.activeFormatIndex = activeFormatIndex
        }
        if properties.contains(.streamFrameDuration) {
            props.frameDuration = CMTime(value: 1, timescale: FormatTable.nominalFrameRate)
        }
        return props
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {
        if let index = streamProperties.activeFormatIndex, index >= 0, index < streamFormats.count {
            activeFormatIndex = index
        }
    }

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        true
    }

    func startStream() throws {
        deviceSource.consumerStarted()
    }

    func stopStream() throws {
        deviceSource.consumerStopped()
    }
}
