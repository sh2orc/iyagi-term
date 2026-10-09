//! 한/영 강제 전환(Shift+Space) — iyagi 고유 기능의 OS 쪽 절반.
//!
//! 웹뷰(JS)는 OS 입력 소스를 바꿀 수 없으므로 앱 프로세스가 OS API를 부른다:
//! - macOS: Carbon Text Input Sources(TIS). 활성화된 한국어 입력 소스와
//!   마지막으로 쓰던 영문 자판(없으면 ABC/US)을 오간다. 이 Mac에서 실증.
//! - Windows: IMM32 변환 모드(`IME_CMODE_NATIVE`)를 설정하고, 자판이 한국어가
//!   아니면 먼저 `0x0412` 배열을 활성화한다. IMM이 거부하면(TSF 전용 IME)
//!   한/영 키(`VK_HANGUL`)를 합성한다. **macOS 호스트에서 컴파일만 검증됐다.**
//! - 그 외(Linux): `available: false` + 이유.
//!
//! 모르는 상태는 `None` + reason이지 추측이 아니다(명세 불변식 6). OS 호출은
//! UI 스레드 친화적이라(TIS/IMM) 메인 스레드에서 실행하고 짧게 기다린다.

use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use term_contracts::error::{ErrorCode, RpcError};

/// 프론트에 보내는 입력기 상태. `hangul`/`source_id`는 모르면 `None`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImeState {
    /// 이 OS에서 강제 전환이 가능한가(한국어 입력 소스가 있는가).
    pub available: bool,
    /// 현재 한글 모드인가. 읽을 수 없으면 `None`.
    pub hangul: Option<bool>,
    /// 현재 입력 소스 식별자(macOS TIS ID / Windows HKL+모드).
    pub source_id: Option<String>,
    /// 불가·실패·미상의 이유.
    pub reason: Option<String>,
}

impl ImeState {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            hangul: None,
            source_id: None,
            reason: Some(reason.into()),
        }
    }
}

const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(2);

/// OS 입력기 호출을 메인 스레드에서 실행하고 결과를 기다린다. 커맨드는
/// `async`라 워커에서 돌므로 여기서 블록해도 이벤트 루프를 막지 않는다.
fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RpcError> {
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| {
        RpcError::new(
            ErrorCode::InvalidState,
            format!("main thread unavailable: {e}"),
        )
    })?;
    rx.recv_timeout(MAIN_THREAD_TIMEOUT)
        .map_err(|_| RpcError::new(ErrorCode::Busy, "input source call timed out"))
}

/// 현재 입력기 상태(읽기 전용).
#[tauri::command]
pub async fn ime_state(app: AppHandle) -> Result<ImeState, RpcError> {
    on_main_thread(&app, platform::state)
}

/// 한글↔영문 토글. 결과는 전환 후 다시 읽은 상태다.
#[tauri::command]
pub async fn ime_toggle_hangul(app: AppHandle) -> Result<ImeState, RpcError> {
    on_main_thread(&app, || platform::set_hangul(None))
}

/// 한글(`on=true`) 또는 영문으로 강제 설정.
#[tauri::command]
pub async fn ime_set_hangul(app: AppHandle, on: bool) -> Result<ImeState, RpcError> {
    on_main_thread(&app, move || platform::set_hangul(Some(on)))
}

// ---------------------------------------------------------------- pure logic

/// 한국어 입력 소스/입력 모드 식별자 판정. Apple(`com.apple.inputmethod.Korean.
/// 2SetKorean` 등 모든 모드)과 서드파티(구름 `…Gureum.han2`, 한글 계열)를 덮는다.
#[cfg(any(target_os = "macos", test))]
pub fn is_korean_source_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    lower.contains("korean") || lower.contains("hangul") || lower.contains(".gureum.")
}

