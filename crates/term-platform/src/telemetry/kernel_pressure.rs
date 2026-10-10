//! 커널 메모리 압력 신호(자원 거버넌스 계획 P2-4).
//!
//! 페이지 비율로 임계를 정하는 규칙은 macOS에서 거짓 양성을 잘 낸다 —
//! "가용" 페이지 수는 압축·스왑이 실제로 감당하는 범위와 느슨하게만
//! 상관한다(여유가 충분해도 커널이 warn을 반환하는 일이 흔하다). 반대로
//! 커널의 판정(jetsam이 죽일 기준)은 압축·스왑이 버티는지를 직접 본다.
//! 그래서 "실제 위기"의 1차 신호는 이 모듈이 읽는 커널 값이고, 데몬은
//! 이 값이 critical일 때 [`term_core::pressure::PressureTracker::force_critical`]
//! 로 즉시 CRITICAL를 올린다(회복은 여전히 10초 지속 게이트를 지난다).
//!
//! warn 단계는 무시한다 — 위기가 아닌데 CRITICAL를 올리면 셸은 물론
//! 대기열·가드까지 과잉 반응한다. 판정은 bool 하나로 좁히다: 읽을 수
//! 없거나(권한·커널 미지원) 파싱에 실패하면 `false`(모름은 행동하지
//! 않음 — 관측 불가는 0이 아니다).
//!
//! 플랫폼: macOS는 `sysctl kern.memorystatus_vm_pressure_level`
//! (1=normal, 2=warn, 4=critical), Linux는 `/proc/pressure/memory`의
//! PSI(`some avg10` > 20 또는 `full avg10` > 5), Windows는 미지원
//! (`CreateMemoryResourceNotification`이 붙을 자리).

/// macOS `kern.memorystatus_vm_pressure_level` 값의 위기 판정. 4가
/// critical이며, 그보다 큰 값이 추가돼도 위기로 본다.
#[cfg(any(target_os = "macos", test))]
fn memorystatus_is_critical(level: i32) -> bool {
    level >= 4
}

/// PSI(`/proc/pressure/memory`) 전문의 위기 판정. `some avg10 > 20` 또는
/// `full avg10 > 5`면 위기다. 두 줄 모두 찾지 못하거나 값이 숫자가 아니면
/// `None`(모름).
#[cfg(any(target_os = "linux", test))]
fn psi_is_critical(text: &str) -> Option<bool> {
    let avg10 = |line_kind: &str| -> Option<f64> {
        let line = text.lines().find(|l| l.starts_with(line_kind))?;
        let field = line.split_whitespace().find(|f| f.starts_with("avg10="))?;
        field.strip_prefix("avg10=")?.parse::<f64>().ok()
    };
    let some = avg10("some")?;
    let full = avg10("full")?;
    Some(some > 20.0 || full > 5.0)
}

/// 커널이 메모리 위기(critical)라고 판정했는가. 매 틱(1초)에 불러도
/// 싸다 — macOS는 sysctl 트랩 하나, Linux는 작은 파일 하나를 읽는다.
#[cfg(target_os = "macos")]
pub fn kernel_memory_critical() -> bool {
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
    read == 0 && memorystatus_is_critical(value)
}

/// 커널이 메모리 위기(critical)라고 판정했는가 — Linux PSI 판정.
#[cfg(target_os = "linux")]
pub fn kernel_memory_critical() -> bool {
    std::fs::read_to_string("/proc/pressure/memory")
        .ok()
        .as_deref()
        .and_then(psi_is_critical)
        .unwrap_or(false)
}

/// 커널 압력 신호를 읽을 수 없는 플랫폼(Windows): 항상 "위기 아님".
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn kernel_memory_critical() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memorystatus_level_maps_only_critical_and_above() {
        assert!(!memorystatus_is_critical(1), "normal");
        assert!(!memorystatus_is_critical(2), "warn은 위기가 아니다");
        assert!(
            !memorystatus_is_critical(3),
            "정의되지 않은 값은 보수적으로 무시"
        );
        assert!(memorystatus_is_critical(4), "critical");
        assert!(memorystatus_is_critical(8), "그보다 큰 값도 위기로 본다");
        assert!(!memorystatus_is_critical(0), "읽기 실패 계열의 0");
    }

    #[test]
    fn psi_thresholds_classify_over_the_lines_only() {
        let quiet = "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n\
                     full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_is_critical(quiet), Some(false));
        // some avg10 > 20.
        let some_stall = "some avg10=20.01 avg60=9.00 avg300=1.00 total=12345\n\
                          full avg10=0.10 avg60=0.05 avg300=0.01 total=100\n";
        assert_eq!(psi_is_critical(some_stall), Some(true));
        // full avg10 > 5.
        let full_stall = "some avg10=21.00 avg60=9.00 avg300=1.00 total=12345\n\
                          full avg10=5.01 avg60=2.00 avg300=0.50 total=999\n";
        assert_eq!(psi_is_critical(full_stall), Some(true));
        // 경계값 자체는 위기가 아니다(초과일 때만).
        let boundary = "some avg10=20.00 avg60=0.00 avg300=0.00 total=0\n\
                        full avg10=5.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_is_critical(boundary), Some(false));
    }

    #[test]
    fn psi_parsing_failure_is_unknown_not_false() {
        assert_eq!(psi_is_critical(""), None);
        assert_eq!(psi_is_critical("garbage"), None);
        // full 줄이 없으면 판정할 수 없다.
        let no_full = "some avg10=90.00 avg60=0.00 avg300=0.00 total=0\n";
        assert_eq!(psi_is_critical(no_full), None);
        // avg10이 숫자가 아니면 그 줄은 모름.
        let bad_number = "some avg10=x avg60=0 avg300=0 total=0\n\
                          full avg10=0 avg60=0 avg300=0 total=0\n";
        assert_eq!(psi_is_critical(bad_number), None);
    }

    /// 실제 커널을 읽어 패닉치지 않는지(값 자체는 기계 상태라 못 박지
    /// 않는다). 지원 플랫폼에서만 돈다.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn live_kernel_signal_reads_without_panicking() {
        let _ = kernel_memory_critical();
    }
}
