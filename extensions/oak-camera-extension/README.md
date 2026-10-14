# oak-camera-extension

The macOS virtual cameras: a CoreMediaIO camera extension, written in Swift,
that `oak-cam.app` embeds at
`Contents/Library/SystemExtensions/com.surlysoft.oak-cam.camera-extension.systemextension`.
It exposes three devices, **OAK RGB**, **OAK Left** and **OAK Right**, each with
a source stream (what Photo Booth, FaceTime, Chrome, Zoom open) and a sink
stream the oak-cam app writes into. Ported from stream-concern's
`extensions/sc-camera-extension` (MIT, same author) and made multi-device.

| File | Role |
|---|---|
| `Sources/main.swift` | Starts the provider service and runs the run loop. |
| `Sources/ExtensionIdentity.swift` | Reads the `OakCameras` array (name, kind, device/source/sink UUIDs) from `Info.plist`. |
| `Sources/ProviderSource.swift` | `CMIOExtensionProviderSource`: one device per `OakCameras` entry. |
| `Sources/DeviceSource.swift` | One camera: its own queue, arbiter, held frame and placeholder; relays sink frames to the source stream, sends the placeholder while nothing is published, holds the last frame with a "Reconnecting…" badge when the app stalls. |
| `Sources/StreamSource.swift` | The source stream consumers read. |
| `Sources/StreamSink.swift` | The sink stream the app writes (`CMIOStreamCopyBufferQueue` + `CMSimpleQueueEnqueue`). |
| `Sources/FormatTable.swift` | Advertised formats per kind: RGB NV12 1920x1080 (default), 1280x720, 3840x2160 + YUY2/UYVY/BGRA 1080p; mono NV12 1280x800 (default), 1280x720, 640x400 + UYVY 1280x800. |
| `Sources/Placeholder.swift` | The NV12 idle card (grey, luma steps, "OAK RGB — waiting for oak-cam"). |
| `Sources/FrameArbiter.swift`, `HeldFrame.swift`, `Badge.swift` | Relay/held/idle state machine, the held-picture copy, the CoreText caption. |
| `Tests/main.swift` | Host test (no CMIO, no signing): run by `scripts/vcam/build-macos.sh --test`/`--check`. |
| `Info.plist.in` | Bundle template: `SYSX`, `CMIOExtension` mach service, the `OakCameras` table. |
| `entitlements.plist.in` | App Sandbox + team app group `TEAMID.com.surlysoft.oak-cam`. |

The Rust side is `crates/vcam/src/macos/`; `crates/vcam/src/camera_id.rs`
(`CAMERAS`) pins the same nine UUIDs and `camera_table_matches_extension_plist`
checks they agree.

## Identifiers

| What | Value |
|---|---|
| App bundle id | `com.surlysoft.oak-cam` |
| Extension bundle id | `com.surlysoft.oak-cam.camera-extension` |
| Mach service | `<TEAMID>.com.surlysoft.oak-cam.camera-extension` |
| App group (extension and app) | `<TEAMID>.com.surlysoft.oak-cam` |
| Logs | `log stream --predicate 'subsystem BEGINSWITH "com.surlysoft.oak-cam"'` |

## Build, sign, install

```sh
scripts/vcam/build-macos.sh --check        # build + host test + sign dist/oak-cam.app, no install
scripts/vcam/build-macos.sh \
  --provisionprofile OakCam_Developer_ID.provisionprofile \
  --notarize <notarytool-keychain-profile>  # build, sign, notarise, install /Applications/oak-cam.app
/Applications/oak-cam.app/Contents/MacOS/oak-cam vcam install
# approve under System Settings › General › Login Items & Extensions › Camera Extensions
oak-cam vcam status
oak-cam vcam test-pattern --camera all
```

Rules carried over from stream-concern (all measured there):

- **The install entitlement needs a profile.** Activating needs the app to
  carry `com.apple.developer.system-extension.install`, a restricted
  entitlement: AMFI kills an ad-hoc app carrying it ("Adhoc signed app with
  restricted entitlements detected") and a Developer ID app without a profile
  ("Unsatisfied entitlements"). The script adds it, plus the app group, only
  with `--provisionprofile`; without one the app builds but
  `vcam install` fails with *Missing entitlement* (OSSystemExtensionErrorDomain 2).
- **Notarisation is mandatory for activation**, not only distribution: a
  Developer ID extension in an un-notarised app is refused
  (OSSystemExtensionErrorDomain 8).
- **Run from /Applications** (error 3 otherwise) unless
  `systemextensionsctl developer on`, which, like ad-hoc activation, needs SIP off.
- **Bump CFBundleVersion** for every rebuilt extension (default
  `<major>.<minor>.<HEAD commit time in seconds>`, which only grows; pass
  `--build` for uncommitted rebuilds). Changing the device list may also need a reboot before macOS shows
  the new devices.
- Grant Camera access to whatever runs `oak-cam` (Terminal, or the app) if
  `CMIOStreamCopyBufferQueue` returns no queue.

## One-time Apple Developer setup (not done yet for oak-cam)

At developer.apple.com, team `5V2V8BX632`:

1. *Identifiers*: register the explicit App ID `com.surlysoft.oak-cam` with the
   **System Extension** and **App Groups** capabilities (optionally
   `com.surlysoft.oak-cam.camera-extension` with App Groups for an extension
   profile). The team-prefixed group needs no registration.
2. *Profiles*: a **Developer ID** profile for `com.surlysoft.oak-cam` and the
   Developer ID Application certificate (and/or a macOS App Development profile
   for local testing). Download it as `.provisionprofile`.
3. A `notarytool` keychain profile (`xcrun notarytool store-credentials`).