/// 영문 목표: 마지막으로 떠났던 비한국어 소스(아직 켜져 있으면) → ABC → US →
/// 아무 `com.apple.keylayout.*` → 그 외 첫 비한국어 소스.
#[cfg(any(target_os = "macos", test))]
pub fn pick_english_source<'a>(
    enabled: &'a [String],
    last_non_korean: Option<&str>,
) -> Option<&'a str> {
    let non_korean = || enabled.iter().filter(|id| !is_korean_source_id(id));
    if let Some(last) = last_non_korean {
        if let Some(found) = non_korean().find(|id| id.as_str() == last) {
            return Some(found.as_str());
        }
    }
    for preferred in ["com.apple.keylayout.ABC", "com.apple.keylayout.US"] {
        if let Some(found) = non_korean().find(|id| id.as_str() == preferred) {
            return Some(found.as_str());
        }
    }
    non_korean()
        .find(|id| id.starts_with("com.apple.keylayout."))
        .or_else(|| non_korean().next())
        .map(String::as_str)
}

/// 한글 목표: 마지막으로 쓰던 한국어 소스(아직 켜져 있으면) → 두벌식 모드 →
/// 입력 모드(`…Korean.<mode>`) → 아무 한국어 소스.
#[cfg(any(target_os = "macos", test))]
pub fn pick_korean_source<'a>(enabled: &'a [String], last_korean: Option<&str>) -> Option<&'a str> {
    let korean = || enabled.iter().filter(|id| is_korean_source_id(id));
    if let Some(last) = last_korean {
        if let Some(found) = korean().find(|id| id.as_str() == last) {
            return Some(found.as_str());
        }
    }
    korean()
        .find(|id| id.ends_with("2SetKorean"))
        .or_else(|| korean().find(|id| id.contains(".Korean.")))
        .or_else(|| korean().next())
        .map(String::as_str)
}

