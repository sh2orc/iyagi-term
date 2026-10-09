//! 앱 종료 확인(트레이 "종료"·앱 메뉴 Quit·마지막 창 파괴).
//!
//! 데몬은 앱과 분리돼 있어 앱이 끝나도 터미널(PTY)은 살아남는다(02 §1).
//! 그래서 "앱 종료"는 두 가지 뜻을 가진다 — 터미널을 백그라운드에 남기고
//! 창만 끝내거나, 터미널까지 함께 끝내거나. 어느 쪽인지는 사용자만 안다.
//!
//! 흐름:
//! 1. `RunEvent::ExitRequested`가 오면(트레이 종료 = `app.exit(0)`, 마지막
//!    창 파괴) 아직 승인 전이면 `prevent_exit()`으로 막고
//!    `iyagi://quit-requested`를 웹뷰에 보낸다.
//! 2. 프론트는 수신 즉시 `app_quit_ack`로 워치독을 풀고, 살아 있는 터미널이
//!    있으면 대화상자를 띄운다. 결정은 `app_quit`(승인 → 종료) 또는
//!    `app_quit_cancel`로 돌아온다.
//! 3. `ACK_TIMEOUT` 안에 ack가 없으면(웹뷰가 죽었거나 아직 로딩 중) 강제
//!    종료한다 — 묻지 못했으니 터미널은 건드리지 않는 쪽(유지)이 안전하다.
//!
//! macOS Cmd+Q는 여기로 오지 않는다: tao는 `applicationShouldTerminate`를
//! 구현하지 않고 muda의 기본 Quit 항목은 `terminate:`를 직접 호출한다.
//! 그래서 프론트(nativeMenu.ts)가 앱 메뉴의 Quit 항목을 자체 항목으로 바꿔
//! 같은 확인 흐름을 태운다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, Runtime};

/// 웹뷰에 보내는 종료 요청 이벤트 이름(프론트 `features/app/quit.ts`와 동일).
pub const QUIT_REQUESTED_EVENT: &str = "iyagi://quit-requested";
/// 프론트 수신 확인 대기 상한. 정상 로딩 상태의 웹뷰는 수 ms 안에 답한다.
pub const ACK_TIMEOUT: Duration = Duration::from_millis(2500);

/// 종료 상태 기계(원자값만 — 이벤트 루프 스레드와 비동기 워치독이 공유).
#[derive(Default)]
pub struct QuitState {
    /// 사용자가(또는 워치독이) 종료를 승인했다 — 다음 ExitRequested는 통과.
    approved: AtomicBool,
    /// 단조 증가 요청 번호 발급기(워치독이 자기 요청을 식별한다).
    next_generation: AtomicU64,
    /// 대기 중 요청 번호(0 = 없음). cancel이 0으로 되돌린다.
    pending: AtomicU64,
    /// 대기 중 요청을 프론트가 받았다(대화상자 표시 중).
    acked: AtomicBool,
}

#[derive(Clone, serde::Serialize)]
struct QuitRequestedPayload {
    generation: u64,
}

impl QuitState {
    pub fn is_approved(&self) -> bool {
        self.approved.load(Ordering::SeqCst)
    }

    pub fn approve(&self) {
        self.approved.store(true, Ordering::SeqCst);
    }

    /// 새 요청을 대기 상태로 만들고 번호를 돌려준다. 이미 대기 중이면(대화상자가
    /// 떠 있는데 트레이 종료를 또 누름) 새 번호를 발급하되 ack 상태는 유지한다 —
    /// 프론트가 다시 ack하지 않아도 워치독이 대화상자를 강제 종료로 덮지 않게.
    pub fn request(&self) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let was_pending = self.pending.swap(generation, Ordering::SeqCst) != 0;
        if !was_pending {
            self.acked.store(false, Ordering::SeqCst);
        }
        generation
    }

    pub fn ack(&self) {
        if self.pending.load(Ordering::SeqCst) != 0 {
            self.acked.store(true, Ordering::SeqCst);
        }
    }

    pub fn cancel(&self) {
        self.pending.store(0, Ordering::SeqCst);
        self.acked.store(false, Ordering::SeqCst);
    }

    /// 워치독 판정: 그 요청이 아직 대기 중인데 프론트가 받지 못했으면 강제 종료.
    pub fn should_force(&self, generation: u64) -> bool {
        self.pending.load(Ordering::SeqCst) == generation && !self.acked.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst) != 0
    }
}

