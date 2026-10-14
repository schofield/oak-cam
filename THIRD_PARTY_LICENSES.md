# Third-party notices

## depthai-core (Luxonis) — MIT

oak-cam links dynamically against `libdepthai-core` (https://github.com/luxonis/depthai-core),
pinned to tag `v3.10.0`, built by `scripts/depthai/build.sh` (macOS, Linux) or fetched from
Luxonis's prebuilt Windows release. depthai-core is MIT licensed; its own third-party
notices ship with the library.

## libusb (Luxonis fork) — LGPL-2.1-or-later

On Windows, depthai-core loads `libusb-1.0.dll` for USB devices. It is built from Luxonis's
libusb fork (https://github.com/luxonis/libusb), pinned to commit
`745c272a2f0305c4ab162b7fb1b2edc7578c6b77` (libusb 1.0.27, the commit depthai-core
`v3.10.0` builds against), by `scripts/depthai/fetch-windows.ps1`, and ships unmodified as
`libusb-1.0.dll` in the Windows package. oak-cam links to it only dynamically, so it can be
replaced with any compatible build. libusb is licensed under the GNU Lesser General Public
License, version 2.1 or later; the full text (`COPYING`) and the source are in the
repository above at that commit. `fetch-windows.ps1` installs the license text as
`share\licenses\libusb\COPYING` in the depthai-core install prefix, and the Windows package
ships it as `licenses\libusb\COPYING`.

## kornia/depthai-rs — Apache-2.0

The C shim in `crates/depthai-sys/csrc/` follows the design of the C interface in
https://github.com/kornia/depthai-rs (`csrc/depthai_c.h`), Apache License 2.0.
No code is copied verbatim; the error-handling and opaque-handle conventions are derived
from it.
