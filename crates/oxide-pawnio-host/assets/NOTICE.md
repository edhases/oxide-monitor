# Third-party notices: PawnIO module

`AMDFamily17.bin` is an unmodified binary module from the official PawnIO.Modules
project:

- Project: PawnIO.Modules
- Release: 0.2.3 (2026-02-25)
- Source: https://github.com/namazso/PawnIO.Modules/tree/0.2.3
- Module source: https://github.com/namazso/PawnIO.Modules/blob/0.2.3/AMDFamily17.p
- License: GNU Lesser General Public License v2.1 or later (LGPL-2.1-or-later)
- License text: `COPYING-PawnIO-Modules.txt` in this directory
- SHA-256: `099dc01d6db97ea997fec4a461e191cc64b9d7ce47c9d2153c451c56c2adcf50`

The module is embedded unmodified in `oxide-pawnio-host.exe` and loaded through the
official PawnIO driver's public `pawnio_load` interface at runtime.

The PawnIO driver and installer are **not** part of Oxide Monitor and are not
redistributed. Users install/update them from https://pawnio.eu.