// -------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::{CFBoolean, CFBooleanRef};
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::string::{CFString, CFStringRef};

    use super::{is_korean_source_id, pick_english_source, pick_korean_source, ImeState};

    type TISInputSourceRef = *mut c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        static kTISPropertyInputSourceID: CFStringRef;
        static kTISPropertyInputSourceCategory: CFStringRef;
        static kTISCategoryKeyboardInputSource: CFStringRef;
        static kTISPropertyInputSourceIsEnabled: CFStringRef;
        static kTISPropertyInputSourceIsSelectCapable: CFStringRef;
        fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
        fn TISCreateInputSourceList(
            properties: CFDictionaryRef,
            include_all_installed: u8,
        ) -> CFArrayRef;
        fn TISGetInputSourceProperty(source: TISInputSourceRef, key: CFStringRef) -> *const c_void;
        fn TISSelectInputSource(source: TISInputSourceRef) -> i32;
    }

    /// 마지막으로 떠났던 소스 — 토글이 사용자의 자판(예: Dvorak)으로 돌아가게.
    static LAST_NON_KOREAN: Mutex<Option<String>> = Mutex::new(None);
    static LAST_KOREAN: Mutex<Option<String>> = Mutex::new(None);

    fn string_prop(source: TISInputSourceRef, key: CFStringRef) -> Option<String> {
        // Get rule: 소스가 소유하는 값을 빌린다(해제하지 않는다).
        let ptr = unsafe { TISGetInputSourceProperty(source, key) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { CFString::wrap_under_get_rule(ptr as CFStringRef) }.to_string())
    }

    fn bool_prop(source: TISInputSourceRef, key: CFStringRef) -> Option<bool> {
        let ptr = unsafe { TISGetInputSourceProperty(source, key) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { CFBoolean::wrap_under_get_rule(ptr as CFBooleanRef) }.into())
    }

    /// 활성화된 키보드 입력 소스(선택 가능한 것만). 반환된 배열이 각 소스를
    /// 살려 두므로 `refs`는 배열이 살아 있는 동안만 유효하다.
    fn enabled_sources() -> (
        Option<CFArray<*const c_void>>,
        Vec<(String, TISInputSourceRef)>,
    ) {
        let raw = unsafe { TISCreateInputSourceList(std::ptr::null(), 0) };
        if raw.is_null() {
            return (None, Vec::new());
        }
        let array = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(raw) };
        let mut out = Vec::new();
        for value in array.get_all_values() {
            let source = value as TISInputSourceRef;
            let category = string_prop(source, unsafe { kTISPropertyInputSourceCategory });
            let keyboard =
                unsafe { CFString::wrap_under_get_rule(kTISCategoryKeyboardInputSource) }
                    .to_string();
            if category.as_deref() != Some(keyboard.as_str()) {
                continue;
            }
            if bool_prop(source, unsafe { kTISPropertyInputSourceIsEnabled }) != Some(true)
                || bool_prop(source, unsafe { kTISPropertyInputSourceIsSelectCapable })
                    != Some(true)
            {
                continue;
            }
            if let Some(id) = string_prop(source, unsafe { kTISPropertyInputSourceID }) {
                out.push((id, source));
            }
        }
        (Some(array), out)
    }

    fn current_id() -> Option<String> {
        let raw = unsafe { TISCopyCurrentKeyboardInputSource() };
        if raw.is_null() {
            return None;
        }
        // Copy rule: 우리가 소유 — CFType이 drop 시 해제한다.
        let owned = unsafe { CFType::wrap_under_create_rule(raw as *const c_void) };
        let id = string_prop(raw, unsafe { kTISPropertyInputSourceID });
        drop(owned);
        id
    }

    fn state_from(current: Option<String>, ids: &[String]) -> ImeState {
        if !ids.iter().any(|id| is_korean_source_id(id)) {
            return ImeState {
                available: false,
                hangul: current.as_deref().map(is_korean_source_id),
                source_id: current,
                reason: Some("no Korean input source enabled in System Settings".into()),
            };
        }
        ImeState {
            available: true,
            hangul: current.as_deref().map(is_korean_source_id),
            reason: current
                .is_none()
                .then(|| "current input source unreadable".to_string()),
            source_id: current,
        }
    }

    pub(super) fn state() -> ImeState {
        let (keep, sources) = enabled_sources();
        if keep.is_none() {
            return ImeState::unavailable("TISCreateInputSourceList failed");
        }
        let ids: Vec<String> = sources.into_iter().map(|(id, _)| id).collect();
        state_from(current_id(), &ids)
    }

    fn remember(current: &str) {
        let slot = if is_korean_source_id(current) {
            &LAST_KOREAN
        } else {
            &LAST_NON_KOREAN
        };
        *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(current.to_string());
    }

    pub(super) fn set_hangul(target: Option<bool>) -> ImeState {
        let current = current_id();
        let (keep, sources) = enabled_sources();
        if keep.is_none() {
            return ImeState::unavailable("TISCreateInputSourceList failed");
        }
        let ids: Vec<String> = sources.iter().map(|(id, _)| id.clone()).collect();
        let before = state_from(current.clone(), &ids);
        if !before.available {
            return before;
        }
        let now_korean = before.hangul.unwrap_or(false);
        let want_korean = target.unwrap_or(!now_korean);
        if let Some(cur) = &current {
            remember(cur);
        }
        let pick = if want_korean {
            let last = LAST_KOREAN
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            pick_korean_source(&ids, last.as_deref()).map(str::to_string)
        } else {
            let last = LAST_NON_KOREAN
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            pick_english_source(&ids, last.as_deref()).map(str::to_string)
        };
        let Some(pick) = pick else {
            return ImeState {
                reason: Some(if want_korean {
                    "no Korean input source to switch to".into()
                } else {
                    "no non-Korean input source to switch to".into()
                }),
                ..before
            };
        };
        if current.as_deref() == Some(pick.as_str()) {
            return before;
        }
        let Some(&(_, source)) = sources.iter().find(|(id, _)| *id == pick) else {
            return ImeState {
                reason: Some("target input source vanished".into()),
                ..before
            };
        };
        let status = unsafe { TISSelectInputSource(source) };
        if status != 0 {
            return ImeState {
                reason: Some(format!("TISSelectInputSource failed (OSStatus {status})")),
                ..before
            };
        }
        // 전환 뒤 다시 읽는다 — 시스템이 거부했으면 그대로 드러난다.
        let after = state_from(current_id(), &ids);
        if after.hangul != Some(want_korean) {
            return ImeState {
                reason: Some(format!("input source did not change to {pick}")),
                ..after
            };
        }
        after
    }
}

