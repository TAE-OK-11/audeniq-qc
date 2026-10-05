# Third-party provenance

## FFmpeg-derived modules (LGPL-2.1-or-later)

Pinned FFmpeg commit: `12c589a37d093cc55618f8377b092dc21416806f`.

* `src/tta.rs`: `libavcodec/tta.c`, `ttadata.c`, `ttadsp.c`; Alex Beregszaszi (2006), FFmpeg contributors.
* `src/wavpack.rs`, `src/wavpack_table.rs`: `libavcodec/wavpack.c`, `wavpack.h`, `wavpackdata.c`; Konstantin Shishkov (2006, 2011), David Bryant (2020). Only integer lossless paths.
* `src/flac.rs`: FLAC fixed/LPC residual prediction, Rice mapping, frame/subframe structure from `libavcodec/flacenc.c` and Welch/Levinson approach from `lpc.c`; Justin Ruggles (2006). Reworked selection, buffers, verification and commit logic.
* `src/alac.rs`: Rice/zero-run, adaptive LPC and stereo reconstruction from `libavcodec/alac.c`, `alacdsp.c`; David Hammerton (2005). Reworked bounded bit access, in-place reusable buffers and specialized predictor orders.
* `src/flac_decode.rs`: frame/header, partitioned Rice, fixed/LPC and channel reconstruction from `libavcodec/flac.c`, `flacdec.c`, `flacdsp.c`; Alex Beregszaszi (2003), Mans Rullgard (2012). Reworked streaming input, strict CRC/count/MD5 checks and specialized LPC orders.
* `src/meter.rs`: K-weighting coefficient equations from `libavfilter/ebur128.c`; Jan Kokemüller (2011). That source also includes the libebur128 MIT notice below.

Rust translations change storage, bounds handling, error propagation, work limits and dispatch; they do not remove upstream attribution. No FFmpeg binary, libav* library, GPL-only filter, or native media-codec FFI is linked. Media processing is Rust, including CPU intrinsics; normal Rust system-runtime libraries still apply. Full LGPL text: LICENSE; referenced GPL text: COPYING.GPLv2.

## Rust dependencies

* Symphonia (MPL-2.0): **optional `reference-codecs` comparison build only**, FLAC/ALAC decoding and FLAC/M4A demuxing. Default builds do not link it. Reference implementations are existing Rust code, not claimed as our ports.
* JPEG/PNG cover validation is implemented in this repository (`src/jpeg.rs`, `src/png.rs`, `src/inflate.rs`). It accepts and rejects the same files as image 0.24.9 (MIT, with jpeg-decoder 0.3.2 and png 0.17.16), which remains only as a dev-dependency test oracle.
* serde/serde_json (MIT OR Apache-2.0): typed reports.
* SHA-256 (canonical PCM hash; x86 SHA extensions, ARMv8 SHA2 or portable) and the required FLAC PCM MD5 (a format integrity field, not a security authenticator) are implemented in this repository (`src/sha256.rs`, FIPS 180-4; `src/md5.rs`, RFC 1321). RustCrypto sha2 and md-5 (MIT OR Apache-2.0) remain only as dev-dependency test oracles; the development tools crate uses the in-repository SHA-256, with FFmpeg as its independent hash oracle.
* IEEE CRC-32 (TTA header/frame checks and the frame-copy round-trip check) is implemented in this repository (`src/crc32.rs`; carry-less-multiply folding after Gopal et al., Intel 2009, with AVX-512/AVX2 VPCLMULQDQ, PCLMULQDQ, AArch64 PMULL and slicing-by-8 paths). crc32fast (MIT OR Apache-2.0) remains a dev-dependency test oracle and a transitive dependency of the PNG stack.

Cargo.lock pins transitive dependencies. Dependency sources/license notices remain available through crates.io. Linking into other applications requires complying with each applicable license; no claim is made that optimization removes copyleft obligations.

`src/m4a.rs` and `src/msb.rs` are AUDENIQ-specific Rust implementations of the container tables and cached bounded bit reader, rather than copied Symphonia source. General JSON libraries remain in both builds; "native media codecs" does not mean zero third-party dependencies.

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
