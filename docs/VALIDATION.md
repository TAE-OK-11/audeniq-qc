# 검증과 성능 측정

검증한 엔진 커밋: `df7e6d57f5d059f95ce1824ee31b5858867b02be`.
[최종 x86·Arm CI](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37185830208)에서 빌드·형식 검사·Clippy·전체 테스트·개발용 기준 비교·성능 측정을 모두 통과했습니다. 엔진과 검증·측정 도구의 소스는 Rust이며, 런타임 FFmpeg 의존성과 백엔드 연결은 없습니다.

## 구현한 범위

WAV/RF64/BW64, AIFF/AIFC, FLAC, M4A ALAC, 정수 무손실 WavPack, TTA1의 probe·디코딩·규격/길이 확인·구조/무결성 검사·PCM SHA-256을 지원합니다. 한 번의 디코딩으로 integrated LUFS, True Peak, sample peak, clipping, silence, block energy, zero crossing과 선택적인 지문 입력을 계산합니다. 지문 입력은 11025Hz mono s16이며 연속 필터 상태로 head/middle/tail 합계 최대 90초만 보관합니다.

입력은 1~2채널, 16/24-bit 정수, 44100~192000Hz입니다. FLAC 변환은 모든 지원 오디오 입력에서 가능하며, WAV와 raw s32le 출력도 제공합니다. 원본과 출력의 PCM 동일성을 검증한 뒤 기존 파일을 덮어쓰지 않고 결과를 게시합니다. 압축 레벨 0~8은 자체 무손실 프리셋이며 FFmpeg의 번호와 동등한 설정을 뜻하지 않습니다. 기본 FLAC 입력은 전체 검증 후 오디오 프레임을 보존하고, 레벨을 명시하면 다시 인코딩합니다.

WAV·AIFF처럼 원래 PCM 내용의 체크섬이 없는 형식은 구조·길이·디코딩 가능성을 검사합니다. 유효한 PCM 값을 다른 값으로 바꾼 변조는 기준 해시 없이 구분할 수 없습니다. 변환 전후 해시 일치는 변환의 무손실성을 검증합니다.

## 통과한 검사

각 아키텍처에서 아래 검사를 수행했습니다. 두 아키텍처에서 반복한 검사를 새로운 서로 다른 테스트로 합산하지 않습니다.

| 검사 | 통과 | 내용 |
|---|---:|---|
| Rust 단위·회귀 테스트 | 12 | SIMD/기준 구현 동일성, 비트 읽기/쓰기 경계, codec/hash, no-clobber, 기한, 손상 시 게시 금지, PCM 출력·규격, 지문 재현성 |
| [기준 비교](qualification.json) | 538 | 28 codec/규격 조합, PCM export, 레벨 0~8 및 명시적 FLAC 재인코딩, 손상/잘림, 태그/커버, fractional tail·anti-aliasing |
| [정수 경계 검사](codec-stress.json) | 288 | 16/24-bit × mono/stereo × 8 패턴, FLAC/WavPack 압축 모드·ALAC/TTA, 마지막 1-frame 블록, 독립 생성 PCM 해시 |
| [합성 EBU 기준](standards-synthesized.json) | 20 | integrated LUFS 5개, True Peak 5개 × 3 rates |

개발 비교에서 FFmpeg를 별도 기준으로 사용합니다. engine CLI는 FFmpeg를 실행하지 않습니다. 압축 프로필과 PCM 출력의 검증을 포함한 개발 도구도 별도 Rust 패키지 `audeniq-qc-tools`입니다.

LUFS 비교 허용차는 0.11 LU, 합성 EBU integrated 기준은 ±0.1 LU입니다. True Peak 비교는 테스트별 공개 허용차를 적용합니다. 전체 대역 백색 잡음의 FFmpeg True Peak 차이는 기록하며 일치한다고 주장하지 않습니다. 공식 EBU ZIP을 사용한 전체 인증이 아니고, cases 6~14와 transient peak cases 20~23은 검증하지 않았습니다. 지문 resampler도 FFmpeg 기본값과 비트 단위로 같지 않으므로 별도의 알고리즘 버전이 필요합니다.

