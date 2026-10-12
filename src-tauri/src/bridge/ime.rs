//! Shift+Space 강제 전환(CJK↔라틴) — iyagi 고유 기능의 OS 쪽 절반.
//!
//! 웹뷰(JS)는 OS 입력 소스를 바꿀 수 없으므로 앱 프로세스가 OS API를 부른다.
//! 대상은 한국어 고정이 아니라 사용자가 켜 둔 CJK 입력 소스(한국어·중국어·
//! 일본어)와 라틴 자판 사이 토글이다:
//! - macOS: Carbon Text Input Sources(TIS). 마지막으로 쓴 CJK 소스(계열 무관)와
//!   마지막으로 쓴 라틴 자판(없으면 ABC/US)을 오간다. 한국어는 이 Mac에서
//!   실증했고 중국어·일본어는 같은 TIS 경로다(실기 확인 절차는 07-korean-ime).
//! - Windows: IMM32 변환 모드(`IME_CMODE_NATIVE`)를 설정하고, 자판이 CJK가
//!   아니면 먼저 해당 언어 배열을 활성화한다. IMM이 거부하면(TSF 전용 IME)
//!   한/영 키(`VK_HANGUL`)를 합성한다 — 합성 키가 한국어 배열에서만 의미가
//!   있으므로 중국어·일본어는 실패 이유로 보고한다. **macOS 호스트에서
//!   컴파일만 검증됐다.**
//! - 그 외(Linux): `available: false` + 이유.
//!
//! 모르는 상태는 `None` + reason이지 추측이 아니다(명세 불변식 6). OS 호출은
//! UI 스레드 친화적이라(TIS/IMM) 메인 스레드에서 실행하고 짧게 기다린다.

use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use term_contracts::error::{ErrorCode, RpcError};

/// 세션의 CJK 계열 — HUD 글리프와 자모 복원 게이팅에 쓴다(serde: "ko"|"zh"|"ja").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ImeFamily {
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "zh")]
    Chinese,
    #[serde(rename = "ja")]
    Japanese,
}

/// 프론트에 보내는 입력기 상태. `native`/`family`/`source_id`는 모르면 `None`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImeState {
    /// 이 OS에서 강제 전환이 가능한가(CJK 입력 소스가 하나라도 켜져 있는가).
    pub available: bool,
    /// 현재 네이티브(CJK) 모드인가. 읽을 수 없으면 `None`.
    pub native: Option<bool>,
    /// 현재 소스의 계열. 라틴이면 다음 네이티브 목표의 계열(마지막으로 쓴
    /// CJK 소스, 없으면 목록 순서 첫 CJK). CJK가 없거나 모르면 `None`.
    pub family: Option<ImeFamily>,
    /// 현재 입력 소스 식별자(macOS TIS ID / Windows HKL+모드).
    pub source_id: Option<String>,
    /// 불가·실패·미상의 이유.
    pub reason: Option<String>,
}

