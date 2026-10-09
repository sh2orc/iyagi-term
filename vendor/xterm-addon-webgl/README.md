# vendor/xterm-addon-webgl

`@xterm/addon-webgl` 0.19.0(업스트림 커밋 `f447274f430fd22513f6adbf9862d19524471c04`, `@xterm/xterm` 6.0.0의 짝)의
소스를 그대로 가져와 아래 수정을 적용한 사본이다. 앱은 npm 패키지 대신 이 소스를 묶은
`src/vendor/xterm-addon-webgl/addon-webgl.js`를 쓴다(`src/features/terminal/xtermSetup.ts`).

## 왜

WKWebView(Tauri)에서 한글·색이 많은 TUI 출력(Claude Code 등)을 오래 보면 일부 글자만 다른 글리프의 조각·상자로
깨져 보이고, 드래그로 선택하면(그 셀을 다시 그리면) 정상으로 돌아오는 증상이 있었다. 원인은 애드온의 글리프
아틀라스: 페이지가 16장에 이르면 4장을 하나로 합치는데(merge), 그 병합이 한 프레임의 셀 갱신 도중에 일어나면
이미 써 둔 정점이 옛 페이지 번호·좌표를 가리킨 채 그려지고, 그 pane에 더 그릴 일이 없으면 그 프레임이 그대로
남는다. 업스트림은 이를 안정 릴리스에는 아직 담지 않고 `0.20.0-beta`(코어 `6.1.0-beta` 필요)에만 고쳤다.
코어 업그레이드는 IME 브리지 등 코어 내부에 기대는 부분이 많아 미루고, 애드온만 패치해 묶는다.

## 적용한 업스트림 수정(xterm.js PR)

