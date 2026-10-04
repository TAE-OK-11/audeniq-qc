# audeniq-qc

AUDENIQ 전용 Rust 오디오 엔진. 실제 AUDENIQ 소스의 FFmpeg·FFprobe·LUFS 사용처를 [먼저 조사](docs/AUDIT.md)하고, 필요한 무손실 처리와 QC만 구현했습니다. 런타임 FFmpeg, libav*, C FFI는 없습니다.

현재는 검증 가능한 첫 구현입니다. 이 저장소의 엔진은 동작하지만, 운영 AUDENIQ를 자동 배포하거나 기존 지문을 새 버전으로 이관하지 않았습니다. 전체 공식 EBU 파일 세트, 실제 음악 코퍼스, EPYC Zen3/요청한 Arm 장비의 검증은 별도입니다. FFmpeg보다 항상 빠르거나 더 정확하다고 주장하지 않습니다.

## 구현 범위

| 기능 | 구현 |
|---|---|
| FFprobe 대체 | 로컬 오디오 규격/길이/코덱, JPEG·PNG 커버 크기, 필요한 출처 태그 |
| FFmpeg 디코딩 대체 | WAV/RF64/BW64, AIFF/AIFC, FLAC, M4A ALAC, TTA1, 정수 무손실 WavPack |
| 분석 | 한 번의 스트리밍 디코딩으로 LUFS, True Peak, 샘플 피크, 클리핑, 50ms 무음/에너지, 영교차, PCM SHA-256 |
| 정규화 | 샘플을 변경하지 않는 FLAC 16/24-bit, 출력 재디코딩·해시 검증 후 원자적 게시 |
| 지문 입력 | 연속 필터 상태의 11025Hz mono s16, head/middle/tail 합계 최대 90초만 보관 |
| CPU 커널 | 런타임 AVX2/NEON 선택: PCM unpack, True Peak 4상 FIR, 지문 FIR; scalar fallback |

1~2채널, 16/24-bit 정수, 44100~192000Hz가 경계입니다. 비디오, 네트워크, 손실 코덱, float/32-bit PCM, hybrid/DSD/float WavPack, fragmented MP4는 지원하지 않습니다. RF64 확장 ds64 테이블은 지원하지 않습니다. 지원 불가나 손상은 오류이며 부분 성공으로 처리하지 않습니다.

FLAC/ALAC 디코더는 필요한 기능만 켠 Symphonia의 기존 Rust 구현입니다. TTA/WavPack 및 FLAC 인코더·K-weighting은 고정한 FFmpeg 소스를 참고/포팅했고, 버퍼·검증·계산 경로를 재설계했습니다. [출처와 라이선스](THIRD_PARTY.md)를 유지합니다.

## 사용

```sh
cargo build --release --locked
target/release/audeniq-qc capabilities
target/release/audeniq-qc probe master.m4a
target/release/audeniq-qc analyze master.flac --fingerprint
target/release/audeniq-qc fingerprint master.flac
target/release/audeniq-qc pcm-hash master.wav
target/release/audeniq-qc convert master.tta normalized.flac
target/release/audeniq-qc probe cover.png
target/release/audeniq-qc tags master.wv
```

표준 출력은 JSON 한 개입니다. 실패 시 종료 코드 2와 표준 오류 JSON을 반환합니다. `--scalar`, `--timeout-secs 60`을 지원합니다. `probe`는 메타데이터 확인이며 전체 오디오 무결성의 증명이 아닙니다. `analyze`와 `convert`가 전체 디코딩을 검사합니다. `convert`는 기존 출력을 덮어쓰지 않습니다.

`src/lib.rs`의 공개 Rust API도 있습니다. 업로드 파일을 다루는 AUDENIQ에서는 CLI를 기존 Linux parser sandbox 안에서 실행해야 합니다. 라이브러리를 워커 내부에서 직접 호출하여 격리를 없애면 안 됩니다. 세부 연결 조건은 [INTEGRATION.md](docs/INTEGRATION.md)에 기록합니다.

## 정확도와 측정

```sh
# FFmpeg는 아래 개발/검증 명령에서만 사용합니다.
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/qualify.py --output qualification.json
python3 scripts/standards.py --output standards.json
python3 scripts/benchmark.py --seconds 240 --repeats 5 --output benchmark.json
python3 scripts/benchmark_convert.py --seconds 60 --repeats 3 --output normalization.json
```

[PCM·변환·손상·태그 비교](docs/qualification.json)와 [EBU 설명을 따라 합성한 기준 테스트](docs/standards-synthesized.json)를 공개합니다. 합성 테스트는 공식 파일 세트 인증을 대신하지 않습니다. LUFS와 지문 리샘플러는 버전이 명시되며, 지문 PCM은 FFmpeg 기본값과 비트 단위로 같지 않습니다. True Peak는 ITU-R BS.1770-5 Annex 2의 공개 FIR 계수를 사용합니다. 백색 잡음의 FFmpeg 비교 차이도 결과에 기록되어 있습니다.

Intel Xeon Platinum 8573C, 4분 48kHz/24-bit stereo 합성 음원, 캐시 예열 후 5회 중앙값:

| 입력 | 분석 시간 native / FFmpeg | CPU 시간 감소 | 최대 RSS 감소 |
|---|---:|---:|---:|
| WAV | 0.42 / 2.28초 | 83.6% | 93.9% |
| FLAC | 0.81 / 1.72초 | 59.9% | 85.9% |
| ALAC | 1.03 / 2.15초 | 58.3% | 93.0% |

[원시 측정·명령·해시·반복](docs/benchmark-intel-240s.json). 양쪽 모두 디코딩+LUFS/True Peak+PCM SHA-256을 수행합니다. native는 추가 QC 지표도 계산합니다. 지문은 양쪽 모두 끈 상태이며, AUDENIQ 전체 파이프라인 속도 향상이 아닙니다. 변환은 별도 측정합니다.

Zen3에서는 AVX2, AArch64에서는 NEON 경로를 선택합니다. 전역 `target-cpu=native`나 LTO 변경에 의존하지 않습니다. 실제 장비에서 ISA 검증과 재측정이 필요합니다. Arm은 AMD EPYC와 다른 아키텍처이며, 요청한 Arm 4세대의 정확한 CPU 모델은 아직 확인되지 않았습니다. 지원하지 않는 ISA나 GPU를 억지로 켜지 않습니다.

## 라이선스

FFmpeg 번역 코드를 포함하므로 LGPL-2.1-or-later입니다. 최적화/재작성 후에도 출처와 해당 의무가 유지됩니다. Rust 의존성에는 MPL-2.0, MIT, Apache-2.0 등이 적용됩니다. [LICENSE](LICENSE), [THIRD_PARTY.md](THIRD_PARTY.md)를 참조하세요.
