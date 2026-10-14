import CoreMedia
import CoreMediaIO
import CoreVideo
import Foundation
import os.log

private let log = OSLog(subsystem: "com.surlysoft.oak-cam.camera-extension", category: "device")

/// `kIOAudioDeviceTransportTypeVirtual` ('virt'), spelled out so the extension
/// does not import IOKit for one constant.
private let transportTypeVirtual: Int = 0x7669_7274

/// The camera device: a source stream consumers read and a sink stream the app
/// writes. All state changes run on `queue`; CoreMediaIO calls the stream
/// sources from its own threads and those hop onto the queue.
///
/// The picture a consumer sees is chosen by [`FrameArbiter`] (a pure, host-
/// tested state machine); this type is the CoreMediaIO glue around it. Three
/// sources, chosen so the camera never shows a frozen or missing picture and
/// never greys out on a reconnect:
///
/// - **relay** — the app is publishing; its frames are relayed straight through.
/// - **held** — the app stalled (> 1 s) or stopped (clean `stopStream`, quit,
///   crash): the last relayed picture is copied once, a "Reconnecting…" badge is
///   drawn on it, and it is re-sent at the nominal rate until the app relays
///   again (straight back to relay, no grey flash) or, after a *clean* stop, the
///   60-second hold times out and idle takes over. An unclean loss holds
///   indefinitely while a consumer is open.
/// - **idle** — nothing has ever been published, or a hold timed out: the grey
///   placeholder with luma bars and a "no signal" caption.
///
/// Two reconnect bugs the arbiter fixes: every frame is stamped through the
/// arbiter's monotonic cursor (a relayed frame after a gap could otherwise carry
/// a host time earlier than the last placeholder, which stalls or greys an
/// AVFoundation consumer), and each consume loop carries a generation token so a
/// stale loop from a previous publisher exits instead of spinning against a dead
/// client next to the new one.
final class DeviceSource: NSObject, CMIOExtensionDeviceSource {
    /// Sample buffers the sink queue holds before the app sees `QueueIsFull`.
    static let sinkQueueDepth = 3
    private static let outputInterval = 1.0 / Double(FormatTable.nominalFrameRate)

    private(set) var device: CMIOExtensionDevice!
    private var source: StreamSource!
    private var sink: StreamSink!

    /// The camera's display name ("OAK RGB"), also the log prefix.
    let name: String
    /// The sensor kind, which picks the advertised formats.
    let kind: CameraKind

    /// This device's own serial queue: each camera relays independently.
    private let queue: DispatchQueue
    private var consumers: UInt32 = 0
    private var publishing = false
    /// Bumped on every publisher start/stop so a stale consume loop (its
    /// completion scheduled against a dead client) sees a mismatch and exits.
    private var generation: UInt64 = 0
    private let arbiter = FrameArbiter()
    private var loggedState: FrameArbiter.State?

    /// The app's most recent relayed sample, retained only while relaying (one
    /// of the sink pool's six buffers). Copied into `heldFrame` and released on
    /// entering the held state.
    private var lastRelayedSample: CMSampleBuffer?
    /// The held picture (last frame + badge), built once on entering the held
    /// state and re-sent each tick.
    private var heldFrame: HeldFrame?

    private var outputTimer: DispatchSourceTimer?
    private var placeholder: Placeholder

    init(identity: ExtensionIdentity) {
        name = identity.deviceName
        kind = identity.kind
        queue = DispatchQueue(
            label: "com.surlysoft.oak-cam.camera-extension.device.\(identity.deviceUUID.uuidString)",
            qos: .userInteractive
        )
        let format = FormatTable.placeholder(for: identity.kind)
        placeholder = Placeholder(
            width: format.width, height: format.height,
            caption: Placeholder.caption(for: identity.deviceName))
        super.init()
        device = CMIOExtensionDevice(
            localizedName: identity.deviceName,
            deviceID: identity.deviceUUID,
            legacyDeviceID: nil,
            source: self
        )
        source = StreamSource(
            streamID: identity.sourceStreamUUID, kind: identity.kind, deviceName: identity.deviceName,
            deviceSource: self)
        sink = StreamSink(
            streamID: identity.sinkStreamUUID, kind: identity.kind, deviceName: identity.deviceName,
            deviceSource: self)
        do {
            try device.addStream(source.stream)
            try device.addStream(sink.stream)
        } catch {
            fatalError("camera extension: addStream failed: \(error.localizedDescription)")
        }
    }

