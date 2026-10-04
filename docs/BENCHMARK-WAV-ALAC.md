# WAV/ALAC 변환·QC: Arm 및 x86, 7회 실측

2026-10-04에 독립 변환과 독립 QC를 각각 측정했습니다. **QC는 두 서버에서 모두 더 빠르고 CPU·RSS가 작았습니다. ALAC→FLAC은 CPU·RSS가 작지만 처리 시간은 FFmpeg보다 길었습니다.** 이 결과는 아래 입력과 러너에 한정됩니다.

- 측정 소스: [aac7a0a](https://github.com/TAE-OK-11/audeniq-qc/commit/aac7a0aa2de70d4f60bc7451009c6626515aa109). 엔진 코드는 [0cc8630](https://github.com/TAE-OK-11/audeniq-qc/commit/0cc863097730cde47b5a03493ff3486724a7b526)과 같습니다.
- [벤치마크 CI](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37189717546): 두 아키텍처 모두 성공. 같은 소스의 [전체 검증 CI](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37189717512)도 성공했습니다.
- 입력: 240초, 48000Hz, 24-bit, 스테레오 합성 톤. 각 작업·엔진 예열 후 실행 순서를 순환하여 7회 측정, 각 지표 중앙값. 따뜻한 페이지 캐시입니다.
- rustc 1.98.1 (48a229cea 2026-09-01); FFmpeg 6.1.1-3ubuntu5. 일반 release 빌드, 엔진 한 프로세스, FFmpeg 코덱 threads=1. 별도의 LTO/target-cpu 튜닝을 적용하지 않았습니다.
- Arm: CPU implementer=0x41, part=0xd49, revision=0, Neoverse N2; DSP NEON. **AWS Graviton4/Neoverse V2 실측이 아닙니다.**
- x86: INTEL(R) XEON(R) PLATINUM 8573C; DSP AVX2. 이번 x86 결과는 EPYC Zen3가 아닙니다. 기존 Zen3 결과는 [GRAVITON4.md](GRAVITON4.md)에 별도로 있습니다.
- 원시 실행 7회, 명령 전체, 바이너리/입력 SHA-256, CPU 식별자, 런타임 기능: [Arm JSON](benchmark-wav-alac-arm.json), [x86 JSON](benchmark-wav-alac-x86.json).

아래 셀은 **audeniq-qc / FFmpeg** 순서입니다. CPU 시간은 각 실행의 user+system 합계로, 순간 CPU 사용률이 아닙니다. RAM은 GNU time 최대 RSS를 MiB로 환산했습니다. 표는 소수 두 자리로 표시하며 원시 값은 JSON에 있습니다.

## Arm

| 작업 | 처리 시간 (초) | CPU 시간 (초) | 최대 RSS (MiB) | CPU 시간 감소 |
|---|---:|---:|---:|---:|
| WAV → FLAC | 1.27 / 1.42 | 1.18 / 1.99 | 3.22 / 53.46 | 40.70% |
| ALAC → FLAC | 1.82 / 1.51 | 1.73 / 2.01 | 3.22 / 53.46 | 13.93% |
| WAV QC | 0.36 / 2.37 | 0.36 / 2.67 | 2.72 / 53.82 | 86.52% |
| ALAC QC | 0.92 / 2.41 | 0.92 / 2.66 | 2.97 / 53.38 | 65.41% |

## x86

| 작업 | 처리 시간 (초) | CPU 시간 (초) | 최대 RSS (MiB) | CPU 시간 감소 |
|---|---:|---:|---:|---:|
| WAV → FLAC | 1.26 / 1.58 | 1.15 / 2.52 | 4.02 / 57.13 | 54.37% |
| ALAC → FLAC | 1.82 / 1.62 | 1.71 / 2.37 | 4.02 / 56.94 | 27.85% |
| WAV QC | 0.40 / 1.82 | 0.39 / 2.36 | 3.60 / 55.29 | 83.47% |
| ALAC QC | 0.95 / 1.78 | 0.94 / 2.14 | 4.00 / 54.36 | 56.07% |

## 비교 작업과 검증

**변환:** 양쪽 모두 입력 디코딩/PCM SHA-256 → FLAC 인코딩 → 출력 전체 재디코딩/PCM SHA-256 검증을 포함합니다. audeniq-qc는 fsync와 기존 파일을 덮어쓰지 않는 원자적 게시도 시간에 포함합니다. FFmpeg는 인코딩·입력 해시를 한 프로세스로 수행하고 별도 프로세스로 출력을 검증합니다. 두 FFmpeg 실행의 CPU/처리 시간을 더하고 RSS는 두 프로세스 중 최댓값을 사용합니다. 따라서 이 표는 검증을 포함한 변환이며 단순 인코딩만의 속도가 아닙니다. FFprobe나 백엔드 처리 시간은 포함하지 않습니다.

모든 반복에서 원본과 변환 결과의 PCM 동일성을 확인했습니다. native 출력은 별도 FFmpeg 디코딩/해시로도 검증하며, 이 독립 검증은 native 시간 밖에서 실행합니다. Arm·x86의 변환 입력과 출력 canonical left-aligned s32le PCM SHA-256은 모두 다음과 같습니다.

```text
26cf6f93e903bc9aafb549ccf11626b7bb0bb83278a4288d3f98e3b0ac7e8261
```

압축 설정은 native 기본 프리셋 5, FFmpeg FLAC 레벨 5입니다. 두 레벨 번호가 같은 압축 알고리즘/탐색 비용을 뜻하지 않습니다. 두 입력의 출력 크기는 각 호스트에서 동일했습니다.

| 서버 | audeniq-qc FLAC (bytes) | FFmpeg FLAC (bytes) | native 크기 증가 |
|---|---:|---:|---:|
| Arm | 32427602 | 32297578 | 0.40% |
| x86 | 32427602 | 32261824 | 0.51% |

**QC:** 양쪽 모두 한 번 디코딩하여 LUFS·True Peak와 canonical PCM SHA-256을 계산합니다. native는 샘플 피크, 클리핑, 무음/에너지, 영교차 등도 계산합니다. FFmpeg는 ebur128=peak=true 및 PCM hash 출력으로 비교합니다. 지문 downmix/resample, 파일 변환, 별도 FFprobe 및 백엔드 처리는 포함하지 않습니다. True Peak 필터가 서로 다르므로 성능 수치로 정확도의 우위를 판단하지 않습니다.

Arm WAV QC는 native DSP scalar 처리 시간 0.67초에서 NEON 0.36초, ALAC QC는 1.22초에서 0.92초였습니다. 이 비교도 동일한 7회 중앙값입니다. scalar 모드는 자체 DSP를 바꾸며 해시 라이브러리와 PCM 배치의 하드웨어 경로는 유지합니다. SVE/SVE2는 지원 여부를 보고하지만 이번 DSP 실행 경로는 NEON입니다.

## 적용 범위와 다음 최적화 지점

- WAV→FLAC은 Arm에서 약 1.12배, x86에서 약 1.25배 빠릅니다.
- ALAC→FLAC은 native 처리 시간이 Arm에서 20.5%, x86에서 12.3% 더 길어 추가 개선 대상입니다. CPU 시간을 적게 쓴다는 사실이 항상 짧은 처리 시간을 뜻하지 않습니다.
- QC 처리 속도는 Arm WAV 6.58배/ALAC 2.62배, x86 WAV 4.55배/ALAC 1.87배입니다.
- GitHub 공유 러너의 합성 입력 결과입니다. 실제 음악 코퍼스, 차가운 캐시, 동시 처리, p95 지연, Graviton4 및 AUDENIQ 백엔드 전체 성능은 측정하지 않았습니다. 백엔드 연결은 변경하지 않았습니다.
- Graviton4에서 아래와 같은 명령을 실행하고 CPU 식별자·실행 커널·원시 반복을 함께 보관해야 해당 장비 성능을 판단할 수 있습니다.

## 재현

Rust로 작성한 도구이며 FFmpeg와 GNU time은 개발 비교 기준에만 필요합니다.

```sh
cargo build --workspace --release --locked
target/release/audeniq-qc capabilities
target/release/audeniq-qc-tools benchmark-convert --wav-alac-only --seconds 240 --repeats 7 --output normalization-benchmark.json
target/release/audeniq-qc-tools benchmark --wav-alac-only --seconds 240 --repeats 7 --output analysis-benchmark.json
```
