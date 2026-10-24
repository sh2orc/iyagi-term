//! 데몬 바이너리 이름 규약(원본 bridge `daemon_manager.rs`에서 분리).
//! shell_profiles의 미리보기가 기본 이름을 그릴 때 쓴다.

pub fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "iyagi-termd.exe"
    } else {
        "iyagi-termd"
    }
}