    // MARK: CMIOExtensionDeviceSource

    var availableProperties: Set<CMIOExtensionProperty> {
        [.deviceTransportType, .deviceModel]
    }

    func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionDeviceProperties
    {
        let props = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) {
            props.transportType = transportTypeVirtual
        }
        if properties.contains(.deviceModel) {
            props.model = "oak-cam Virtual Camera"
        }
        return props
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {}

    // MARK: Consumer (source stream) lifecycle

    func consumerStarted() {
        queue.async {
            self.consumers += 1
            if self.consumers == 1 {
                self.startOutputTimer()
            }
            os_log(.info, log: log, "%{public}@: consumer started (%u open)", self.name, self.consumers)
        }
    }

    func consumerStopped() {
        queue.async {
            if self.consumers > 0 {
                self.consumers -= 1
            }
            if self.consumers == 0 {
                self.stopOutputTimer()
                // Nobody is watching: keep no picture resident (neither the
                // app's pooled buffer nor a held copy) between viewer sessions.
                self.heldFrame = nil
                self.lastRelayedSample = nil
            }
            os_log(.info, log: log, "%{public}@: consumer stopped (%u open)", self.name, self.consumers)
        }
    }

    // MARK: Publisher (sink stream) lifecycle

    func publisherStarted(client: CMIOExtensionClient) {
        queue.async {
            self.publishing = true
            self.generation &+= 1
            let generation = self.generation
            os_log(.info, log: log, "%{public}@: publisher started (generation %llu)", self.name, generation)
            self.consumeNext(client, generation: generation)
        }
    }

    func publisherStopped() {
        queue.async {
            self.publishing = false
            // Retire any in-flight consume loop.
            self.generation &+= 1
            self.arbiter.publisherStopped(clean: true, nowNs: DeviceSource.hostTimeNs())
            os_log(.info, log: log, "%{public}@: publisher stopped (clean); holding last frame", self.name)
        }
    }

    /// Pull the next sample buffer the app enqueued. The completion fires when
    /// one is available; re-arming from inside it keeps the relay continuous
    /// without a polling timer. Each loop carries the `generation` it was started
    /// under: once a start or stop bumps the counter the stale loop exits instead
    /// of spinning against a dead client. An error with no buffer backs off
    /// briefly so a wedged sink cannot spin the queue.
    private func consumeNext(_ client: CMIOExtensionClient, generation: UInt64) {
        guard publishing, generation == self.generation else {
            return
        }
        sink.stream.consumeSampleBuffer(from: client) { [weak self] sampleBuffer, sequenceNumber, _, _, error in
            guard let self = self else {
                return
            }
            self.queue.async {
                guard generation == self.generation else {
                    return
                }
                if let sampleBuffer = sampleBuffer {
                    self.relay(sampleBuffer, sequenceNumber: sequenceNumber)
                    self.consumeNext(client, generation: generation)
                    return
                }
                if let error = error {
                    os_log(.error, log: log, "%{public}@: sink consume failed: %{public}@", self.name, error.localizedDescription)
                }
                self.queue.asyncAfter(deadline: .now() + .milliseconds(5)) {
                    self.consumeNext(client, generation: generation)
                }
            }
        }
    }

    private func relay(_ sampleBuffer: CMSampleBuffer, sequenceNumber: UInt64) {
        let now = DeviceSource.hostTimeNs()

        if let description = CMSampleBufferGetFormatDescription(sampleBuffer) {
            let dims = CMVideoFormatDescriptionGetDimensions(description)
            placeholder.resize(width: dims.width, height: dims.height)
            let subtype = CMFormatDescriptionGetMediaSubType(description)
            if let index = FormatTable.index(
                for: kind, pixelFormat: subtype, width: dims.width, height: dims.height),
                index != source.activeFormatIndex
            {
                source.activeFormatIndex = index
                let state = CMIOExtensionPropertyState<AnyObject>(value: NSNumber(value: index))
                source.stream.notifyPropertiesChanged([.streamActiveFormatIndex: state])
            }
        }

        // Retain the most recent frame (replacing the previous reference) so the
        // held state can copy it, and drop any held picture — we are live again.
        lastRelayedSample = consumers > 0 ? sampleBuffer : nil
        heldFrame = nil

        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let preferred = pts.isValid ? UInt64(max(0, CMTimeGetSeconds(pts)) * 1e9) : now
        arbiter.noteRelay(hostNs: preferred)
        logStateIfChanged()

        if consumers > 0 {
            let (hostNs, discontinuity) = arbiter.stampForSend(source: .relay, preferred: preferred)
            // Consumers read the buffer's own PTS, not only the host-time
            // argument. When the cursor had to move this frame forward (a frame
            // queued just before a held/idle tick), re-time a shallow copy (it
            // shares the image buffer) so the PTS cannot step backwards either.
            let outgoing = hostNs == preferred ? sampleBuffer : DeviceSource.retimed(sampleBuffer, hostNs: hostNs)
            source.stream.send(
                outgoing, discontinuity: discontinuity ? .time : [], hostTimeInNanoseconds: hostNs)
        }

        sink.stream.notifyScheduledOutputChanged(
            CMIOExtensionScheduledOutput(sequenceNumber: sequenceNumber, hostTimeInNanoseconds: now)
        )
    }

