# Future features

Ideas that are not scheduled yet.

- `--list` (and the GUI device combo) should also show devices that another
  process has booted, marked "in use", instead of hiding them. Today the shim
  uses `getAllAvailableDevices()`, which drops BOOTED devices, so a camera held
  by OAK Viewer simply looks absent (seen 2026-09-25). `getAllConnectedDevices()`
  plus the state column would make this obvious.
- After the camera extension is first approved, apps that were already open
  (Photo Booth at least) show a dead preview for the new cameras until they are
  restarted. The GUI's install flow should say so on success.
- Warn in the GUI when the requested combination exceeds what the RVC2 can
  sustain, e.g. mono pair at 1280x800@60 with RGB 1080p30 on (measured: ~41 fps,
  p50 175 ms, steady drops). 720p60 holds but with ~100 ms latency.
- PoE discovery across subnets: broadcast discovery (UDP 11491 on every
  interface) cannot see a camera behind a router. Rescan should also send a
  unicast discovery to the recent IPs and to the 169.254.1.222 fallback, so a
  camera on another subnet or a direct cable is listed without typing its IP.
- `oak-cam poe-config <ip> [show | dynamic <fallback ip> <mask> <gw> <ms> |
  static ... | dhcp]`: read and flash the PoE bootloader network config from
  the app. Found 2026-09-27: a camera shipped with DHCP, a 30 s timeout and
  fallback 10.0.0.15/24 never came up on a direct cable, because the booted
  firmware's watchdog resets it ~15 s after link-up, before the fallback.
  Flashing DHCP + fallback 169.254.1.222/16 after 2 s fixed it (done with a
  throwaway tool at ~/.cache/oak-cam/tools/blcfg, source alongside). The GUI
  could detect the repeated BOOTLOADER-boot-vanish cycle and suggest this.