impl ImeState {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            native: None,
            family: None,
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

/// 네이티브(CJK)↔라틴 토글. 결과는 전환 후 다시 읽은 상태다.
#[tauri::command]
pub async fn ime_toggle(app: AppHandle) -> Result<ImeState, RpcError> {
    on_main_thread(&app, || platform::set_native(None))
}

// ---------------------------------------------------------------- pure logic

/// 입력 소스/입력 모드 식별자 → CJK 계열. 소문자 부분일치, 판정 순서 ko→zh→ja.
/// Apple 표준(두벌식, SCIM/TCIM 모든 모드, Kotoeri — 로마자 입력 모드 포함,
/// 로마자 입력도 일본어 IME다)과 주요 서드파티(구름, Rime/Squirrel, AquaSKK)를 덮는다.
pub fn cjk_family(id: &str) -> Option<ImeFamily> {
    let lower = id.to_ascii_lowercase();
    if lower.contains("korean") || lower.contains("hangul") || lower.contains(".gureum.") {
        return Some(ImeFamily::Korean);
    }
    if lower.contains("scim")
        || lower.contains("tcim")
        || lower.contains("cangjie")
        || lower.contains("pinyin")
        || lower.contains("zhuyin")
        || lower.contains("bopomofo")
        || lower.contains("wubi")
        || lower.contains(".rime.")
        || lower.contains("squirrel")
    {
        return Some(ImeFamily::Chinese);
    }
    if lower.contains("kotoeri")
        || lower.contains("aquaskk")
        || lower.contains(".skk.")
        || lower.contains("inputmethod.japanese")
    {
        return Some(ImeFamily::Japanese);
    }
    None
}

/// 라틴 목표: 마지막으로 떠났던 비-CJK 소스(아직 켜져 있으면) → ABC → US →
/// 아무 `com.apple.keylayout.*` → 그 외 첫 비-CJK 소스.
pub fn pick_latin_source<'a>(enabled: &'a [String], last_latin: Option<&str>) -> Option<&'a str> {
    let latin = || enabled.iter().filter(|id| cjk_family(id).is_none());
    if let Some(last) = last_latin {
        if let Some(found) = latin().find(|id| id.as_str() == last) {
            return Some(found.as_str());
        }
    }
    for preferred in ["com.apple.keylayout.ABC", "com.apple.keylayout.US"] {
        if let Some(found) = latin().find(|id| id.as_str() == preferred) {
            return Some(found.as_str());
        }
    }
    latin()
        .find(|id| id.starts_with("com.apple.keylayout."))
        .or_else(|| latin().next())
        .map(String::as_str)
}

