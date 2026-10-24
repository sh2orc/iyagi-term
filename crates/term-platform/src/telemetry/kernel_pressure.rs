//! 커널 메모리 압력 신호(자원 거버넌스 계획 P2-4).
//!
//! 페이지 비율로 임계를 정하는 규칙은 macOS에서 거의 쓸모가 없다 — "가용"
//! 페이지 수(active+inactive+free)는 압축·스왑이 이미 일하는 중에도 넉넉하게
//! 나온다. 반대로 커널의 판정(jetsam이 죽일 기준)은 압축·스왑이 버티는지를
//! 직접 본다. 그래서 이 모듈이 읽는 커널 값이 압력의 1차 신호이고, 데몬은
//! 이 값을 [`term_core::pressure::PressureTracker::update_with_kernel`]에
//! 그대로 넘긴다:
//!
//! * critical — 즉시 CRITICAL, 신호가 남아 있는 동안 회복 없음.
//! * warn — 흔하므로 10초 지속될 때만 WARNING으로 본다(추적기가 센다).
//! * 읽을 수 없거나(권한·커널 미지원) 파싱에 실패하면 `Unknown`(모름은
//!   행동하지 않음 — 관측 불가는 0이 아니다).
//!
//! 플랫폼: macOS는 `sysctl kern.memorystatus_vm_pressure_level`
//! (1=normal, 2=warn, 4=critical), Linux는 `/proc/pressure/memory`의
//! PSI(critical: `some avg10` > 20 또는 `full avg10` > 5, warn: `some avg10`
//! > 10), Windows는 미지원(`CreateMemoryResourceNotification`이 붙을 자리).

use term_core::KernelMemorySignal;

/// macOS `kern.memorystatus_vm_pressure_level` 값의 판정. 4 이상은
/// critical(그보다 큰 값이 추가돼도 위기로 본다), 2·3은 warn, 1은 normal,
/// 그 밖(읽기 실패 계열의 0 등)은 모름.
#[cfg(any(target_os = "macos", test))]
fn memorystatus_signal(level: i32) -> KernelMemorySignal {
    match level {
        level if level >= 4 => KernelMemorySignal::Critical,
        2 | 3 => KernelMemorySignal::Warn,
        1 => KernelMemorySignal::Normal,
        _ => KernelMemorySignal::Unknown,
    }
}

/// PSI(`/proc/pressure/memory`) 전문의 판정. `some avg10 > 20` 또는
/// `full avg10 > 5`면 critical, `some avg10 > 10`이면 warn. 두 줄 모두
/// 찾지 못하거나 값이 숫자가 아니면 모름.
#[cfg(any(target_os = "linux", test))]
fn psi_signal(text: &str) -> KernelMemorySignal {
    let avg10 = |line_kind: &str| -> Option<f64> {
        let line = text.lines().find(|l| l.starts_with(line_kind))?;
        let field = line.split_whitespace().find(|f| f.starts_with("avg10="))?;
        field.strip_prefix("avg10=")?.parse::<f64>().ok()
    };
    let (Some(some), Some(full)) = (avg10("some"), avg10("full")) else {
        return KernelMemorySignal::Unknown;
    };
    if some > 20.0 || full > 5.0 {
        KernelMemorySignal::Critical
    } else if some > 10.0 {
        KernelMemorySignal::Warn
    } else {
        KernelMemorySignal::Normal
    }
}

/// 커널의 메모리 압력 판정. 매 틱(1초)에 불러도 싸다 — macOS는 sysctl
/// 트랩 하나, Linux는 작은 파일 하나를 읽는다.
#[cfg(target_os = "macos")]
pub fn kernel_memory_pressure() -> KernelMemorySignal {
    const NAME: &[u8] = b"kern.memorystatus_vm_pressure_level\0";
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // 안전성: 출력 버퍼의 크기를 함께 넘기는 표준 sysctl 호출이다.
    let read = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr().cast(),
            &mut value as *mut libc::c_int as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read == 0 {
        memorystatus_signal(value)
    } else {
        KernelMemorySignal::Unknown
    }
}