## 최신 분석 측정

120초, 48kHz/24-bit stereo 합성 음원, 캐시 예열 후 3회 중앙값입니다. 양쪽 모두 디코딩·LUFS/True Peak·canonical s32le SHA-256을 수행합니다. native는 추가 QC 지표도 계산합니다. CPU 시간은 user+system, RAM은 측정 대상 프로세스의 최대 RSS입니다. 측정 도구의 메모리는 제외합니다.

### x86 — AMD EPYC 9V45 96-Core Processor

| 입력 | 시간 native / FFmpeg (초) | CPU 시간 감소 | 최대 RSS 감소 |
|---|---:|---:|---:|
| WAV | 0.13 / 0.57 | 83.3% | 93.3% |
| FLAC | 0.27 / 0.46 | 55.9% | 89.0% |
| AIFF | 0.13 / 0.59 | 83.8% | 93.2% |
| ALAC | 0.32 / 0.62 | 56.8% | 93.1% |
| WavPack | 0.35 / 0.52 | 42.6% | 93.5% |
| TTA | 0.32 / 0.52 | 47.5% | 92.5% |

### Arm — implementer 0x41, part 0xd49, revision 0

| 입력 | 시간 native / FFmpeg (초) | CPU 시간 감소 | 최대 RSS 감소 |
|---|---:|---:|---:|
| WAV | 0.20 / 1.21 | 85.3% | 94.7% |
| FLAC | 0.34 / 1.05 | 72.3% | 90.4% |
| AIFF | 0.20 / 1.21 | 85.3% | 94.7% |
| ALAC | 0.48 / 1.24 | 64.7% | 94.4% |
| WavPack | 0.48 / 1.12 | 61.8% | 94.3% |
| TTA | 0.45 / 1.18 | 65.1% | 93.3% |

Arm의 실제 제품명/세대는 확인되지 않아 요청한 Arm 4세대 장비로 표시하지 않습니다. [x86 원시 결과](benchmark-ci-x86.json)와 [Arm 원시 결과](benchmark-ci-arm.json)에는 지문 입력을 포함한 분석의 측정도 있습니다. 해당 측정은 양쪽 모두 mono downmix·resample을 수행합니다. native는 최대 90초 창을 유지하고 JSON을 직렬화하며, FFmpeg는 연속 raw tap을 버리고 backend의 창 보관 비용을 포함하지 않습니다.

### EPYC Zen3에서 수행한 이전 실행

