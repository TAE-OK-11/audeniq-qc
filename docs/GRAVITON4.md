# Graviton4 우선 최적화

주요 대상은 AWS Graviton4에서 WAV/ALAC → FLAC 변환과 심사용 QC입니다. 백엔드 연결은 별도이며 엔진과 검증 도구는 Rust입니다.

AWS/Arm 문서에 따르면 Graviton4는 Neoverse V2, Armv9, NEON·SVE2와 코어당 4개의 128-bit SIMD 실행 경로를 제공합니다. [AWS 기술 가이드](https://aws.github.io/graviton/), [Arm Graviton4 설명](https://developer.arm.com/community/arm-community-blogs/b/servers-and-cloud-computing-blog/posts/leading-hpc-performance-with-graviton4)를 기준으로 했습니다. SVE2가 있다는 사실만으로 이 DSP에서 NEON보다 빨라지는 것은 아닙니다. 현재 수치로 검증한 커널은 NEON이며 SVE2 커널은 추가하지 않았습니다.

## 적용한 변경

* `convert --analyze [--fingerprint]`: 원본 PCM을 디코딩할 때 QC와 선택적 지문 입력도 계산합니다. 입력 SHA-256도 공유합니다. 출력 FLAC은 반드시 별도로 재디코딩해서 해시·규격·프레임 수를 검증합니다. 따라서 QC용 세 번째 디코딩은 줄이고 무손실 검증은 유지합니다.
* ALAC/FLAC의 planar S32를 caller 버퍼로 직접 interleave합니다. Arm에서는 NEON 구조화 store를 사용해 중간 SampleBuffer와 재복사를 제거합니다.
* canonical s32le SHA-256은 little-endian CPU에서 PCM의 읽기 전용 byte view를 해시합니다. 샘플별 endian 변환·추가 PCM byte 버퍼를 제거하며 big-endian은 기존 의미를 유지합니다.
* FLAC MD5 작업 버퍼를 재사용합니다. Arm Rice 비용 계산은 한 번의 128-bit residual load로 주변 후보를 평가하고 64-bit로 누적합니다. 비용·선택과 PCM은 정확히 유지합니다.
* LUFS의 두 채널 필터를 f64 NEON으로 같이 계산합니다. scalar와 같은 곱셈·덧셈 순서, denormal 처리와 반올림을 유지하며 FMA/fast-math를 적용하지 않습니다.
* QC만 요청하면 지문용 mono downmix를 계산하지 않습니다. x86 필터는 per-frame 함수 호출의 회귀를 확인하고 정확한 계산을 meter 루프에 inline합니다. 기존 AVX2 FIR·PCM·LPC와 scalar fallback은 유지합니다.

`capabilities.cpu_features`는 사용 가능한 NEON/SVE/SVE2/SHA2/CRC/PMULL 또는 x86 AVX2/SHA/PCLMULQDQ를 표시합니다. `backend`는 선택한 DSP 커널입니다. available feature를 전부 사용했다는 의미는 아닙니다. SHA-256/IEEE CRC32 라이브러리는 자체 하드웨어 감지를 유지합니다. `--scalar`는 수치 DSP 커널을 바꾸며 baseline memory layout 연산과 해시 라이브러리의 감지를 제거하지 않습니다.

## 측정과 정확도

엔진 커밋 `0cc863097730cde47b5a03493ff3486724a7b526`를 [x86·Arm CI](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37188719223)에서 검증했습니다. 각 아키텍처에서 Rust 테스트 15개, [기준 비교](qualification.json) 596개, [정수 경계](codec-stress.json) 288개, [합성 EBU 기준](standards-synthesized.json) 20개를 통과했습니다. 총 904개 개발 기준 검사이며 공식 전체 EBU 인증은 아닙니다.

원본과 출력의 PCM 해시를 독립적으로 확인했습니다. 28 codec/규격 조합 및 121.2353초 음원의 fused QC·지문이 standalone 결과와 정확히 일치합니다. SIMD K-weighting은 4개 rate에서 60,000-frame noise/극값/silent-tail을 기존 scalar history와 bit 단위로 비교했습니다. Rice/PCM layout은 정수 극값과 vector tail을 별도 기준과 비교했습니다. 압축 레벨·truncation·손상 시 출력 게시 금지 검사를 유지합니다.

48kHz/24-bit stereo 합성 음원, page-cache 예열, QC/변환 단독은 3회, review는 5회 중앙값입니다. 같은 job에서 이전 커밋 `9dacd1e3b47906366b710dad9afd97fdd160d99c`와 새 코드를 같은 toolchain으로 빌드했습니다. CPU는 user+system, 처리 시간은 wall time, RAM은 측정 프로세스의 최대 RSS입니다. 별도 subprocess를 순차 측정할 때 CPU/time은 합산하고 RSS는 최대값을 사용합니다. 실행 간 다른 CPU의 수치를 개선률로 비교하지 않습니다.

### Arm — Neoverse N2, Graviton4 실측 아님

실제 CPU 식별자는 implementer 0x41, part 0xd49, revision 0입니다. [Linux의 CPU 식별 정의](https://github.com/torvalds/linux/blob/master/arch/arm64/include/asm/cputype.h)는 0xd49를 Neoverse N2, 0xd4f를 Neoverse V2로 구분합니다. 따라서 이 결과는 공통 Arm 커널의 검증이고 AWS Graviton4/V2의 최종 장비 측정은 아닙니다. NEON·SVE·SVE2·SHA2·CRC·PMULL feature가 감지되었습니다. 실제 제품명은 확인하지 않았습니다.

| 작업 | 입력 | CPU 시간 이전 → 현재 (초) | CPU 감소 | 처리 시간 이전 → 현재 (초) | 현재 최대 RSS (KiB) |
|---|---|---:|---:|---:|---:|
| QC 단독 · 120초 | WAV | 0.20 → 0.17 | 15.0% | 0.20 → 0.18 | 2784 |
| QC 단독 · 120초 | ALAC | 0.48 → 0.45 | 6.2% | 0.48 → 0.46 | 3040 |
| FLAC 변환 · 60초 | WAV | 0.32 → 0.29 | 9.4% | 0.34 → 0.31 | 3288 |
| FLAC 변환 · 60초 | ALAC | 0.46 → 0.43 | 6.5% | 0.48 → 0.45 | 3296 |
| 변환+QC · 120초 | WAV | 0.98 → 0.74 | 24.5% | 1.02 → 0.78 | 3424 |
| 변환+QC · 120초 | ALAC | 1.26 → 1.02 | 19.0% | 1.30 → 1.06 | 3296 |
| 변환+QC+지문 · 120초 | WAV | 1.04 → 0.80 | 23.1% | 1.09 → 0.84 | 5588 |
| 변환+QC+지문 · 120초 | ALAC | 1.32 → 1.08 | 18.2% | 1.37 → 1.12 | 5472 |

### x86 — AMD EPYC 7763, Zen3

| 작업 | 입력 | CPU 시간 이전 → 현재 (초) | CPU 감소 | 처리 시간 이전 → 현재 (초) | 현재 최대 RSS (KiB) |
|---|---|---:|---:|---:|---:|
| QC 단독 · 120초 | WAV | 0.24 → 0.24 | 0.0% | 0.25 → 0.24 | 3616 |
| QC 단독 · 120초 | ALAC | 0.58 → 0.56 | 3.4% | 0.59 → 0.56 | 3776 |
| FLAC 변환 · 60초 | WAV | 0.36 → 0.34 | 5.6% | 0.37 → 0.35 | 3924 |
| FLAC 변환 · 60초 | ALAC | 0.54 → 0.51 | 5.6% | 0.55 → 0.51 | 3920 |
| 변환+QC · 120초 | WAV | 1.19 → 0.92 | 22.7% | 1.20 → 0.92 | 4164 |
| 변환+QC · 120초 | ALAC | 1.54 → 1.24 | 19.5% | 1.54 → 1.24 | 4032 |
| 변환+QC+지문 · 120초 | WAV | 1.25 → 0.98 | 21.6% | 1.27 → 0.99 | 6100 |
| 변환+QC+지문 · 120초 | ALAC | 1.59 → 1.31 | 17.6% | 1.61 → 1.32 | 6188 |

x86 QC 단독의 WAV CPU 시간은 동일했고 ALAC은 약 3.4% 줄었습니다. Arm 우선 변경 후에도 x86의 재측정과 전체 기능 검사를 수행했습니다. [Arm 원시 결과](benchmark-arm-review.json)와 [x86 원시 결과](benchmark-x86-review.json)는 여섯 codec의 분석·지문·변환 및 WAV/ALAC review, 명령·fixture/binary 해시·전체 반복값·실제 CPU를 기록합니다.

### FFmpeg 비교와 한계

Arm의 변환+QC는 native/FFmpeg CPU 시간이 WAV 0.74/1.98초, ALAC 1.02/1.95초였고 처리 시간은 0.78/1.68초, 1.06/1.68초였습니다. 지문을 포함하면 CPU는 0.80/2.18초, 1.08/2.04초였습니다. 최대 RSS는 review에서 native 약 3.2~3.3MiB, FFmpeg 약 59MiB였고, 지문을 포함하면 native 약 5.3~5.5MiB, FFmpeg 약 59MiB였습니다.

FFmpeg도 한 번의 입력 decode에서 FLAC·입력 hash·ebur128/True Peak를 계산하고 출력 검증을 따로 수행합니다. 지문을 포함하면 mono 11025Hz s16 tap도 계산하지만 창 보관·JSON 비용은 제외합니다. native의 clipping/silence/zero-crossing 같은 추가 QC 지표도 FFmpeg 기준에 포함하지 않습니다. 이 측정은 전체 AUDENIQ backend pipeline의 수치가 아닙니다.

**Arm의 ALAC → FLAC 변환만 실행한 경우 처리 시간은 native 0.45초, FFmpeg 0.43초로 아직 조금 더 깁니다.** native CPU는 0.43/0.56초, 최대 RSS는 3296/54748KiB였습니다. 변환+QC의 개선을 모든 변환 상황의 개선으로 일반화하지 않습니다. 낮은 소요 시간과 GNU time의 0.01초 해상도 때문에 작은 차이를 큰 효과로 주장하지 않습니다.

지문을 함께 계산하면 encoding/QC 상태가 동시에 살아 있으므로 이전의 순차 실행보다 최대 RSS가 늘 수 있습니다. 이번 Arm 측정에서는 WAV 5344→5588KiB, ALAC 5344→5472KiB였습니다. CPU를 줄이면서 모든 경우에 이전보다 RAM도 줄었다고 주장하지 않습니다. 파일별 작업 버퍼는 제한을 유지합니다.

압축 설정/출력 bytes도 원시 결과에 있습니다. 동일 host/fixture에서 이전·현재 native의 FLAC 크기는 같았고 PCM hash도 같았습니다. FFmpeg와 자체 level 5의 설정과 압축률은 같다고 가정하지 않습니다.

## 실제 Graviton4에서 재현

Graviton4 arm64 Linux에서 아래처럼 실행합니다. FFmpeg와 GNU time은 개발용 비교 도구에만 필요합니다.

```sh
cargo build --workspace --release --locked
target/release/audeniq-qc capabilities
target/release/audeniq-qc-tools qualify --output qualification.json
target/release/audeniq-qc-tools standards --output standards.json
target/release/audeniq-qc-tools codec-stress --output codec-stress.json
target/release/audeniq-qc-tools benchmark --seconds 240 --repeats 7 --output qc.json
target/release/audeniq-qc-tools benchmark-convert --seconds 240 --repeats 7 --output conversion.json
target/release/audeniq-qc-tools benchmark-review --seconds 240 --repeats 7 --fingerprint --output review.json
```

같은 장비의 이전 엔진과 비교하려면 커밋 `9dacd1e3b47906366b710dad9afd97fdd160d99c`를 별도 worktree/target directory에서 빌드하고 각 benchmark에 `--baseline-binary /absolute/path/to/old/audeniq-qc`를 추가합니다. baseline/current의 CPU 식별자, fixture SHA-256, binary SHA-256과 전체 반복값을 기록합니다. review benchmark는 이전 엔진의 convert + 출력 analyze를 순차 측정하고, 새 엔진은 fused command를 측정하며 QC·지문 결과의 정확한 일치를 확인합니다.

최적화는 기본 portable 빌드에서도 작동합니다. Graviton4 전용 배포라면 별도 디렉터리에서 아래 빌드의 추가 효과도 비교할 수 있습니다. LLVM의 Neoverse V2 instruction scheduling을 요청하는 보조 설정이며, 위 SIMD·복사·중복 작업 개선의 대체물이 아닙니다. 이 전용 빌드의 성능 수치는 아직 측정하지 않았습니다.

```sh
RUSTFLAGS="-C target-cpu=neoverse-v2" CARGO_TARGET_DIR=target/graviton4 cargo build --workspace --release --locked
```

[AWS의 Rust 가이드](https://aws.github.io/graviton/rust.html)는 arm64의 기본 outline atomics와 Graviton의 LSE 지원을 설명합니다. 이 엔진의 공유 atomic은 파일 이름 카운터 정도이며, mutex/atomic 플래그 변경으로 큰 성능 향상을 주장하지 않습니다. 각 파일은 한 프로세스에서 처리하며 파일 단위 병렬성은 나중의 워커가 제한합니다. 실제 워커 수는 배포 장비에서 처리량, p95 지연과 메모리를 함께 측정해 정해야 합니다.

전체 공식 EBU 파일, 실제 정상/손상 음악 코퍼스, fuzzing, 실제 Graviton4에서 장시간·동시 처리 검증은 남아 있습니다. 기존 [검증 경계](VALIDATION.md)의 1~2채널·16/24-bit·44100~192000Hz 및 지원 codec 조건도 유지합니다. 처리 후 정책 PASS/FAIL 판정과 backend sandbox는 엔진 외부의 통합 단계입니다.