/// 커널의 메모리 압력 판정 — Linux PSI.
#[cfg(target_os = "linux")]
pub fn kernel_memory_pressure() -> KernelMemorySignal {
    std::fs::read_to_string("/proc/pressure/memory")
        .map(|text| psi_signal(&text))
        .unwrap_or(KernelMemorySignal::Unknown)
}

/// 커널 압력 신호를 읽을 수 없는 플랫폼(Windows): 항상 모름.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn kernel_memory_pressure() -> KernelMemorySignal {
    KernelMemorySignal::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memorystatus_level_maps_to_signals() {
        assert_eq!(memorystatus_signal(1), KernelMemorySignal::Normal);
        assert_eq!(memorystatus_signal(2), KernelMemorySignal::Warn);
        assert_eq!(
            memorystatus_signal(3),
            KernelMemorySignal::Warn,
            "정의되지 않은 중간 값은 warn으로 본다(critical로 올리지 않는다)"
        );
        assert_eq!(memorystatus_signal(4), KernelMemorySignal::Critical);
        assert_eq!(
            memorystatus_signal(8),
            KernelMemorySignal::Critical,
            "그보다 큰 값도 위기로 본다"
        );
        assert_eq!(
            memorystatus_signal(0),
            KernelMemorySignal::Unknown,
            "읽기 실패 계열의 0"
        );
    }

    #[test]
    fn psi_thresholds_classify_over_the_lines_only() {
        let quiet = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n\
                     full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_signal(quiet), KernelMemorySignal::Normal);
        // some avg10 > 10: warn.
        let busy = "some avg10=10.01 avg60=4.00 avg300=1.00 total=12345\n\
                    full avg10=0.10 avg60=0.05 avg300=0.01 total=100\n";
        assert_eq!(psi_signal(busy), KernelMemorySignal::Warn);
        // some avg10 > 20.
        let some_stall = "some avg10=20.01 avg60=9.00 avg300=1.00 total=12345\n\
                          full avg10=0.10 avg60=0.05 avg300=0.01 total=100\n";
        assert_eq!(psi_signal(some_stall), KernelMemorySignal::Critical);
        // full avg10 > 5.
        let full_stall = "some avg10=1.00 avg60=9.00 avg300=1.00 total=12345\n\
                          full avg10=5.01 avg60=2.00 avg300=0.50 total=999\n";
        assert_eq!(psi_signal(full_stall), KernelMemorySignal::Critical);
        // 경계값 자체는 그 단계가 아니다(초과일 때만).
        let boundary = "some avg10=20.00 avg60=0.00 avg300=0.00 total=0\n\
                        full avg10=5.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_signal(boundary), KernelMemorySignal::Warn);
        let warn_boundary = "some avg10=10.00 avg60=0.00 avg300=0.00 total=0\n\
                             full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_signal(warn_boundary), KernelMemorySignal::Normal);
    }

    #[test]
    fn psi_parsing_failure_is_unknown() {
        assert_eq!(psi_signal(""), KernelMemorySignal::Unknown);
        assert_eq!(psi_signal("garbage"), KernelMemorySignal::Unknown);
        // full 줄이 없으면 판정할 수 없다.
        let no_full = "some avg10=90.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_signal(no_full), KernelMemorySignal::Unknown);
        // avg10이 숫자가 아니면 그 줄은 모름.
        let bad_number = "some avg10=x avg60=0 avg300=0 total=0\n\
                          full avg10=0 avg60=0 avg300=0 total=0\n";
        assert_eq!(psi_signal(bad_number), KernelMemorySignal::Unknown);
    }

    /// 실제 커널을 읽어 패닉치지 않는지(값 자체는 기계 상태라 못 박지
    /// 않는다). 지원 플랫폼에서만 돈다.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn live_kernel_signal_reads_without_panicking() {
        let _ = kernel_memory_pressure();
    }
}