[EPYC 7763 원시 결과](benchmark-zen3-7763.json)는 커밋 `f4ca93da29ad4f07fcdf91964f975db7a71d323b`의 실제 AVX2 실행입니다. 이 기록은 WAV/FLAC/ALAC 3개 입력에서 분석·지문·변환을 측정한 이전 구현입니다. 추가 PCM 출력·압축 프로필·후속 최적화까지 같은 장비에서 모두 측정했다고 주장하지 않습니다. AMD는 [EPYC 7763을 Zen3 기반 EPYC 7003 제품군으로 설명](https://www.amd.com/en/newsroom/press-releases/2021-3-15-amd-epyc-7003-series-cpus-set-new-standard-as-hig.html)합니다. GitHub 러너의 CPU는 실행마다 바뀌므로 서로 다른 CPU의 실행 간 수치를 코드 개선률로 비교하지 않습니다.

## 최신 검증한 FLAC 변환 측정

60초, 48kHz/24-bit stereo 합성 음원, 3회 중앙값입니다. 양쪽 모두 입력 decode/hash와 FLAC 출력, 출력 재decode/hash를 측정합니다. native에는 fsync와 no-clobber 게시도 포함합니다. FFmpeg는 level 5로 재인코딩하며 별도 검증 프로세스의 CPU·시간을 합산하고 최대 RSS를 사용합니다. native 결과의 추가 독립 FFmpeg 검증은 측정 밖에서 수행합니다.

### x86

| 입력 | 시간 native / FFmpeg (초) | CPU 시간 감소 | 최대 RSS 감소 |
|---|---:|---:|---:|
| WAV | 0.25 / 0.32 | 52.1% | 93.0% |
| FLAC | 0.19 / 0.26 | 55.3% | 89.4% |
| AIFF | 0.28 / 0.33 | 45.8% | 92.8% |
| ALAC | 0.36 / 0.35 | 29.8% | 92.7% |
| WavPack | 0.37 / 0.29 | 10.3% | 92.7% |
| TTA | 0.35 / 0.29 | 15.4% | 91.6% |

### Arm

| 입력 | 시간 native / FFmpeg (초) | CPU 시간 감소 | 최대 RSS 감소 |
|---|---:|---:|---:|
| WAV | 0.35 / 0.41 | 40.7% | 93.7% |
| FLAC | 0.22 / 0.33 | 58.7% | 89.9% |
| AIFF | 0.35 / 0.41 | 40.7% | 93.7% |
| ALAC | 0.49 / 0.42 | 16.4% | 93.7% |
| WavPack | 0.49 / 0.37 | 4.2% | 93.4% |
| TTA | 0.47 / 0.39 | 13.5% | 92.5% |

분석의 이득을 변환의 이득으로 일반화할 수 없습니다. ALAC/WavPack/TTA 변환은 CPU·RAM이 줄어도 일부 조건에서 처리 시간이 더 깁니다. 특히 Arm WavPack 변환의 CPU 차이는 작으며 3회·0.01초 해상도의 측정으로 큰 개선을 주장할 수 없습니다. native는 한 프로세스에서 검증과 동기화까지 수행하고 FFmpeg의 출력 분기는 내부적으로 병렬 처리될 수 있으므로 CPU 시간과 wall time을 따로 봐야 합니다.

현재 합성 음원의 재인코딩 결과는 native 8,106,960 bytes입니다. FFmpeg는 x86에서 8,071,996 bytes, Arm에서 8,080,916 bytes로, native 출력이 각각 약 0.43%, 0.32% 큽니다. FLAC 프레임 보존은 태그를 제거하므로 출력 크기가 다릅니다. 크기와 실제 PCM 해시를 원시 결과에 기록했으며 같은 압축률을 가정하지 않습니다.

## 적용한 최적화와 남은 검증

* 필요한 codec/container와 JPEG·PNG만 빌드하고, 디코딩과 QC·hash·선택적 지문을 한 흐름에서 처리합니다.
* AVX2/NEON PCM unpack·FIR·f64 autocorrelation·정수 LPC 예측, RustCrypto SHA-256과 IEEE CRC32 하드웨어 선택을 사용합니다. 전역 target-cpu나 LTO 조절에 의존하지 않습니다.
* 비트 입력을 64-bit cache로 읽고 unary run을 묶어서 처리합니다. FLAC Rice 출력도 단어 단위로 묶어 씁니다.
* 레벨 4~5의 LPC 후보는 최대 128개의 정수 잔차로 먼저 평가하고, 선택한 후보의 전체 잔차를 정확히 계산합니다. 레벨 6~8은 전체 후보 평가를 유지합니다. 모든 방식에서 PCM을 재검증합니다.
* 검증한 FLAC은 불필요한 재압축을 생략합니다. 지문을 포함한 JSON은 샘플별 Value 객체를 만들지 않고 직접 직렬화합니다.

전체 공식 EBU 데이터, 실제 정상/손상 음악 코퍼스, 장시간·동시 작업의 p95와 메모리, parser fuzzing 및 실제 배포 장비/요청한 Arm 장비의 재측정은 남아 있습니다. 이 결과는 명시한 테스트·합성 음원·CPU·FFmpeg 6.1.1에 대한 증거이며, 모든 입력에서 더 빠르거나 FFmpeg보다 더 정확하다는 보장은 아닙니다. 백엔드 연결과 sandbox smoke test는 사용자가 나중에 수행할 통합 단계입니다.
