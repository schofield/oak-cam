import Foundation

/// Which picture the source stream should carry right now.
enum OutputSource: Equatable {
    /// No consumer is open, so nothing is sent.
    case none
    /// A publisher is relaying; the relay path pushes the app's frame.
    case relay
    /// Hold the last relayed picture (with a "Reconnecting…" badge) at the
    /// nominal rate until a publisher relays again or the hold times out.
    case held
    /// The idle placeholder (grey with luma bars): never published, or the hold
    /// timed out.
    case idle
}

/// The pure decision core for the virtual camera's output, free of CoreMediaIO
/// so it can be exercised on the host (`scripts/vcam/build-macos.sh --check`).
/// `DeviceSource` is the thin CMIO glue around it.
///
/// It owns two things the "stays grey after reconnect" bug turned on:
///
/// 1. **The monotonic host-time cursor.** Every frame the device sends is
///    stamped through [`stampForSend`], which never lets the host time move
///    backwards. The old code stamped the placeholder with `now` and relayed
///    frames with the app's (older) PTS, so the first relayed frame after a gap
///    could carry a host time *earlier* than the last placeholder — and an
///    AVFoundation consumer stalls or greys out on a backwards timestamp.
/// 2. **The output state machine** (idle → relaying → held), so a stall or a
///    publisher stop holds the last picture instead of dropping to grey, and a
///    source switch is flagged as a discontinuity.
final class FrameArbiter {
    /// A relayed frame is stale after this long without another (today's
    /// one-second stall window).
    static let stallNanoseconds: UInt64 = 1_000_000_000
    /// After a CLEAN publisher stop the held picture is shown for this long,
    /// then the idle placeholder takes over. An unclean loss (no `stopStream`:
    /// an app crash detected as a stall) is held indefinitely while a consumer
    /// is open.
    static let holdTimeoutNanoseconds: UInt64 = 60_000_000_000

    /// The output state. `idle` before anything is published or after a hold
    /// times out; `relaying` while fresh app frames arrive; `held` while the
    /// last picture is repeated.
    enum State: Equatable { case idle, relaying, held }
    private(set) var state: State = .idle

    private var everRelayed = false
    private var lastRelayNs: UInt64 = 0
    private var lastSentNs: UInt64 = 0
    /// Host time after which a clean-stop hold falls back to idle; 0 means no
    /// deadline (an unclean loss, held while a consumer is open).
    private var holdDeadlineNs: UInt64 = 0
    /// The source of the last frame actually sent, so a switch can be flagged as
    /// a discontinuity on the next send.
    private var lastSentSource: OutputSource = .none

    // MARK: Sending (monotonic clock + discontinuity)

    /// The host time and discontinuity flag for a frame about to be sent from
    /// `source`, presented at `preferred` (its natural host time). The returned
    /// time is clamped so it strictly increases across every source, and the
    /// discontinuity flag is set the first time the source changes.
    func stampForSend(source: OutputSource, preferred: UInt64) -> (hostTimeNs: UInt64, discontinuity: Bool) {
        let discontinuity = source != lastSentSource
        lastSentSource = source
        let stamp = max(preferred, lastSentNs &+ 1)
        lastSentNs = stamp
        return (stamp, discontinuity)
    }

    // MARK: Events

    /// A relayed frame is available (its PTS is `hostNs`). Moves to the relaying
    /// state; the actual send is stamped through [`stampForSend`].
    func noteRelay(hostNs: UInt64) {
        everRelayed = true
        lastRelayNs = hostNs
        holdDeadlineNs = 0
        state = .relaying
    }

    /// The publisher stopped. `clean` is a real `stopStream` (versus a dropped
    /// client or crash we later infer from a stall). With a picture to hold, a
    /// clean stop arms the 60-second hold timeout; an unclean loss is held
    /// indefinitely. Before anything was ever relayed there is nothing to hold,
    /// so it falls straight back to idle.
    func publisherStopped(clean: Bool, nowNs: UInt64) {
        guard everRelayed else {
            state = .idle
            return
        }
        state = .held
        holdDeadlineNs = clean ? nowNs &+ FrameArbiter.holdTimeoutNanoseconds : 0
    }

    // MARK: Tick

    /// Decide what to send on a timer tick with `consumers` consumers open.
    /// Advances the state machine: a relaying stream that has gone stale becomes
    /// an (indefinite) hold, and a hold past its clean-stop deadline becomes
    /// idle.
    func tick(nowNs: UInt64, consumers: UInt32) -> OutputSource {
        if consumers == 0 {
            return .none
        }
        if !everRelayed {
            state = .idle
            return .idle
        }
        // A relaying stream with a fresh frame keeps relaying; the relay path
        // (not the tick) does the sending.
        if state == .relaying {
            if nowNs &- lastRelayNs < FrameArbiter.stallNanoseconds {
                return .relay
            }
            // Stalled with no stopStream: an unclean loss. Hold indefinitely.
            state = .held
            holdDeadlineNs = 0
        }
        if holdDeadlineNs != 0 && nowNs >= holdDeadlineNs {
            state = .idle
            return .idle
        }
        return .held
    }
}
