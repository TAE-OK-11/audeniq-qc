# Third-party provenance

## FFmpeg-derived modules (LGPL-2.1-or-later)

Pinned FFmpeg commit: `12c589a37d093cc55618f8377b092dc21416806f`.

* `src/tta.rs`: `libavcodec/tta.c`, `ttadata.c`, `ttadsp.c`; Alex Beregszaszi (2006), FFmpeg contributors.
* `src/wavpack.rs`, `src/wavpack_table.rs`: `libavcodec/wavpack.c`, `wavpack.h`, `wavpackdata.c`; Konstantin Shishkov (2006, 2011), David Bryant (2020). Only integer lossless paths.
* `src/flac.rs`: FLAC fixed/LPC residual prediction, Rice mapping, frame/subframe structure from `libavcodec/flacenc.c` and Welch/Levinson approach from `lpc.c`; Justin Ruggles (2006). Reworked selection, buffers, verification and commit logic.
* `src/meter.rs`: K-weighting coefficient equations from `libavfilter/ebur128.c`; Jan Kokemüller (2011). That source also includes the libebur128 MIT notice below.

Rust translations change storage, bounds handling, error propagation, work limits and dispatch; they do not remove upstream attribution. No FFmpeg binary, libav* library, GPL-only filter, assembler file, or C FFI is linked. Full LGPL text: LICENSE; referenced GPL text: COPYING.GPLv2.

## Rust dependencies

* Symphonia (MPL-2.0): FLAC/ALAC decoders and FLAC/M4A demuxers only; default features disabled. These decoders are existing Rust implementations, not claimed as new FFmpeg ports.
* image (MIT): JPEG/PNG decoding only; default features disabled.
* serde/serde_json (MIT OR Apache-2.0): typed reports.
* RustCrypto sha2/md-5 (MIT OR Apache-2.0): canonical SHA-256 and required FLAC PCM MD5. SHA-256 uses runtime CPU feature dispatch from RustCrypto; MD5 is a format integrity field, not a security authenticator.

Cargo.lock pins transitive dependencies. Dependency sources/license notices remain available through crates.io. Linking into other applications requires complying with each applicable license; no claim is made that optimization removes copyleft obligations.

## libebur128 MIT notice

Copyright (c) 2011 Jan Kokemüller

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
