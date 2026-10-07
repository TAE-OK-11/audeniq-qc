# audeniq-qc

AUDENIQ 전용 Rust 오디오 엔진. 실제 AUDENIQ 소스의 FFmpeg·FFprobe·LUFS 사용처를 [먼저 조사](docs/AUDIT.md)하고, 필요한 무손실 처리와 QC만 구현했습니다. 런타임 FFmpeg·libav* 의존성이 없고 미디어 처리 경로는 Rust로 동작합니다.

독립 엔진 구현과 최적화, EPYC 7763(Zen3)·Arm 검증 결과를 공개합니다. 요청에 따라 백엔드 연결·배포·지문 이관은 작업 범위에서 제외합니다. 전체 공식 EBU 파일 세트, 실제 음악 코퍼스, 실제 배포 장비의 부하 검증은 별도입니다. 현재 검증 범위와 측정의 한계는 [VALIDATION.md](docs/VALIDATION.md)를 참조하세요.

최우선 대상은 **AWS Graviton4의 WAV/ALAC → FLAC와 심사용 QC**입니다. 변환 중 QC·선택적 지문을 함께 계산하는 경로, Arm NEON 필터/정수 비용 계산, 복사·버퍼 개선을 추가했습니다. 같은 러너에서 이전 엔진과 비교한 결과 및 Graviton4 재현 명령은 [GRAVITON4.md](docs/GRAVITON4.md)에 기록합니다. 현재 Arm CI는 Neoverse N2이며 Graviton4 실측으로 표시하지 않습니다.

## 구현 범위

| 기능 | 구현 |
|---|---|
| FFprobe 대체 | 로컬 오디오 규격/길이/코덱, JPEG·PNG 커버 크기, 필요한 출처 태그 |
| FFmpeg 디코딩 대체 | WAV/RF64/BW64, AIFF/AIFC, FLAC, M4A ALAC, TTA1, 정수 무손실 WavPack |
| 분석 | 한 번의 스트리밍 디코딩으로 LUFS, True Peak, 샘플 피크, 클리핑, 50ms 무음/에너지, 영교차, PCM SHA-256 |
| PCM 출력 | 원본 규격 WAV 또는 canonical left-aligned s32le, 출력 검증 후 원자적 게시 |
| 정규화 | 샘플을 변경하지 않는 FLAC 16/24-bit, 적응형 LPC/Rice 인코딩 또는 검증한 FLAC 프레임 보존, 출력 재디코딩·해시 검증 후 원자적 게시 |
| 변환+심사용 QC | `convert --analyze [--fingerprint]`에서 같은 source PCM으로 분석; QC용 추가 디코딩과 중복 SHA-256 제거 |
| 지문 입력 | 연속 필터 상태의 11025Hz mono s16, head/middle/tail 합계 최대 90초만 보관 |
| CPU 커널 | 런타임 AVX2/NEON 선택: PCM unpack, True Peak 4상 FIR, 지문 FIR, FLAC LPC autocorrelation·정수 예측; scalar fallback |

1~2채널, 16/24-bit 정수, 44100~192000Hz가 경계입니다. 비디오, 네트워크, 손실 코덱, float/32-bit PCM, hybrid/DSD/float WavPack, fragmented MP4는 지원하지 않습니다. RF64 확장 ds64 테이블과 샘플 수가 선언되지 않은 FLAC도 지원하지 않습니다. 지원 불가나 손상은 오류이며 부분 성공으로 처리하지 않습니다.

기본 빌드의 FLAC/ALAC/TTA/WavPack 디코더와 FLAC 인코더는 고정한 FFmpeg 소스를 참고/포팅한 자체 Rust 경로입니다. WAV/AIFF 및 제한된 단일 ALAC 트랙 M4A 컨테이너도 자체 처리합니다. 기존 Symphonia 코덱은 `--features reference-codecs` 비교 빌드에만 남깁니다. JSON·SHA-256·MD5·CRC32·JPEG/PNG 검증도 자체 구현이며 기본 빌드에는 외부 크레이트가 없습니다(기존 라이브러리는 테스트 비교용 dev-dependency로만 남김). [출처와 라이선스](THIRD_PARTY.md)를 유지합니다.

## 사용