// ------------------------------------------------------------------ Windows

/// **컴파일만 검증된 경로** — 호스트가 macOS라 실행 확인은 Windows에서 해야 한다.
#[cfg(windows)]
mod platform {
    use std::mem::size_of;

    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::Input::Ime::{
        ImmGetContext, ImmGetConversionStatus, ImmGetDefaultIMEWnd, ImmReleaseContext,
        ImmSetConversionStatus, IME_CMODE_NATIVE, IME_CONVERSION_MODE, IME_SENTENCE_MODE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ActivateKeyboardLayout, GetKeyboardLayout, LoadKeyboardLayoutW, SendInput, INPUT, INPUT_0,
        INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KLF_ACTIVATE,
        KLF_SETFORPROCESS, VK_HANGUL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SendMessageTimeoutW,
        GUITHREADINFO, SMTO_ABORTIFHUNG, SMTO_ERRORONEXIT, WM_IME_CONTROL,
    };

    use super::ImeState;

    /// 한국어(대한민국) 주 언어 ID — HKL의 하위 워드.
    const KOREAN_LANGID: u16 = 0x0412;

    fn langid(hkl_bits: usize) -> u16 {
        (hkl_bits & 0xFFFF) as u16
    }

    /// 전면 창의 포커스 자식(WebView2)과 그 스레드. 없으면 `None`.
    fn focus_window() -> Option<(HWND, u32)> {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_invalid() {
            return None;
        }
        let thread = unsafe { GetWindowThreadProcessId(foreground, None) };
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let focus = if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok()
            && !info.hwndFocus.is_invalid()
        {
            info.hwndFocus
        } else {
            foreground
        };
        Some((focus, unsafe { GetWindowThreadProcessId(focus, None) }))
    }

    fn layout_is_korean(thread: u32) -> bool {
        let hkl = unsafe { GetKeyboardLayout(thread) };
        langid(hkl.0 as usize) == KOREAN_LANGID
    }

    fn conversion_mode(hwnd: HWND) -> Option<IME_CONVERSION_MODE> {
        context_conversion_mode(hwnd).or_else(|| {
            // WebView2's focused HWND can belong to its renderer process. A HIMC
            // cannot be read across processes; ask that thread's default IME
            // window instead. Zero is a valid Latin mode, not a failed query.
            const IMC_GETCONVERSIONMODE: usize = 0x0001;
            let ime = unsafe { ImmGetDefaultIMEWnd(hwnd) };
            if ime.is_invalid() {
                return None;
            }
            let mut mode = 0usize;
            let sent = unsafe {
                SendMessageTimeoutW(
                    ime,
                    WM_IME_CONTROL,
                    WPARAM(IMC_GETCONVERSIONMODE),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                    100,
                    Some(&mut mode),
                )
            };
            (sent.0 != 0).then_some(IME_CONVERSION_MODE(mode as u32))
        })
    }

    fn context_conversion_mode(hwnd: HWND) -> Option<IME_CONVERSION_MODE> {
        let himc = unsafe { ImmGetContext(hwnd) };
        if himc.is_invalid() {
            return None;
        }
        let mut conversion = IME_CONVERSION_MODE(0);
        let mut sentence = IME_SENTENCE_MODE(0);
        let ok =
            unsafe { ImmGetConversionStatus(himc, Some(&mut conversion), Some(&mut sentence)) }
                .as_bool();
        let _ = unsafe { ImmReleaseContext(hwnd, himc) };
        ok.then_some(conversion)
    }