| PR | 내용 |
| --- | --- |
| [#5883](https://github.com/xtermjs/xterm.js/pull/5883) | 갱신 도중 페이지 병합이 일어나면 모델을 다시 만들고 텍스처를 모두 다시 올린다(`MERGE_RETRY_LIMIT`). `AtlasPage.version`을 전역 단조 증가 값으로 바꿔, 병합 뒤 같은 인덱스에 다른 페이지가 오는 경우를 놓치지 않는다. `RectangleRenderer` 정점 버퍼 확장 크기(+1 뷰포트 지우기 사각형). |
| [#6042](https://github.com/xtermjs/xterm.js/pull/6042) | 병합 알림을 소비 1회 플래그(`_requestClearModel`) 대신 `pageLayoutVersion`으로 바꾸고, 렌더러마다 마지막으로 본 값을 기억한다 — 아틀라스를 공유하는 다른 터미널도 다음 프레임에 모델을 다시 만든다. |
| [#6055](https://github.com/xtermjs/xterm.js/pull/6055) | `clearTexture()`도 `pageLayoutVersion`을 올린다(한 터미널이 아틀라스를 지워도 다른 터미널이 낡은 좌표로 그리지 않게). |
| [#6043](https://github.com/xtermjs/xterm.js/pull/6043) | 같은 크기 4장이 없어 병합할 수 없거나 큰 글리프용 페이지가 상한을 넘기면, 페이지를 더 만들지 않고 전부 비운다(`_evictAllPages`) — 텍스처 유닛 수를 넘어 `undefined.version`으로 죽던 경로. 렌더러는 페이지 수를 유닛 수로 잘라 쓴다. |
| [#5987](https://github.com/xtermjs/xterm.js/pull/5987) | 아틀라스 텍스처에 밉맵을 만들지 않는다(1:1로만 샘플하므로 불필요하고, 일부 드라이버에서 `GL_INVALID_OPERATION`으로 텍스처가 불완전해졌다). `LINEAR` 필터 명시. |
| [#5929](https://github.com/xtermjs/xterm.js/pull/5929) | 아틀라스 공유 판정(`configEquals`)에 `deviceMaxTextureSize`·`deviceCellWidth`·`deviceCellHeight`를 넣는다. |
| [#5923](https://github.com/xtermjs/xterm.js/pull/5923) | `TextureAtlas.dispose()`가 `onRemoveTextureAtlasCanvas` 이미터도 해제한다. |

0.19.0에 없는 로그 서비스 대신 페이지 상한 초과 경고는 `console.warn` 한 번으로 남긴다.

## 이 저장소 고유 수정(업스트림 PR 아님)

**`BaseRenderLayer`가 쓰지도 않는 텍스처 아틀라스를 잡던 것을 걷어냈다.**

`BaseRenderLayer._refreshCharAtlas()`는 `acquireTextureAtlas(..., 2048)`로 아틀라스를 잡고 `warmUp()`까지
돌렸지만, `_charAtlas` 필드는 이 클래스에서도 유일한 구현체인 `LinkRenderLayer`에서도 **한 번도 읽히지
않는다** — 링크 밑줄은 2D 컨텍스트의 `fillStyle`로 사각형을 그릴 뿐이다. 문제는 두 가지였다.

1. 마지막 인자가 `2048` 하드코딩이라 `WebglRenderer`가 쓰는 `gl.MAX_TEXTURE_SIZE`(Apple GPU에서 16384)와
   절대 같아지지 않는다. `configEquals`가 `deviceMaxTextureSize`를 비교하므로(위 #5929) 한 터미널이
   **설정이 다른 아틀라스 두 개를 소유**하게 된다.
2. `acquireTextureAtlas`의 소유권 루프는 이 터미널이 가진 **첫 번째 엔트리만 보고 `break`** 한다. 두 엔트리
   각각의 `ownedBy`가 길이 1이므로, 한쪽 렌더러의 acquire가 **다른 쪽 렌더러가 지금 쓰고 있는 아틀라스를
   `dispose()`** 한다.

그 결과 글꼴 크기가 바뀔 때마다(`fontSize`는 아틀라스 config의 일부다) 아틀라스가 불필요하게 두 번씩
버려지고 다시 만들어지며 ASCII 93자 `warmUp()`도 두 배로 돌았다. 글리프 래스터 비용은 셀 픽셀 넓이에
비례하므로 이 낭비는 **글꼴을 키울수록 커진다** — pane 줌에서 확대만 유독 걸리던 원인이다.

`_charAtlas` 필드·`_refreshCharAtlas()`와 그 호출부 세 곳(생성자의 `onChangeColors`, `_setTransparency`,
`resize`), 그리고 쓰이지 않게 된 import를 지웠다. 링크 레이어는 글리프를 그리지 않으므로 렌더링 결과는
같다. 업스트림을 올릴 때 이 수정이 들어갔는지 따로 확인해야 한다.

## 다시 묶기

```sh
node scripts/build-webgl-addon.mjs
```

esbuild가 `src/WebglAddon.ts`를 진입점으로 묶는다. 애드온이 코어 내부 모듈(`browser/…`, `common/…`, `vs/…`)을
가져오는 것은 업스트림 웹팩 번들과 같이 설치된 `@xterm/xterm`의 `src/`에서 해결하고, 클래스 필드는 업스트림
tsc 설정과 같이 대입 의미론(`useDefineForClassFields: false`, target es2021)으로 컴파일한다. 결과 번들은
저장소에 넣는다 — 앱 빌드(`vite build`)는 이 스크립트에 기대지 않는다. 타입은
`src/vendor/xterm-addon-webgl/addon-webgl.d.ts`(업스트림 typings와 같은 API).

## 업스트림을 올릴 때

`@xterm/addon-webgl`/`@xterm/xterm`을 안정판으로 올려 위 수정이 모두 들어가면 이 디렉터리와 번들을 지우고
`xtermSetup.ts`의 import를 패키지로 되돌린다. 그 전까지는 `node_modules/@xterm/addon-webgl/src`와 이 `src`를
diff해 패치를 유지한다.

## 검증

`docs/implementation/06-verification.md`의 WebGL 아틀라스 항목: 실제 xterm+이 애드온을 헤드리스 Chrome(ANGLE Metal)과
WKWebView 시험대에서 돌려, 다른 pane이 아틀라스 페이지를 16장 넘게 채워 병합을 여러 번 일으키는 동안 병합 직후
프레임의 정점이 아틀라스와 어긋나지 않고(패치 전: 한 화면의 920셀 중 886셀이 낡음), 화면 픽셀이 다시 그린 것과
같음을 확인한다.

## 라이선스

MIT — Copyright (c) 2017 The xterm.js authors (LICENSE).
