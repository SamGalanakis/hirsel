# Third-party notices

Hirsel adapts code and test vectors from the sources below. Each is used under
the license named in its row; the license texts follow once each.

This is the adapted-source inventory. Every path a row cites under "Where in
this tree" or "License" exists: `//:gate-distribution-notices` is red on one
that does not, and on a row that cites none. Release packaging separately
derives the linked dependency inventory in the root `NOTICES.md` and retains
upstream license texts under the bundle's `notices/` directory; see
`tools/toolchain-dist/README.md`.

| Source | Adapted | Where in this tree | Upstream revision | License |
|---|---|---|---|---|
| Rust standard library | `library/core/src/slice/sort/stable/{mod,drift,merge,quicksort}.rs`, `shared/{pivot,smallsort,mod}.rs` stable driftsort structure; `library/alloctests/tests/sort/tests.rs:18-22,450-565,599-638` stability, pattern and failure vectors | `docs/prior-art-sort.md` | rust-lang/rust `f6c1107f8be13e48465e12c0fd26e6a39fe18e4f` | MIT (Rust) |
| Rust standard library | Ordered-run detection and predecessor-pivot partitioning in `library/core/src/slice/sort/{shared/mod,unstable/quicksort}.rs` | `core/std/list.hirsel` | Rust 1.99.0 | MIT (Rust) |
| memchr | Forward Two-Way critical factorization and shifts in `src/arch/all/twoway.rs`; four-vector unrolled byte scan and overlapping tail in `src/arch/generic/memchr.rs:143-230` | `core/std/string.hirsel`, `core/std/list.hirsel` | memchr 2.8.3 | MIT, `third-party/LICENSE.byte-search` |
| base64-simd | Intra-lane 24-byte shuffle layout (`SPLIT_SHUFFLE` at `src - 4`) in `src/encode.rs:128-268` | `core/core/string.hirsel` | base64-simd 0.8.0 | MIT |
| simdutf8 | Keiser-Lemire SIMD UTF-8 lookup tables and continuation check in `src/implementation/algorithm.rs:69-170` | `core/core/string.hirsel` | simdutf8 0.1.5 | MIT |
| rustc_target | `compiler/rustc_target/src/callconv/{x86_64,aarch64}.rs` C return classification: SysV eightbyte class merging, AAPCS64 homogeneous floating aggregates and integer carriers, and the indirect return otherwise; Hirsel classifies only FORM's naturally aligned scalar fields | `crates/hirsel-llvm/src/core/abi.rs` | rust-lang/rust, revision not recorded when adapted (the pinned toolchain is 1.99.0 `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`) | MIT (Rust); upstream also Apache-2.0 |
| Goose | `src/format.h` and its formatter: the file's own tokens and comments printed in order with the layout between them chosen afresh, token roles taken from the parser, and the result checked against the input's tokens before use; Hirsel combines it with its doc/trivia printer | `crates/hirsel-core-syntax/src/format.rs` | aardappel/goose `d5ae5bbda990923fa4129f969c7f853c20d4d1e6` | Apache-2.0 (text below, without the LLVM exception) |
| Goose | `stdlib/csv.goose:19-57,60-71` (each_record and format_field) | `core/std/csv.hirsel` | aardappel/goose `d5ae5bbda990923fa4129f969c7f853c20d4d1e6` | Apache-2.0 (text below, without the LLVM exception) |
| Go standard library | `src/encoding/csv/reader.go:297-460` (strict field scanning), `src/encoding/csv/reader_test.go:40-405` (readTests), `src/encoding/csv/writer_test.go:14-50` (writeTests) | `core/std/csv.hirsel`, `core/std/tests/csv.hirsel` | golang/go `46b62495e1bf4d56e0313320772ae588d9119aed` | BSD-3-Clause (Go) |
| commonmark.js | Container matching, inline delimiter and bracket processing | `core/std/markdown.hirsel` | commonmark/commonmark.js `898886961b49c1df1175666d03c3ac66c9cc9040` | BSD-2-Clause (CommonMark) |
| cmark-gfm | Generated HTML entity names and values | `core/std/markdown.hirsel` | github/cmark-gfm `27d942c8b0a62d192f616e5bf3578f4b6a89e180` | BSD-2-Clause (CommonMark) |
| CommonMark specification | All 0.31.2 Markdown and expected HTML examples | `core/std/tests/markdown.hirsel` | commonmark/commonmark-spec `9103e341a973013013bb1a80e13567007c5cef6f` | CC-BY-SA-4.0 (CommonMark examples) |
| Go standard library | Reflected IEEE slicing-by-8 construction and recurrence in `src/hash/crc32/crc32_generic.go:60-90` | `core/std/crc32.hirsel` | golang/go `ebe2feefc400f813aefdc8a036e66d2619da6649` | BSD-3-Clause (Go) |
| OpenTelemetry Protocol | OTLP JSON examples and protoc descriptor field/enum projection | `tools/tests/fixtures/obs-export/{logs,metrics,trace,proto-json-schema}.json`, `tools/obs_export_schema.py` | open-telemetry/opentelemetry-proto `09769935deb8b2ebcf73ad1e2142b691d17bf70b` | Apache-2.0 (text below, without the LLVM exception; also `tools/tests/fixtures/obs-export/LICENSE`) |
| Go standard library and vendored cryptobyte | `src/crypto/x509/parser.go:38-154,209-247,465-534,1395-1452`, `pkix/pkix.go:28-104`, `src/net/url/url.go:441-649`, vendored `golang.org/x/crypto/cryptobyte/asn1.go:755-835`; TestPKIXNameString (14 rows + input-preservation assertion), TestRDNSequenceString (1), TestParseASN1String (15), TestDomainNameValid (25) | `docs/x509-prior-art.md` | golang/go `ebe2feefc400f813aefdc8a036e66d2619da6649` | BSD-3-Clause (Go) |
| Go UUID package | `src/uuid/uuid_test.go:130-206`, TestParseSuccess and TestParseErrors inputs | `core/std/tests/uuid.hirsel` | golang/go `46b62495e1bf4d56e0313320772ae588d9119aed` | BSD-3-Clause (Go) |
| Go standard library | `src/time/format.go:1556-1615,1634-1731`, `src/time/time.go:947-1067` duration parsing and formatting; `src/time/format_rfc3339.go:82-153` timestamp grammar; `src/time/format_test.go:77-81,274,283,652-674` RFC 3339 vectors; `src/time/time_test.go:614-641,945-1005,1016-1043` duration vectors | `core/std/time.hirsel`, `core/std/tests/time.hirsel` | golang/go `46b62495e1bf4d56e0313320772ae588d9119aed` | BSD-3-Clause (Go) |
| jiff | `src/fmt/temporal/parser.rs:813-827` leap-second clamp and `src/fmt/temporal/parser.rs:1792-1825,2260-2271` timestamp vectors | `core/std/time.hirsel`, `core/std/tests/time.hirsel` | jiff 0.2.43 | MIT; upstream also Unlicense |
| Howard Hinnant | `days_from_civil` and `civil_from_days`, Chrono-Compatible Low-Level Date Algorithms | `core/std/time.hirsel` | [Published reference](https://howardhinnant.github.io/date_algorithms.html#days_from_civil) | Public domain |
| Go standard library | `src/path/path_test.go:100-106,152-158,168-181,191-205` Split, Ext, Base and Dir vectors restricted to valid paths | `core/std/tests/path.hirsel` | golang/go `46b62495e1bf4d56e0313320772ae588d9119aed` | BSD-3-Clause (Go) |
| Rust standard library | `library/std/src/path.rs:334-354,2880-2975` last-dot rule for valid names and stem, prefix and extension examples | `core/std/path.hirsel`, `core/std/tests/path.hirsel` | rust-lang/rust `5f4effd579500175af844241eb179e756190f1e7` | MIT (Rust) |
| fastcdc | v2016 normalization masks and gear table in `src/v2016/mod.rs` | `core/std/cdc.hirsel` | nlfiedler/fastcdc-rs `b025a70229a13d9482c943d27e494c40e84e0892` | MIT, `core/std/api/cdc-LICENSE-MIT.txt` |
| RustCrypto SHA-1 | SHA instruction round and schedule intrinsics in `sha1/src/compress/{x86_sha,aarch64_sha2}.rs` | `crates/hirsel-core-rt/src/sha1.rs` | RustCrypto/hashes, sha1 0.11.0, `2f00175af936de46b3ddefe65c4de93cb4e876e4` | MIT (RustCrypto) |
| twox-hash | AVX2 XXH3 multiply and adjacent-lane addition in `src/xxhash3/large/avx2.rs`. Hirsel keeps two accumulator vectors live across multiple stripes. | `crates/hirsel-core-rt/src/xxh3.rs` | shepmaster/twox-hash 2.1.5, `0222275b9e2faaa0a09eead8057753609ff891d1` | MIT (twox-hash) |
| xxhash-rust | Scalar XXH3 recurrence and default secret in `src/const_xxh3.rs:40-254`, `src/xxh3_common.rs:13-28,34-38,56-59` | `core/std/check.hirsel`, `core/programs/store/xxh3_bench.hirsel` | DoumanAsh/xxhash-rust 0.8.15, `7026cd705195f502283f97aafc9ea41930099c68` | BSL-1.0 |
| crc32c | Portable reflected CRC-32C byte recurrence in `src/sw.rs:35-40`; generated 256-entry table, with no slicing or hardware dispatch | `core/programs/store/crc_bench.hirsel` | zowens/crc32c 0.6.8, `254a861f7cc71bfe455e86f0e1d86e3c83c33390` | MIT; upstream also Apache-2.0 |
| Ryū (Ulf Adams) | Binary32 and binary64 interval scaling and digit removal from `src/f2s.rs`, `src/f2s_intrinsics.rs`, `src/d2s.rs`, and `src/d2s_intrinsics.rs`, and halfway vectors from `tests/f2s_test.rs` | `core/std/float.hirsel`, `core/std/tests/float.hirsel` | dtolnay/ryu 1.0.23, `f0b52bb194befe6fd242154f2182fafd43a819b8` | Apache-2.0 (text below, without the LLVM exception) |
| Cranelift | code generator dependency (`cranelift-codegen`, `-frontend`, `-module`, `-object`) | `crates/hirsel-cranelift/Cargo.toml` | bytecodealliance/wasmtime cranelift 0.136.3 crate releases | Apache-2.0 WITH LLVM-exception |
| Unicode Character Database 16.0.0 | data files and generated casing and property tables | `bench/apps/tasks/rust/src/unicode16.rs` | Unicode 16.0.0 | Unicode, `bench/apps/tasks/rust/UNICODE-LICENSE.txt` |
| The Computer Language Benchmarks Game | binarytrees C gcc #2 and #3 | `bench/native/c/binarytrees-arena.h` | benchmarksgame-team.pages.debian.net | BSD-3-Clause |
| Valgrind | `include/valgrind.h`, `memcheck/memcheck.h`, vendored unchanged | `third-party/valgrind/{valgrind,memcheck}.h` | Valgrind 3.22.0 (Ubuntu `1:3.22.0-0ubuntu3`) | Valgrind header licence (BSD-style, these two files only) |
| Zig compiler | `src/libs/glibc.zig` stub shared objects: `.symver` placeholder assembly per versioned symbol, linked as a DSO with the library's SONAME and a version script; `lib/libc/glibc/abilists` read at GLIBC_2.28 to cross-check the stubs | `tools/buck2/native-toolchain/link_kit.py` (the link kit) | ziglang/zig `bff490a23889750de7096d9057c0a432efc929a8` | MIT |
| Zig std.Uri | `lib/std/Uri.zig:23-141,180-310,352-489` raw components, parse, write and resolve structure; `466-471,541-866` test groups with strict RFC 3986 deviations documented in the URI contract and tests | `core/std/uri.hirsel`, `core/std/tests/uri.hirsel` | ziglang/zig `bff490a23889750de7096d9057c0a432efc929a8` | MIT |
| Go net/url | `src/net/url/url_test.go:30-665,1521-1627` strict inputs from `urltests` and the `parseTests` query table with literal plus and semicolon | `core/std/tests/uri.hirsel` | golang/go `46b62495e1bf4d56e0313320772ae588d9119aed` | BSD-3-Clause (Go) |
| LLVM | `compiler-rt/lib/builtins/crt{begin,end}.c` and `libunwind/` compiled into the link kit and linked into every executable | link kit (`tools/buck2/native-toolchain/link_kit.py`) | llvm/llvm-project `a738f2737189bcc1de75c25e8cd1d4b466c9526e` | Apache-2.0 WITH LLVM-exception (license text below) |
| GNU C Library | glibc 2.28's `Scrt1.o`, `crti.o`, `crtn.o` and `libc_nonshared.a` (Debian 2.28-10+deb10u1) linked into every executable; its libraries' public dynamic symbol tables generate the link kit's stubs | link kit (`tools/buck2/native-toolchain/link_kit.py`) | glibc 2.28 (Debian 2.28-10+deb10u1) | LGPL-2.1-or-later, with glibc's exception granting unlimited permission to link these compiled startup and nonshared files into other programs; `tools/toolchain-dist/licenses/common-licenses/LGPL-2.1` |
| Go standard library | Block decoding and Huffman completeness in `src/compress/flate/inflate.go`, hash-chain match search in `src/compress/flate/deflate.go`, framing in `src/compress/gzip/gunzip.go` and `src/compress/zlib/reader.go` | `core/std/flate.hirsel`, `core/std/tests/flate.hirsel` | golang/go `ebe2feefc400f813aefdc8a036e66d2619da6649` | BSD-3-Clause (Go) |
| zlib | Canonical Huffman decoding and construction in `contrib/puff/puff.c:235-255,340-379`. Copyright (C) 2002-2013 Mark Adler, all rights reserved. | `core/std/flate.hirsel` | madler/zlib `09155eaa2f9270dc4ed1fa13e2b4b2613e6e4851` | Zlib |
| Zig standard library | Entropy decoding and repeat offsets in `lib/std/compress/zstd.zig` and `lib/std/compress/zstd/Decompress.zig`. Copyright (c) Zig contributors. | `core/std/zstd.hirsel` | ziglang/zig `bff490a23889750de7096d9057c0a432efc929a8` | MIT |
| Zstandard | `lib/common/fse_decompress.c:59-160` FSE table construction and `tests/golden-decompression-errors/` inputs | `core/std/zstd.hirsel`, `core/std/tests/zstd.hirsel` | facebook/zstd `49cf51799ea051457f57243527aa2058f999f005` | BSD-3-Clause (Zstandard) |
| cespare/xxhash | XXH64 stripes and digest finalization in `xxhash.go:72-109` and `xxhash_other.go:64-75`. Copyright (c) 2016 Caleb Spare. | `core/std/zstd.hirsel` | cespare/xxhash `ab37246c889f9db16b606fda1c232d659df9271d` | MIT |

Where an upstream offers MIT or Apache-2.0, the MIT option is used.

## MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## BSD-3-Clause License (The Computer Language Benchmarks Game)

All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name "The Computer Language Benchmarks Game" nor the name "The
   Benchmarks Game" nor the name "The Computer Language Shootout Benchmarks" nor
   the names of its contributors may be used to endorse or promote products
   derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT,
INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING,
BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE
OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED
OF THE POSSIBILITY OF SUCH DAMAGE.

## Valgrind header licence (valgrind.h, memcheck.h)

It applies to those two files only; the rest of Valgrind is GPL-2.0 and is not
vendored.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. The origin of this software must not be misrepresented; you must
   not claim that you wrote the original software.  If you use this
   software in a product, an acknowledgment in the product
   documentation would be appreciated but is not required.

3. Altered source versions must be plainly marked as such, and must
   not be misrepresented as being the original software.

4. The name of the author may not be used to endorse or promote
   products derived from this software without specific prior written
   permission.

THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS
OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED.  IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE
GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## BSD-3-Clause License (Go)

Copyright 2009 The Go Authors.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the name of Google LLC nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## Apache License 2.0 with LLVM Exception (Cranelift, LLVM)

Goose and OpenTelemetry are under the Apache License 2.0 alone: the text below
without the LLVM exceptions that follow it.

```
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright [yyyy] [name of copyright owner]

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.


--- LLVM Exceptions to the Apache 2.0 License ----

As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.
```

## MIT (Rust)

```text
Copyright (c) The Rust Project Contributors
Copyright (c) 2010 The Rust Project Developers (Rayon)

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## Boost Software License 1.0 (xxhash-rust)

```text
Boost Software License - Version 1.0 - August 17th, 2003

Permission is hereby granted, free of charge, to any person or organization
obtaining a copy of the software and accompanying documentation covered by
this license (the "Software") to use, reproduce, display, distribute,
execute, and transmit the Software, and to prepare derivative works of the
Software, and to permit third-parties to whom the Software is furnished to
do so, all subject to the following:

The copyright notices in the Software and this entire statement, including
the above license grant, this restriction and the following disclaimer,
must be included in all copies of the Software, in whole or in part, and
all derivative works of the Software, unless such copies or derivative
works are solely in the form of machine-executable object code generated by
a source language processor.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE
FOR ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## MIT (RustCrypto)

```text
Copyright (c) 2016-2026 The RustCrypto Project Developers
Copyright (c) 2016 Artyom Pavlov
Copyright (c) 2009-2013 Mozilla Foundation
Copyright (c) 2006-2009 Graydon Hoare

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## MIT (twox-hash)

```text
The MIT License (MIT)

Copyright (c) 2015 Jake Goulding

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Zlib License (zlib)

```text
This software is provided 'as-is', without any express or implied
warranty.  In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not
   claim that you wrote the original software. If you use this software
   in a product, an acknowledgment in the product documentation would be
   appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

## BSD-3-Clause License (Zstandard)

BSD License

For Zstandard software

Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

 * Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

 * Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

 * Neither the name Facebook, nor Meta, nor the names of its contributors may
   be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.


## BSD-2-Clause (CommonMark)

Copyright (c) 2014, John MacFarlane

All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

    * Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.

    * Redistributions in binary form must reproduce the above
      copyright notice, this list of conditions and the following
      disclaimer in the documentation and/or other materials provided
      with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## CC-BY-SA-4.0 (CommonMark examples)

Copyright (C) 2014–16 John MacFarlane. The CommonMark 0.31.2 spec examples
are repackaged as newline-separated JSON pairs inside the Markdown contract
test. The Markdown and HTML strings retain their original contents. The
examples are licensed under <https://creativecommons.org/licenses/by-sa/4.0/>.