    fn set_conversion_native(hwnd: HWND, native: bool) -> bool {
        let himc = unsafe { ImmGetContext(hwnd) };
        if himc.is_invalid() {
            return false;
        }
        let mut conversion = IME_CONVERSION_MODE(0);
        let mut sentence = IME_SENTENCE_MODE(0);
        let mut ok =
            unsafe { ImmGetConversionStatus(himc, Some(&mut conversion), Some(&mut sentence)) }
                .as_bool();
        if ok {
            let next = if native {
                IME_CONVERSION_MODE(conversion.0 | IME_CMODE_NATIVE.0)
            } else {
                IME_CONVERSION_MODE(conversion.0 & !IME_CMODE_NATIVE.0)
            };
            ok = unsafe { ImmSetConversionStatus(himc, next, sentence) }.as_bool();
        }
        let _ = unsafe { ImmReleaseContext(hwnd, himc) };
        ok
    }

    /// 한/영 키 합성(IMM이 거부하는 TSF 전용 IME용 폴백).
    fn press_hangul_key() {
        let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_HANGUL,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
        let _ = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    }

    fn state_of(hwnd: HWND, thread: u32) -> ImeState {
        let korean_layout = layout_is_korean(thread);
        let mode = conversion_mode(hwnd);
        let hangul = if korean_layout {
            mode.map(|m| (m.0 & IME_CMODE_NATIVE.0) != 0)
        } else {
            Some(false)
        };
        ImeState {
            available: true,
            hangul,
            source_id: Some(format!(
                "hkl:{:04x}{}",
                langid(unsafe { GetKeyboardLayout(thread) }.0 as usize),
                mode.map(|m| format!(":cmode={:#x}", m.0))
                    .unwrap_or_default()
            )),
            reason: hangul
                .is_none()
                .then(|| "IME conversion status unreadable".to_string()),
        }
    }

    pub(super) fn state() -> ImeState {
        match focus_window() {
            Some((hwnd, thread)) => state_of(hwnd, thread),
            None => ImeState::unavailable("no foreground window"),
        }
    }

    pub(super) fn set_hangul(target: Option<bool>) -> ImeState {
        let Some((hwnd, thread)) = focus_window() else {
            return ImeState::unavailable("no foreground window");
        };
        let before = state_of(hwnd, thread);
        let want = target.unwrap_or(!before.hangul.unwrap_or(false));
        if want && !layout_is_korean(thread) {
            // 자판이 한국어가 아니면 먼저 한국어 배열로.
            match unsafe { LoadKeyboardLayoutW(w!("00000412"), KLF_ACTIVATE) } {
                Ok(hkl) => {
                    let _ = unsafe { ActivateKeyboardLayout(hkl, KLF_SETFORPROCESS) };
                }
                Err(e) => {
                    return ImeState {
                        reason: Some(format!("Korean keyboard layout unavailable: {e}")),
                        ..before
                    };
                }
            }
        }
        if !set_conversion_native(hwnd, want) {
            // TSF 전용 IME: 변환 모드 API가 거부하면 키 합성으로 토글한다.
            press_hangul_key();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let after = state_of(hwnd, thread);
        if after.hangul != Some(want) {
            return ImeState {
                reason: Some(format!(
                    "IME mode did not change (wanted {})",
                    if want { "hangul" } else { "latin" }
                )),
                ..after
            };
        }
        after
    }
}

// -------------------------------------------------------------- other OSes

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::ImeState;

    pub(super) fn state() -> ImeState {
        ImeState::unavailable("not supported on this platform")
    }

