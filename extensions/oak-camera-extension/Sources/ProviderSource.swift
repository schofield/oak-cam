import CoreMediaIO
import Foundation

/// The provider: one device per `OakCameras` entry, created up front so the
/// cameras exist as soon as the extension process is running (macOS launches it
/// when a client asks for the device list). Each device has its own queue,
/// arbiter, held frame and placeholder, so one camera stalling never affects
/// the others.
final class ProviderSource: NSObject, CMIOExtensionProviderSource {
    private(set) var provider: CMIOExtensionProvider!
    private var deviceSources: [DeviceSource] = []

    init(identities: [ExtensionIdentity]) {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: nil)
        for identity in identities {
            let deviceSource = DeviceSource(identity: identity)
            do {
                try provider.addDevice(deviceSource.device)
            } catch {
                fatalError(
                    "camera extension: addDevice \(identity.deviceName) failed: \(error.localizedDescription)")
            }
            deviceSources.append(deviceSource)
        }
    }

    func connect(to client: CMIOExtensionClient) throws {}

    func disconnect(from client: CMIOExtensionClient) {}

    var availableProperties: Set<CMIOExtensionProperty> {
        [.providerName, .providerManufacturer]
    }

    func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionProviderProperties
    {
        let props = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerName) {
            props.name = "oak-cam Camera Extension"
        }
        if properties.contains(.providerManufacturer) {
            props.manufacturer = "surlysoft"
        }
        return props
    }

    func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}
}
