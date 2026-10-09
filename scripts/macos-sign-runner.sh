#!/bin/sh
# macos-sign-runner.sh — `tauri dev|build --runner`로 쓰는 cargo 대리자.
#
# cargo를 그대로 부르고, `cargo build`가 성공하면 앱(iyagi-app)과 데몬
# (iyagi-termd) 실행 파일을 로컬 인증서(IYAGI_SIGNING_IDENTITY)로 다시 서명한다.
# scripts/tauri.mjs가 macOS에서 그 인증서를 찾았을 때만 이 runner를 넘긴다.
#
# 왜 여기서: `tauri dev`는 번들 없이 target/<profile>/iyagi-app을 곧바로
# 띄운다 — 빌드와 실행 사이에 서명을 끼울 곳이 runner뿐이다. ad-hoc 서명은
# 빌드마다 바뀌어 macOS 권한(TCC)이 매번 새 앱으로 여기고 다시 묻는다.
#
# 실행 중인 파일을 제자리에서 다시 서명하면 커널이 그 프로세스를 죽일 수 있어
# (코드 페이지 서명 불일치) 복제본(cp -c, APFS clone)에 서명한 뒤 mv로 바꾼다.
# 서명 실패는 경고만 하고 빌드는 성공으로 둔다 — 서명은 권한 창을 줄이는
# 편의이지 빌드 조건이 아니다.

if [ "${1:-}" != "build" ] || [ -z "${IYAGI_SIGNING_IDENTITY:-}" ] || [ "$(uname -s)" != "Darwin" ]; then
  exec cargo "$@"
fi

cargo "$@" || exit $?

profile=debug
triple=
expect=
for arg in "$@"; do
  if [ -n "$expect" ]; then
    case "$expect" in
      profile) profile=$arg ;;
      target) triple=$arg ;;
    esac
    expect=
    continue
  fi
  case "$arg" in
    --) break ;;
    --release | -r) profile=release ;;
    --profile) expect=profile ;;
    --profile=*) profile=${arg#--profile=} ;;
    --target) expect=target ;;
    --target=*) triple=${arg#--target=} ;;
  esac
done
[ "$profile" = dev ] && profile=debug

root=$(cd "$(dirname "$0")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-$root/target}
case "$target_dir" in
  /*) ;;
  *) target_dir=$PWD/$target_dir ;;
esac
out=$target_dir/${triple:+$triple/}$profile

sign() {
  file=$1
  identifier=$2
  [ -f "$file" ] || return 0
  tmp=$file.iyagi-signing
  rm -f "$tmp"
  if { cp -c "$file" "$tmp" 2>/dev/null || cp "$file" "$tmp"; } &&
    codesign --force --sign "$IYAGI_SIGNING_IDENTITY" --identifier "$identifier" "$tmp" 2>/dev/null &&
    mv -f "$tmp" "$file"; then
    echo "macos-sign-runner: signed ${file##*/} as $identifier with \"$IYAGI_SIGNING_IDENTITY\"" >&2
    return 0
  fi
  rm -f "$tmp"
  echo "macos-sign-runner: warning: could not sign $file — it keeps its ad-hoc signature" >&2
}

sign "$out/iyagi-app" ai.iyagi.term
sign "$out/iyagi-termd" iyagi-termd
exit 0
