import CoreMedia
import CoreMediaIO
import Foundation
import os.log

private let log = OSLog(subsystem: "com.surlysoft.oak-cam.camera-extension", category: "sink")

/// The sink stream: the oak-cam app attaches to it with
/// `CMIOStreamCopyBufferQueue` + `CMIODeviceStartStream` and enqueues
/// IOSurface-backed sample buffers, which `DeviceSource` relays to the source
/// stream.
final class StreamSink: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private unowned let deviceSource: DeviceSource
    private let streamFormats: [CMIOExtensionStreamFormat]
    private var client: CMIOExtensionClient?

    init(streamID: UUID, kind: CameraKind, deviceName: String, deviceSource: DeviceSource) {
        self.deviceSource = deviceSource
        self.streamFormats = FormatTable.streamFormats(for: kind)
        super.init()
        stream = CMIOExtensionStream(
            localizedName: "\(deviceName) input",
            streamID: streamID,
            direction: .sink,
            clockType: .hostTime,
            source: self
        )
    }

    var formats: [CMIOExtensionStreamFormat] { streamFormats }

    var availableProperties: Set<CMIOExtensionProperty> {
        [
            .streamActiveFormatIndex,
            .streamFrameDuration,
            .streamSinkBufferQueueSize,
            .streamSinkBuffersRequiredForStartup,
            .streamSinkBufferUnderrunCount,
            .streamSinkEndOfData,
        ]
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionStreamProperties
    {
        let props = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            props.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            props.frameDuration = CMTime(value: 1, timescale: FormatTable.nominalFrameRate)
        }
        if properties.contains(.streamSinkBufferQueueSize) {
            props.sinkBufferQueueSize = DeviceSource.sinkQueueDepth
        }
        if properties.contains(.streamSinkBuffersRequiredForStartup) {
            props.sinkBuffersRequiredForStartup = 1
        }
        return props
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}

    /// Any client may publish (a `cargo test` binary has no fixed signing id);
    /// the publisher is logged so an unexpected writer is visible in the log.
    /// CoreMediaIO calls this immediately before `startStream` for the client it
    /// is about to start, so recording the latest here is what `startStream`
    /// then hands to the device.
    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        self.client = client
        os_log(
            .info, log: log, "sink authorized pid %d signingID %{public}@", client.pid,
            client.signingID ?? "-")
        return true
    }

    func startStream() throws {
        guard let client = client else {
            // No authorized client: the publisher never starts and the camera
            // would stay on the placeholder. Log it so a start/stop/start race
            // that lost the client is visible rather than silent.
            os_log(.error, log: log, "startStream with no authorized client; publisher not started")
            return
        }
        os_log(.info, log: log, "sink startStream pid %d", client.pid)
        deviceSource.publisherStarted(client: client)
    }

    func stopStream() throws {
        os_log(.info, log: log, "sink stopStream pid %d", client?.pid ?? -1)
        deviceSource.publisherStopped()
        // Keep the authorized client: CoreMediaIO need not call
        // `authorizedToStartStream` again when the same process restarts the
        // stream, and a start with no client would leave the camera on the
        // held/idle picture until the app relaunched.
    }
}