/// `RunEvent::ExitRequested` 처리. 승인 전이면 종료를 막고 프론트에 묻는다.
pub fn on_exit_requested<R: Runtime>(
    app: &AppHandle<R>,
    api: &tauri::ExitRequestApi,
    code: Option<i32>,
) {
    let state = app.state::<QuitState>();
    // 재시작(RESTART_EXIT_CODE)과 승인된 종료는 통과.
    if state.is_approved() || code == Some(tauri::RESTART_EXIT_CODE) {
        return;
    }
    api.prevent_exit();

    if app.get_webview_window("main").is_none() {
        // 물어볼 웹뷰가 없다 — 그대로 끝낸다(데몬이 세션을 지킨다).
        state.approve();
        app.exit(0);
        return;
    }
    // 창 닫기(CloseRequested)는 `exit(0)`으로 이 경로에 들어온다 — 트레이
    // 숨김 경로는 없다(Linux GNOME에는 트레이 자체가 없을 수 있다). 창은
    // 여기서 건드리지 않는다: 묻지 않고 끝나는 경로(설정 keep/terminate,
    // 살아 있는 터미널 없음)에서 깜빡이지 않도록, 대화상자를 실제로 띄우는
    // 프론트가 그때 창을 앞으로 가져온다(quit.ts `reveal`).

    let generation = state.request();
    if app
        .emit(QUIT_REQUESTED_EVENT, QuitRequestedPayload { generation })
        .is_err()
    {
        state.approve();
        app.exit(0);
        return;
    }

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(ACK_TIMEOUT).await;
        let state = handle.state::<QuitState>();
        if state.should_force(generation) {
            tracing::warn!(
                generation,
                "quit: webview did not acknowledge the request in time — exiting"
            );
            state.approve();
            handle.exit(0);
        }
    });
}

/// 프론트가 종료 요청을 받았다(대화상자를 띄우거나 바로 결정한다).
#[tauri::command]
pub fn app_quit_ack(state: tauri::State<'_, QuitState>) {
    state.ack();
}

/// 사용자가 취소했다 — 다음 종료 요청은 다시 묻는다.
#[tauri::command]
pub fn app_quit_cancel(state: tauri::State<'_, QuitState>) {
    state.cancel();
}

/// 최종 종료. 터미널 종료 여부는 프론트가 이미 데몬에 반영했다(workload.cancel).
#[tauri::command]
pub fn app_quit(app: AppHandle, state: tauri::State<'_, QuitState>) {
    state.approve();
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_neither_approved_nor_pending() {
        let state = QuitState::default();
        assert!(!state.is_approved());
        assert!(!state.is_pending());
        assert!(!state.should_force(1));
    }

    #[test]
    fn unacked_request_forces_after_timeout_but_acked_one_waits() {
        let state = QuitState::default();
        let first = state.request();
        assert!(state.is_pending());
        assert!(
            state.should_force(first),
            "no ack yet → watchdog must force"
        );
        state.ack();
        assert!(!state.should_force(first), "dialog is up → never force");
    }

    #[test]
    fn cancel_clears_pending_so_a_stale_watchdog_is_a_no_op() {
        let state = QuitState::default();
        let first = state.request();
        state.cancel();
        assert!(!state.is_pending());
        assert!(!state.should_force(first));
        // 다음 요청은 새 번호 — 이전 워치독과 절대 겹치지 않는다.
        let second = state.request();
        assert_ne!(first, second);
        assert!(state.should_force(second));
        assert!(!state.should_force(first));
    }

    #[test]
    fn repeated_request_while_dialog_is_open_keeps_the_ack() {
        let state = QuitState::default();
        let first = state.request();
        state.ack();
        let second = state.request();
        assert_ne!(first, second);
        assert!(
            !state.should_force(second),
            "user is still looking at the dialog"
        );
    }

    #[test]
    fn ack_without_a_pending_request_is_ignored() {
        let state = QuitState::default();
        state.ack();
        let generation = state.request();
        assert!(state.should_force(generation));
    }

    #[test]
    fn approve_is_sticky() {
        let state = QuitState::default();
        state.approve();
        state.request();
        state.cancel();
        assert!(state.is_approved());
    }
}