    // MARK: Output timer (idle / held)

    private func startOutputTimer() {
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
        timer.schedule(deadline: .now(), repeating: DeviceSource.outputInterval, leeway: .milliseconds(2))
        timer.setEventHandler { [weak self] in
            self?.outputTick()
        }
        timer.resume()
        outputTimer = timer
    }

    private func stopOutputTimer() {
        outputTimer?.cancel()
        outputTimer = nil
    }

    /// The heartbeat while a consumer is open. The relay path drives sending
    /// while frames flow; this fills the gaps with the held picture or the idle
    /// placeholder, chosen by the arbiter, and never lets time run backwards.
    private func outputTick() {
        let now = DeviceSource.hostTimeNs()
        let decision = arbiter.tick(nowNs: now, consumers: consumers)
        logStateIfChanged()
        switch decision {
        case .none, .relay:
            return
        case .held:
            sendHeld(now: now)
        case .idle:
            heldFrame = nil
            lastRelayedSample = nil
            sendIdle(now: now)
        }
    }

    private func sendHeld(now: UInt64) {
        if heldFrame == nil {
            if let sample = lastRelayedSample, let held = HeldFrame(from: sample) {
                heldFrame = held
                // Release the app's pooled buffer now that we own a copy.
                lastRelayedSample = nil
                os_log(.info, log: log, "%{public}@: held: copied last frame", self.name)
            }
        }
        guard let held = heldFrame else {
            // Nothing captured to hold (should not happen once anything relayed);
            // fall back to the idle placeholder rather than freeze.
            sendIdle(now: now)
            return
        }
        let (hostNs, discontinuity) = arbiter.stampForSend(source: .held, preferred: now)
        guard let sampleBuffer = held.sampleBuffer(hostTimeNs: hostNs) else {
            return
        }
        source.stream.send(sampleBuffer, discontinuity: discontinuity ? .time : [], hostTimeInNanoseconds: hostNs)
    }

    private func sendIdle(now: UInt64) {
        let (hostNs, discontinuity) = arbiter.stampForSend(source: .idle, preferred: now)
        guard let sampleBuffer = placeholder.sampleBuffer(hostTimeNs: hostNs) else {
            return
        }
        source.stream.send(sampleBuffer, discontinuity: discontinuity ? .time : [], hostTimeInNanoseconds: hostNs)
    }

    private func logStateIfChanged() {
        if loggedState != arbiter.state {
            loggedState = arbiter.state
            os_log(.info, log: log, "%{public}@: output source -> %{public}@", self.name, String(describing: arbiter.state))
        }
    }

    /// `sampleBuffer` with its presentation time replaced by `hostNs`; the
    /// original when the copy cannot be made.
    private static func retimed(_ sampleBuffer: CMSampleBuffer, hostNs: UInt64) -> CMSampleBuffer {
        var timing = CMSampleTimingInfo(
            duration: CMSampleBufferGetDuration(sampleBuffer),
            presentationTimeStamp: CMTime(value: CMTimeValue(hostNs), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid
        )
        var copy: CMSampleBuffer?
        let status = CMSampleBufferCreateCopyWithNewTiming(
            allocator: kCFAllocatorDefault,
            sampleBuffer: sampleBuffer,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleBufferOut: &copy
        )
        guard status == noErr, let copy = copy else {
            os_log(.error, log: log, "CMSampleBufferCreateCopyWithNewTiming failed: %d", status)
            return sampleBuffer
        }
        return copy
    }

    private static func hostTimeNs() -> UInt64 {
        let time = CMClockGetTime(CMClockGetHostTimeClock())
        return UInt64(max(0, CMTimeGetSeconds(time)) * 1e9)
    }
}