    pub(super) fn set_hangul(_target: Option<bool>) -> ImeState {
        ImeState::unavailable("not supported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn korean_source_ids_cover_apple_modes_and_third_parties() {
        for id in [
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.inputmethod.Korean.3SetKorean",
            "com.apple.inputmethod.Korean.390Sebulshik",
            "com.apple.inputmethod.Korean",
            "org.youknowone.inputmethod.Gureum.han2",
            "kr.hangul.some.ime",
        ] {
            assert!(is_korean_source_id(id), "{id}");
        }
        for id in [
            "com.apple.keylayout.ABC",
            "com.apple.keylayout.US",
            "com.apple.inputmethod.SCIM.ITABC",
            "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese",
        ] {
            assert!(!is_korean_source_id(id), "{id}");
        }
    }

    #[test]
    fn english_pick_prefers_last_then_abc_then_us_then_any_layout() {
        let enabled = ids(&[
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.keylayout.Dvorak",
            "com.apple.keylayout.US",
            "com.apple.keylayout.ABC",
        ]);
        assert_eq!(
            pick_english_source(&enabled, Some("com.apple.keylayout.Dvorak")),
            Some("com.apple.keylayout.Dvorak")
        );
        // 마지막 소스가 꺼졌으면 ABC.
        assert_eq!(
            pick_english_source(&enabled, Some("com.apple.keylayout.Colemak")),
            Some("com.apple.keylayout.ABC")
        );
        assert_eq!(
            pick_english_source(&enabled, None),
            Some("com.apple.keylayout.ABC")
        );
        let no_abc = ids(&[
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.keylayout.US",
        ]);
        assert_eq!(
            pick_english_source(&no_abc, None),
            Some("com.apple.keylayout.US")
        );
        let only_korean = ids(&["com.apple.inputmethod.Korean.2SetKorean"]);
        assert_eq!(pick_english_source(&only_korean, None), None);
    }

    #[test]
    fn korean_pick_prefers_last_then_two_set_then_any_mode() {
        let enabled = ids(&[
            "com.apple.keylayout.ABC",
            "com.apple.inputmethod.Korean.3SetKorean",
            "com.apple.inputmethod.Korean.2SetKorean",
        ]);
        assert_eq!(
            pick_korean_source(&enabled, Some("com.apple.inputmethod.Korean.3SetKorean")),
            Some("com.apple.inputmethod.Korean.3SetKorean")
        );
        assert_eq!(
            pick_korean_source(&enabled, None),
            Some("com.apple.inputmethod.Korean.2SetKorean")
        );
        let gureum = ids(&[
            "com.apple.keylayout.ABC",
            "org.youknowone.inputmethod.Gureum.han2",
        ]);
        assert_eq!(
            pick_korean_source(&gureum, None),
            Some("org.youknowone.inputmethod.Gureum.han2")
        );
        assert_eq!(
            pick_korean_source(&ids(&["com.apple.keylayout.ABC"]), None),
            None
        );
    }

    #[test]
    fn unavailable_state_carries_a_reason_and_no_guess() {
        let state = ImeState::unavailable("why");
        assert!(!state.available);
        assert_eq!(state.hangul, None);
        assert_eq!(state.source_id, None);
        assert_eq!(state.reason.as_deref(), Some("why"));
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["sourceId"], serde_json::Value::Null);
    }

    /// 읽기 전용: 사용자의 입력 소스를 바꾸지 않는다.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reads_the_current_input_source_without_changing_it() {
        let before = platform::state();
        eprintln!(
            "current input source: {:?} (hangul={:?}, available={}, reason={:?})",
            before.source_id, before.hangul, before.available, before.reason
        );
        let again = platform::state();
        assert_eq!(before.source_id, again.source_id);
        // 한국어 소스가 켜져 있으면 available, 아니면 이유가 있어야 한다.
        assert!(before.available || before.reason.is_some());
    }

    /// 수동 확인용: 실제로 두 번 토글해 원래 소스로 돌아오는지 본다
    /// (`cargo test -p iyagi-app manual_round_trip -- --ignored --nocapture`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "changes the user's active input source; run manually"]
    fn manual_round_trip_toggles_twice() {
        let start = platform::state();
        if !start.available {
            eprintln!("skip: {:?}", start.reason);
            return;
        }
        let once = platform::set_hangul(None);
        eprintln!("after 1st toggle: {once:?}");
        assert_ne!(once.hangul, start.hangul);
        let twice = platform::set_hangul(None);
        eprintln!("after 2nd toggle: {twice:?}");
        assert_eq!(twice.hangul, start.hangul);
        assert_eq!(twice.source_id, start.source_id);
    }
}