Rust 1.89 이상이 필요합니다. SHA-256(x86 SHA-NI/AArch64 SHA2), FLAC MD5와 IEEE CRC32(x86 AVX-512/AVX2 VPCLMULQDQ·PCLMULQDQ, AArch64 PMULL)는 저장소 안의 구현을 사용합니다. 지원 여부를 런타임에 검사하며 미지원 CPU에서는 이식 가능한 구현을 선택합니다. `--scalar`는 자체 DSP 커널을 바꾸며 해시/CRC의 하드웨어 감지는 유지합니다.

```sh
cargo build --release --locked
target/release/audeniq-qc capabilities
target/release/audeniq-qc probe master.m4a
target/release/audeniq-qc analyze master.flac --fingerprint
target/release/audeniq-qc fingerprint master.flac
target/release/audeniq-qc pcm-hash master.wav
target/release/audeniq-qc convert master.tta normalized.flac
target/release/audeniq-qc convert master.m4a normalized.flac --compression-level 5
target/release/audeniq-qc convert master.m4a normalized.flac --analyze --fingerprint
target/release/audeniq-qc decode master.wv decoded.wav --format wav
target/release/audeniq-qc decode master.flac decoded.s32le --format s32le
target/release/audeniq-qc probe cover.png
target/release/audeniq-qc tags master.wv
```

