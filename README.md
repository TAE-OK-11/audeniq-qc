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

FLAC/ALAC 디코더는 필요한 기능만 켠 Symphonia의 기존 Rust 구현입니다. TTA/WavPack 및 FLAC 인코더·K-weighting은 고정한 FFmpeg 소스를 참고/포팅했고, 버퍼·검증·계산 경로를 재설계했습니다. [출처와 라이선스](THIRD_PARTY.md)를 유지합니다.

## 사용

Rust 1.89 이상이 필요합니다. SHA-256은 RustCrypto의 x86 SHA-NI/AArch64 SHA2 감지를 사용하고, IEEE CRC32는 crc32fast의 하드웨어 경로를 사용합니다. 지원 여부를 런타임에 검사하며 미지원 CPU에서는 기준 구현을 선택합니다. `--scalar`는 자체 DSP 커널을 바꾸며 해시 라이브러리의 하드웨어 감지는 유지합니다.

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

표준 출력은 JSON 한 개입니다. 실패 시 종료 코드 2와 표준 오류 JSON을 반환합니다. `--scalar`, `--timeout-secs 60`을 지원합니다. `probe`는 메타데이터 확인이며 전체 오디오 무결성의 증명이 아닙니다. `analyze`와 `convert`가 전체 디코딩을 검사합니다. `convert`는 기존 출력을 덮어쓰지 않습니다.

`convert --analyze`의 `analysis`는 입력 PCM의 QC입니다. 출력 FLAC의 재디코딩·규격·프레임 수·PCM 해시가 일치한 뒤 결과를 게시합니다. 상위 `spec`은 출력 FLAC, `analysis.spec`은 원본 입력입니다. standalone `analyze`와 같은 규격/백엔드에서 지표와 지문이 정확히 일치합니다. `--fingerprint`는 `analyze` 또는 `convert --analyze`에만 적용합니다. 압축 레벨 0~8과도 함께 사용할 수 있습니다.

`decode`도 기존 출력을 덮어쓰지 않고 손상된 입력의 결과를 게시하지 않습니다. WAV는 입력의 샘플레이트·채널·16/24-bit 깊이를 그대로 유지합니다. raw s32le는 같은 샘플을 32-bit 정수에 왼쪽 정렬합니다. 보고서의 `spec`은 출력 규격, `source_spec`은 입력 규격입니다. raw 출력은 헤더가 없으므로 보고서를 같이 보관해야 합니다. WAV 출력은 RIFF 크기 및 지정한 파일 크기 제한을 초과하면 오류입니다.

## 압축 레벨

`convert --compression-level 0..8`은 자체 무손실 프리셋입니다. FFmpeg/libFLAC의 같은 번호와 일치하는 설정을 의미하지 않습니다. 레벨을 명시하면 FLAC 입력도 다시 인코딩합니다. 생략하면 일반 입력은 레벨 5를 사용하고, FLAC은 검증한 오디오 프레임을 보존하며 태그만 제거합니다.

| 레벨 | 블록 프레임 | 최대 fixed 예측 차수 | 최대 LPC 차수 |
|---|---:|---:|---:|
| 0 | 1024 | 0 | 0 |
| 1 | 2048 | 1 | 0 |
| 2 | 4096 | 2 | 0 |
| 3 | 4096 | 3 | 0 |
| 4 | 4096 | 4 | 4 |
| 5 (기본) | 4096 | 4 | 8 |
| 6 | 8192 | 4 | 8 |
| 7 | 16384 | 4 | 8 |
| 8 | 32768 | 4 | 8 |

모든 레벨에서 PCM SHA-256을 계산하고 출력을 재디코딩해 검증합니다. 레벨 4~5는 최대 128개의 균일 간격 정수 잔차로 LPC 후보를 먼저 평가하고, 선택한 후보만 전체 샘플에서 정확히 계산합니다. 레벨 6~8은 전체 후보 평가를 유지합니다. 이 선택은 압축률에만 영향을 주며 PCM 복원에는 영향을 주지 않습니다. 높은 레벨은 더 큰 블록·예측 후보를 사용하므로 메모리와 계산 비용이 달라집니다. 입력마다 압축률이 달라지며 높은 레벨이 항상 더 작은 파일을 만드는 것은 아닙니다. 손실 압축, 샘플레이트 변경, bit depth 축소, ALAC/WavPack/TTA 인코딩은 구현 범위에 포함하지 않습니다.

`src/lib.rs`의 공개 Rust API도 있습니다. 업로드 파일을 다루는 AUDENIQ에서는 CLI를 기존 Linux parser sandbox 안에서 실행해야 합니다. 라이브러리를 워커 내부에서 직접 호출하여 격리를 없애면 안 됩니다. 세부 연결 조건은 [INTEGRATION.md](docs/INTEGRATION.md)에 기록합니다.

## 정확도와 측정

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

Zen3에서는 AVX2, AArch64에서는 NEON을 사용하며, 전역 `target-cpu=native`나 LTO 변경에 의존하지 않습니다. [EPYC 7763 실행](docs/benchmark-zen3-7763.json)에서도 AVX2 경로를 검증했습니다. GitHub 러너의 실제 CPU는 실행마다 달라질 수 있으므로 각 결과의 CPU 식별자·소스 커밋·선택한 커널을 확인하세요. Arm CPU의 세대/제품명이 확인되지 않아 요청한 Arm 4세대 장비로 표시하지 않습니다. 배포할 장비에서도 위 명령으로 다시 측정할 수 있습니다.

## 라이선스

FFmpeg 번역 코드를 포함하므로 LGPL-2.1-or-later입니다. 최적화/재작성 후에도 출처와 해당 의무가 유지됩니다. Rust 의존성에는 MPL-2.0, MIT, Apache-2.0 등이 적용됩니다. [LICENSE](LICENSE), [THIRD_PARTY.md](THIRD_PARTY.md)를 참조하세요.