/// 네이티브(CJK) 목표: 마지막으로 쓴 CJK 소스(아직 켜져 있으면) → 그 계열의
/// 선호 모드 → 아무 CJK 소스. 계열 힌트가 없으면 enabled 순서상 첫 CJK의
/// 계열을 쓴다 — 목록 순서는 시스템 설정의 사용자 순서를 따르므로 결정적이다.
pub fn pick_native_source<'a>(enabled: &'a [String], last_native: Option<&str>) -> Option<&'a str> {
    let cjk = || enabled.iter().filter(|id| cjk_family(id).is_some());
    if let Some(last) = last_native {
        if let Some(found) = cjk().find(|id| id.as_str() == last) {
            return Some(found.as_str());
        }
    }
    let family = last_native.and_then(cjk_family).or_else(|| {
        enabled
            .iter()
            .find(|id| cjk_family(id).is_some())
            .and_then(|id| cjk_family(id))
    });
    let pick = match family {
        Some(ImeFamily::Korean) => cjk()
            .find(|id| id.to_ascii_lowercase().ends_with("2setkorean"))
            .or_else(|| cjk().find(|id| id.to_ascii_lowercase().contains(".korean.")))
            .or_else(|| cjk().find(|id| cjk_family(id) == Some(ImeFamily::Korean))),
        Some(ImeFamily::Chinese) => cjk()
            .find(|id| id.to_ascii_lowercase().contains("scim.itabc"))
            .or_else(|| cjk().find(|id| id.to_ascii_lowercase().contains(".scim.")))
            .or_else(|| cjk().find(|id| id.to_ascii_lowercase().contains(".tcim.")))
            .or_else(|| cjk().find(|id| cjk_family(id) == Some(ImeFamily::Chinese))),
        Some(ImeFamily::Japanese) => cjk()
            .find(|id| id.to_ascii_lowercase().ends_with("kotoeri.japanese"))
            .or_else(|| cjk().find(|id| id.to_ascii_lowercase().contains("kotoeri")))
            .or_else(|| cjk().find(|id| cjk_family(id) == Some(ImeFamily::Japanese))),
        None => None,
    };
    pick.or_else(|| cjk().next()).map(String::as_str)
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

    use super::{cjk_family, pick_latin_source, pick_native_source, ImeFamily, ImeState};

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

    /// 마지막으로 쓴 소스들 — 토글이 사용자의 자판(예: Dvorak, 마지막 CJK
    /// 계열)으로 돌아가게 한다. 네이티브 쪽은 계열 무관 단일 기억이다.
    static LAST_NATIVE: Mutex<Option<String>> = Mutex::new(None);
    static LAST_LATIN: Mutex<Option<String>> = Mutex::new(None);

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

    /// 다음 네이티브 전환 목표의 계열: 마지막으로 쓴 CJK 소스(아직 켜져
    /// 있으면) → enabled 순서상 첫 CJK의 계열.
    fn next_native_family(ids: &[String]) -> Option<ImeFamily> {
        let last = LAST_NATIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(found) = last
            .as_deref()
            .and_then(|last| ids.iter().find(|id| *id == last))
        {
            return cjk_family(found);
        }
        ids.iter()
            .find(|id| cjk_family(id).is_some())
            .and_then(|id| cjk_family(id))
    }

    fn state_from(current: Option<String>, ids: &[String]) -> ImeState {
        if !ids.iter().any(|id| cjk_family(id).is_some()) {
            return ImeState {
                available: false,
                native: current.as_deref().map(|id| cjk_family(id).is_some()),
                family: current.as_deref().and_then(cjk_family),
                source_id: current,
                reason: Some("no CJK input source enabled in System Settings".into()),
            };
        }
        let family = match &current {
            Some(cur) => cjk_family(cur).or_else(|| next_native_family(ids)),
            None => next_native_family(ids),
        };
        ImeState {
            available: true,
            native: current.as_deref().map(|id| cjk_family(id).is_some()),
            family,
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
        let slot = if cjk_family(current).is_some() {
            &LAST_NATIVE
        } else {
            &LAST_LATIN
        };
        *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(current.to_string());
    }

    pub(super) fn set_native(target: Option<bool>) -> ImeState {
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
        let now_native = before.native.unwrap_or(false);
        let want_native = target.unwrap_or(!now_native);
        if let Some(cur) = &current {
            remember(cur);
        }
        let pick = if want_native {
            let last = LAST_NATIVE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            pick_native_source(&ids, last.as_deref()).map(str::to_string)
        } else {
            let last = LAST_LATIN.lock().unwrap_or_else(|p| p.into_inner()).clone();
            pick_latin_source(&ids, last.as_deref()).map(str::to_string)
        };
        let Some(pick) = pick else {
            return ImeState {
                reason: Some(if want_native {
                    "no CJK input source to switch to".into()
                } else {
                    "no non-CJK input source to switch to".into()
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
        if after.native != Some(want_native) {
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
    use std::sync::Mutex;

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

    use super::{ImeFamily, ImeState};

    /// HKL 하위 워드(주 언어 ID) → CJK 계열. zh는 간체(0x0804)·번체(0x0404)·
    /// 홍콩(0x0c04) 세 locale을 묶는다.
    fn family_of_langid(lang: u16) -> Option<ImeFamily> {
        match lang {
            0x0412 => Some(ImeFamily::Korean),
            0x0411 => Some(ImeFamily::Japanese),
            0x0804 | 0x0404 | 0x0c04 => Some(ImeFamily::Chinese),
            _ => None,
        }
    }

    /// 마지막으로 관찰한 CJK 계열 — 라틴 상태에서 네이티브로 돌아갈 배열을
    /// 고르는 힌트다(관찰이 없으면 한국어 — 종전 동작의 결정적 기본값).
    static LAST_NATIVE_FAMILY: Mutex<Option<ImeFamily>> = Mutex::new(None);

    fn last_native_family() -> Option<ImeFamily> {
        *LAST_NATIVE_FAMILY.lock().unwrap_or_else(|p| p.into_inner())
    }

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

    fn layout_family(thread: u32) -> Option<ImeFamily> {
        let hkl = unsafe { GetKeyboardLayout(thread) };
        family_of_langid(langid(hkl.0 as usize))
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

    /// 한/영 키 합성(IMM이 거부하는 TSF 전용 IME용 폴백) — 한국어 배열 전용.
    fn press_hangul_key() {
        let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
            r#types: INPUT_KEYBOARD,
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
        let _ = unsafe { SendInput(&inputs, size_of::<INPUT>() as u32) };
    }

    fn state_of(hwnd: HWND, thread: u32) -> ImeState {
        let family = layout_family(thread);
        if let Some(f) = family {
            // 관찰 기록 — 라틴에서 돌아올 배열을 고르는 힌트. 사용자의 입력
            // 소스를 바꾸지는 않는다(읽기 경로의 계약 유지).
            *LAST_NATIVE_FAMILY.lock().unwrap_or_else(|p| p.into_inner()) = Some(f);
        }
        let mode = conversion_mode(hwnd);
        let native = match family {
            Some(_) => mode.map(|m| (m.0 & IME_CMODE_NATIVE.0) != 0),
            None => Some(false),
        };
        ImeState {
            available: true,
            native,
            family: family.or_else(last_native_family),
            source_id: Some(format!(
                "hkl:{:04x}{}",
                langid(unsafe { GetKeyboardLayout(thread) }.0 as usize),
                mode.map(|m| format!(":cmode={:#x}", m.0))
                    .unwrap_or_default()
            )),
            reason: native
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

    pub(super) fn set_native(target: Option<bool>) -> ImeState {
        let Some((hwnd, thread)) = focus_window() else {
            return ImeState::unavailable("no foreground window");
        };
        let before = state_of(hwnd, thread);
        let want = target.unwrap_or(!before.native.unwrap_or(false));
        let current_family = layout_family(thread);
        if want && current_family.is_none() {
            // 자판이 CJK가 아니면 먼저 목표 언어 배열로. 목표 계열은 마지막
            // 관찰 CJK 계열, 없으면 한국어(종전 동작의 결정적 기본값).
            let kl = match last_native_family().unwrap_or(ImeFamily::Korean) {
                ImeFamily::Korean => w!("00000412"),
                ImeFamily::Chinese => w!("00000804"),
                ImeFamily::Japanese => w!("00000411"),
            };
            match unsafe { LoadKeyboardLayoutW(kl, KLF_ACTIVATE) } {
                Ok(hkl) => {
                    let _ = unsafe { ActivateKeyboardLayout(hkl, KLF_SETFORPROCESS) };
                }
                Err(e) => {
                    return ImeState {
                        reason: Some(format!("native keyboard layout unavailable: {e}")),
                        ..before
                    };
                }
            }
        }
        if !set_conversion_native(hwnd, want) {
            // TSF 전용 IME: 변환 모드 API가 거부하면 키 합성으로 토글한다.
            // VK_HANGUL 합성은 한국어 배열에서만 의미가 있으므로 zh/ja는
            // 이유로 보고하고 추측 합성은 하지 않는다.
            if current_family == Some(ImeFamily::Korean) {
                press_hangul_key();
                std::thread::sleep(std::time::Duration::from_millis(30));
            } else {
                return ImeState {
                    reason: Some(
                        "TSF-only IME refused conversion mode (key synthesis is Korean-only)"
                            .into(),
                    ),
                    ..before
                };
            }
        }
        let after = state_of(hwnd, thread);
        if after.native != Some(want) {
            return ImeState {
                reason: Some(format!(
                    "IME mode did not change (wanted {})",
                    if want { "native" } else { "latin" }
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

    pub(super) fn set_native(_target: Option<bool>) -> ImeState {
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
    fn cjk_family_covers_apple_modes_and_third_parties() {
        // 한국어 — Apple 모든 모드와 서드파티(구름, 한글 계열).
        for id in [
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.inputmethod.Korean.3SetKorean",
            "com.apple.inputmethod.Korean.390Sebulshik",
            "com.apple.inputmethod.Korean",
            "org.youknowone.inputmethod.Gureum.han2",
            "kr.hangul.some.ime",
        ] {
            assert_eq!(cjk_family(id), Some(ImeFamily::Korean), "{id}");
        }
        // 중국어 — 병음·오필·창힐·번체, Rime/Squirrel.
        for id in [
            "com.apple.inputmethod.SCIM.ITABC",
            "com.apple.inputmethod.SCIM.WBX",
            "com.apple.inputmethod.TCIM.ITABC",
            "com.apple.inputmethod.TCIM.Cangjie",
            "im.rime.inputmethod.Squirrel.Rime",
        ] {
            assert_eq!(cjk_family(id), Some(ImeFamily::Chinese), "{id}");
        }
        // 일본어 — Kotoeri 모든 모드(로마자 입력도 일본어 IME다), AquaSKK, Google IME.
        for id in [
            "com.apple.inputmethod.Kotoeri.Japanese",
            "com.apple.inputmethod.Kotoeri.Japanese.Katakana",
            "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese",
            "com.apple.inputmethod.Japanese",
            "jp.sourceforge.aquaskk.inputmethod.SKK.JA",
            "com.google.inputmethod.Japanese.Roman",
        ] {
            assert_eq!(cjk_family(id), Some(ImeFamily::Japanese), "{id}");
        }
        // 라틴 자판은 어느 계열도 아니다.
        for id in [
            "com.apple.keylayout.ABC",
            "com.apple.keylayout.US",
            "com.apple.keylayout.Dvorak",
        ] {
            assert_eq!(cjk_family(id), None, "{id}");
        }
    }

    #[test]
    fn serde_serializes_family_lowercase() {
        assert_eq!(
            serde_json::to_value(ImeFamily::Korean).unwrap(),
            serde_json::json!("ko")
        );
        assert_eq!(
            serde_json::to_value(ImeFamily::Chinese).unwrap(),
            serde_json::json!("zh")
        );
        assert_eq!(
            serde_json::to_value(ImeFamily::Japanese).unwrap(),
            serde_json::json!("ja")
        );
    }

    #[test]
    fn latin_pick_prefers_last_then_abc_then_us_then_any_layout() {
        let enabled = ids(&[
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.keylayout.Dvorak",
            "com.apple.keylayout.US",
            "com.apple.keylayout.ABC",
        ]);
        assert_eq!(
            pick_latin_source(&enabled, Some("com.apple.keylayout.Dvorak")),
            Some("com.apple.keylayout.Dvorak")
        );
        // 마지막 소스가 꺼졌으면 ABC.
        assert_eq!(
            pick_latin_source(&enabled, Some("com.apple.keylayout.Colemak")),
            Some("com.apple.keylayout.ABC")
        );
        assert_eq!(
            pick_latin_source(&enabled, None),
            Some("com.apple.keylayout.ABC")
        );
        let no_abc = ids(&[
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.keylayout.US",
        ]);
        assert_eq!(
            pick_latin_source(&no_abc, None),
            Some("com.apple.keylayout.US")
        );
        // CJK(중국어·일본어 포함)는 라틴 후보에서 제외된다.
        let mixed = ids(&[
            "com.apple.inputmethod.Korean.2SetKorean",
            "com.apple.inputmethod.SCIM.ITABC",
            "com.apple.keylayout.ABC",
        ]);
        assert_eq!(
            pick_latin_source(&mixed, None),
            Some("com.apple.keylayout.ABC")
        );
        let only_cjk = ids(&["com.apple.inputmethod.Kotoeri.Japanese"]);
        assert_eq!(pick_latin_source(&only_cjk, None), None);
    }

    #[test]
    fn native_pick_prefers_last_then_family_preference() {
        // 한국어: 마지막 사용 → 두벌식 우선 → 아무 한국어.
        let ko = ids(&[
            "com.apple.keylayout.ABC",
            "com.apple.inputmethod.Korean.3SetKorean",
            "com.apple.inputmethod.Korean.2SetKorean",
        ]);
        assert_eq!(
            pick_native_source(&ko, Some("com.apple.inputmethod.Korean.3SetKorean")),
            Some("com.apple.inputmethod.Korean.3SetKorean")
        );
        assert_eq!(
            pick_native_source(&ko, None),
            Some("com.apple.inputmethod.Korean.2SetKorean")
        );
        let gureum = ids(&[
            "com.apple.keylayout.ABC",
            "org.youknowone.inputmethod.Gureum.han2",
        ]);
        assert_eq!(
            pick_native_source(&gureum, None),
            Some("org.youknowone.inputmethod.Gureum.han2")
        );
        // 중국어: 병음(ITABC) 우선, 마지막 사용 존중.
        let zh = ids(&[
            "com.apple.keylayout.ABC",
            "com.apple.inputmethod.SCIM.WBX",
            "com.apple.inputmethod.SCIM.ITABC",
        ]);
        assert_eq!(
            pick_native_source(&zh, None),
            Some("com.apple.inputmethod.SCIM.ITABC")
        );
        assert_eq!(
            pick_native_source(&zh, Some("com.apple.inputmethod.SCIM.WBX")),
            Some("com.apple.inputmethod.SCIM.WBX")
        );
        // 일본어: 히라가나(Kotoeri.Japanese) 우선.
        let ja = ids(&[
            "com.apple.keylayout.ABC",
            "com.apple.inputmethod.Kotoeri.Japanese.Katakana",
            "com.apple.inputmethod.Kotoeri.Japanese",
        ]);
        assert_eq!(
            pick_native_source(&ja, None),
            Some("com.apple.inputmethod.Kotoeri.Japanese")
        );
        // 다계열: 기억 없으면 목록 순서 첫 CJK(시스템 설정의 사용자 순서) — 결정적.
        let multi = ids(&[
            "com.apple.keylayout.ABC",
            "com.apple.inputmethod.Kotoeri.Japanese",
            "com.apple.inputmethod.Korean.2SetKorean",
        ]);
        assert_eq!(
            pick_native_source(&multi, None),
            Some("com.apple.inputmethod.Kotoeri.Japanese")
        );
        // 기억한 계열이 꺼졌으면 나머지 CJK 중 계열 선호로.
        assert_eq!(
            pick_native_source(&multi, Some("com.apple.inputmethod.SCIM.ITABC")),
            Some("com.apple.inputmethod.Kotoeri.Japanese")
        );
        // 라틴만 있으면 네이티브 후보가 없다.
        assert_eq!(
            pick_native_source(&ids(&["com.apple.keylayout.ABC"]), None),
            None
        );
    }

    #[test]
    fn unavailable_state_carries_a_reason_and_no_guess() {
        let state = ImeState::unavailable("why");
        assert!(!state.available);
        assert_eq!(state.native, None);
        assert_eq!(state.family, None);
        assert_eq!(state.source_id, None);
        assert_eq!(state.reason.as_deref(), Some("why"));
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["sourceId"], serde_json::Value::Null);
        assert_eq!(json["family"], serde_json::Value::Null);
    }

    /// 읽기 전용: 사용자의 입력 소스를 바꾸지 않는다.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reads_the_current_input_source_without_changing_it() {
        let before = platform::state();
        eprintln!(
            "current input source: {:?} (native={:?}, family={:?}, available={}, reason={:?})",
            before.source_id, before.native, before.family, before.available, before.reason
        );
        let again = platform::state();
        assert_eq!(before.source_id, again.source_id);
        // CJK 소스가 켜져 있으면 available, 아니면 이유가 있어야 한다.
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
        let once = platform::set_native(None);
        eprintln!("after 1st toggle: {once:?}");
        assert_ne!(once.native, start.native);
        let twice = platform::set_native(None);
        eprintln!("after 2nd toggle: {twice:?}");
        assert_eq!(twice.native, start.native);
        assert_eq!(twice.source_id, start.source_id);
    }

    /// 수동 확인용(2개 이상 CJK 계열 필요): 네이티브 구간에 두 번 들어가
    /// 모두 마지막으로 쓴 계열로 모이는지 본다
    /// (`cargo test -p iyagi-app manual_round_trip -- --ignored --nocapture`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "changes the user's active input source; run manually"]
    fn manual_round_trip_prefers_last_used_family() {
        let first = platform::state();
        if !first.available {
            eprintln!("skip: {:?}", first.reason);
            return;
        }
        let once = platform::set_native(Some(true));
        eprintln!("after native #1: {once:?}");
        assert_eq!(once.native, Some(true));
        let _ = platform::set_native(Some(false));
        let again = platform::set_native(Some(true));
        eprintln!("after native #2: {again:?}");
        assert_eq!(again.native, Some(true));
        assert_eq!(again.family, once.family);
    }
}