정적 musl 빌드(`rustup target add $(uname -m)-unknown-linux-musl` 후 `cargo build --release --locked --target $(uname -m)-unknown-linux-musl`)는 같은 출력을 만들면서 공유 라이브러리 페이지가 없어 최대 RSS가 작습니다(레벨 5: Arm 2.9 → 1.3 MiB, x86 3.8 → 1.6 MiB, CPU 차이 2% 이내, [라운드 16](docs/BENCHMARK-VERIFIED-PIPELINE.md#round-16-level-5-smaller-with-less-cpu-static-musl-build)).

표준 출력은 JSON 한 개입니다. 실패 시 종료 코드 2와 표준 오류 JSON을 반환합니다. `--scalar`, `--timeout-secs 60`을 지원합니다. `probe`는 메타데이터 확인이며 전체 오디오 무결성의 증명이 아닙니다. `analyze`와 `convert`가 전체 디코딩을 검사합니다. `convert`는 기존 출력을 덮어쓰지 않습니다.

`convert --analyze`의 `analysis`는 입력 PCM의 QC입니다. 출력 FLAC의 재디코딩·규격·프레임 수·PCM 해시가 일치한 뒤 결과를 게시합니다. 상위 `spec`은 출력 FLAC, `analysis.spec`은 원본 입력입니다. standalone `analyze`와 같은 규격/백엔드에서 지표와 지문이 정확히 일치합니다. `--fingerprint`는 `analyze` 또는 `convert --analyze`에만 적용합니다. 압축 레벨 0~8과도 함께 사용할 수 있습니다.

`decode`도 기존 출력을 덮어쓰지 않고 손상된 입력의 결과를 게시하지 않습니다. WAV는 입력의 샘플레이트·채널·16/24-bit 깊이를 그대로 유지합니다. raw s32le는 같은 샘플을 32-bit 정수에 왼쪽 정렬합니다. 보고서의 `spec`은 출력 규격, `source_spec`은 입력 규격입니다. raw 출력은 헤더가 없으므로 보고서를 같이 보관해야 합니다. WAV 출력은 RIFF 크기 및 지정한 파일 크기 제한을 초과하면 오류입니다.

## 압축 레벨

`convert --compression-level 0..8`은 자체 무손실 프리셋입니다. FFmpeg/libFLAC의 같은 번호와 일치하는 설정을 의미하지 않습니다. 레벨을 명시하면 FLAC 입력도 다시 인코딩합니다. 생략하면 일반 입력은 레벨 5를 사용하고, FLAC은 검증한 오디오 프레임을 보존하며 태그만 제거합니다.

모든 레벨은 fixed 예측 0~4차(한 번의 통합 패스)를 사용하고 FLAC streamable subset을 지킵니다(48kHz 이하: LPC 12차·4096 프레임 블록, 48kHz 초과: LPC 32차까지·레벨 6~8은 8192 프레임 블록).

| 레벨 | 최대 LPC 차수 (≤48kHz / >48kHz) | 정확히 계산하는 LPC 차수 | 최대 Rice 분할 차수 | 정확히 비교하는 스테레오 배치 | 테스트 코퍼스 바이트 (FFmpeg 6.1 같은 번호) |
|---|---:|---|---:|---:|---:|
| 0 | 0 | - | 3 | 1 | 71,700,400 (73,909,130) |
| 1 | 0 | - | 6 | 1 | 71,628,179 (72,199,900) |
| 2 | 4 | 추정 1개 | 6 | 1 | 68,619,943 (72,127,350) |
| 3 | 8 | 추정 1개 | 6 | 1 | 67,627,433 (68,574,784) |
| 4 | 12 | 추정 1개 | 6 | 1 | 67,349,714 (68,295,424) |
| 5 (기본) | 12 / 16 | 추정 1개 | 6 | 1 | 67,325,634 (68,251,657) |
| 6 | 12 / 32 | 추정 1개 | 8 | 1 | 67,223,444 (68,226,043) |
| 7 | 12 / 32 | 추정 + 최고 차수 | 8 | 2 | 67,137,764 (68,224,152) |
| 8 | 12 / 32 | 추정 ±2 + 최고 차수 | 8 | 4 | 67,123,667 (67,939,273) |

LPC 계수는 13-bit로 양자화합니다. Levinson-Durbin 오차로 차수별 크기를 추정하고(잔차 log2(오차)/2 비트 + 워밍업·계수 비트), 추정이 가장 작은 차수만 전체 샘플에서 정확히 계산합니다. 스테레오 배치(L/R, L/S, S/R, M/S)는 블록 가운데 절반의 4차 Levinson 추정으로 순위를 매깁니다. 코퍼스는 IETF CELLAR FLAC 테스트 파일 중 지원 규격 49곡(CC0)이며 [라운드 13~14](docs/BENCHMARK-VERIFIED-PIPELINE.md#round-14-streamable-subset-stereo-estimate-and-block-size)에 측정 조건과 기각한 가설을 기록했습니다.

모든 레벨에서 PCM SHA-256을 계산하고 모든 프레임을 원본과 비교 검증한 뒤 게시합니다. 예측 모델 선택은 압축률에만 영향을 주며 PCM 복원에는 영향을 주지 않습니다. 입력마다 압축률이 달라지며 높은 레벨이 항상 더 작은 파일을 만드는 것은 아닙니다. 손실 압축, 샘플레이트 변경, bit depth 축소, ALAC/WavPack/TTA 인코딩은 구현 범위에 포함하지 않습니다.

`src/lib.rs`의 공개 Rust API도 있습니다. 업로드 파일을 다루는 AUDENIQ에서는 CLI를 기존 Linux parser sandbox 안에서 실행해야 합니다. 라이브러리를 워커 내부에서 직접 호출하여 격리를 없애면 안 됩니다. 세부 연결 조건은 [INTEGRATION.md](docs/INTEGRATION.md)에 기록합니다.

## 정확도와 측정

[인코더·외부 의존성 자체화 조사](docs/DEPENDENCY-AUDIT.md)에 이미 자체 구현한 부분, MD5/SHA-256/CRC32/JPEG/PNG/JSON 자체화 경과와 측정 결과([검증 파이프라인 벤치마크](docs/BENCHMARK-VERIFIED-PIPELINE.md) 라운드 4~8)를 정리했습니다.

[FFmpeg·자체·일부 외부 코덱 3개 재측정](docs/BENCHMARK-THREEWAY-RERUN.md)을 N2와 EPYC 9V74에서 완료했습니다. WAV/ALAC 변환, QC, 변환+QC의 CPU·처리 시간·RAM·파일 크기와 반복 원시값을 공개합니다. 이번 x86 결과는 이전 EPYC 7763 Zen3 측정과 구분합니다.

최신 [자체 코덱·컨테이너 병목 개선 결과](docs/NATIVE-BOTTLENECKS.md)는 `b8918279`를 Neoverse N2와 EPYC 7763에서 검증했습니다. 같은 서버의 이전 자체 구현 대비 ALAC 디코딩 CPU는 각각 21.1%/19.2%, ALAC→FLAC은 8.5%/8.1%, 변환+QC는 7.0%/5.6% 감소했습니다. WAV 변환 CPU는 거의 같았습니다. 복사·버퍼 생성 감소, 제거한 느린 SIMD 후보, 남은 느린 경로와 전체 원시 반복을 함께 공개합니다. N2 측정은 Graviton4 실측을 대신하지 않습니다.

[이전 WAV/ALAC 비교](docs/BENCHMARK-WAV-ALAC.md)는 당시 구현의 기록이며 최신 결과는 위 보고서를 기준으로 확인하세요.

```sh
# FFmpeg는 아래 개발/검증 명령에서만 사용합니다.
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo build --workspace --release --locked
target/release/audeniq-qc-tools qualify --output qualification.json
target/release/audeniq-qc-tools standards --output standards.json
target/release/audeniq-qc-tools codec-stress --output codec-stress.json
target/release/audeniq-qc-tools benchmark --seconds 240 --repeats 5 --output benchmark.json
target/release/audeniq-qc-tools benchmark --seconds 240 --repeats 5 --fingerprint --output fingerprint-benchmark.json
target/release/audeniq-qc-tools benchmark-convert --seconds 60 --repeats 3 --output normalization.json
target/release/audeniq-qc-tools benchmark-review --seconds 120 --repeats 5 --fingerprint --output review.json
```

엔진과 개발 도구 모두 Rust입니다. 일반 `cargo build --release --locked`는 엔진만 빌드합니다. `--workspace`를 지정하면 별도 개발 도구도 빌드합니다. 개발 검증은 비교 기준인 FFmpeg, Linux 자원 측정은 GNU time이 필요합니다. `AUDENIQ_GNU_TIME`으로 GNU time 경로를 지정할 수 있습니다. 테스트 난수는 고정 시드 SplitMix64이며, 도구의 메모리는 측정 대상 엔진의 RSS에 포함하지 않습니다.

[PCM·변환·손상·태그 비교](docs/qualification.json)와 [EBU 설명을 따라 합성한 기준 테스트](docs/standards-synthesized.json)를 공개합니다. 합성 테스트는 공식 파일 세트 인증을 대신하지 않습니다. LUFS와 지문 리샘플러는 버전이 명시되며, 지문 PCM은 FFmpeg 기본값과 비트 단위로 같지 않습니다. True Peak는 ITU-R BS.1770-5 Annex 2의 공개 FIR 계수를 사용합니다. 백색 잡음의 FFmpeg 비교 차이도 결과에 기록되어 있습니다.

이전 독립 엔진의 [x86 원시 측정](docs/benchmark-ci-x86.json), [Arm 원시 측정](docs/benchmark-ci-arm.json), [검증 조건](docs/VALIDATION.md)과 최신 [Arm 우선 최적화 결과](docs/GRAVITON4.md)를 공개합니다. 양쪽 모두 디코딩+LUFS/True Peak+PCM SHA-256을 수행합니다. native는 추가 QC 지표도 계산합니다. 지문을 포함한 분석과 검증한 FLAC 변환도 각각 측정합니다. CPU 시간은 user+system, 메모리는 프로세스 최대 RSS이며, AUDENIQ 전체 파이프라인 속도 향상 수치는 아닙니다.

Zen3에서는 AVX2, AArch64에서는 NEON을 사용하며, 전역 `target-cpu=native`에 의존하지 않습니다. release 프로필은 x86_64와 aarch64 모두 fat LTO(`codegen-units = 1`)로 빌드합니다. SIMD는 런타임 선택 커널이 담당하므로 LTO는 모듈 간 인라이닝과 바이너리 크기(상주 text 페이지)에 대한 설정입니다. [EPYC 7763 실행](docs/benchmark-zen3-7763.json)에서도 AVX2 경로를 검증했습니다. GitHub 러너의 실제 CPU는 실행마다 달라질 수 있으므로 각 결과의 CPU 식별자·소스 커밋·선택한 커널을 확인하세요. Arm CPU의 세대/제품명이 확인되지 않아 요청한 Arm 4세대 장비로 표시하지 않습니다. 배포할 장비에서도 위 명령으로 다시 측정할 수 있습니다.

## 라이선스

FFmpeg 번역 코드를 포함하므로 LGPL-2.1-or-later입니다. 최적화/재작성 후에도 출처와 해당 의무가 유지됩니다. Rust 의존성에는 MPL-2.0, MIT, Apache-2.0 등이 적용됩니다. [LICENSE](LICENSE), [THIRD_PARTY.md](THIRD_PARTY.md)를 참조하세요.
