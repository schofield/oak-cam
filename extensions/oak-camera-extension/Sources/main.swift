// Entry point of the oak-cam CoreMediaIO camera extension.
//
// The extension is a sandboxed system-extension process macOS launches on
// demand. It exposes one camera device per OAK sensor ("OAK RGB", "OAK Left",
// "OAK Right"), each with a source stream (what Photo Booth, Chrome, Zoom see)
// and a sink stream the oak-cam app writes frames into through the CoreMediaIO
// client API. The device and stream UUIDs live in Info.plist (`OakCameras`) so
// the app (crates/vcam/src/macos) and this process agree on them.

import CoreMediaIO
import Foundation

let identities = ExtensionIdentity.fromMainBundle()
let providerSource = ProviderSource(identities: identities)
CMIOExtensionProvider.startService(provider: providerSource.provider)
CFRunLoopRun()
