/**
 * 경로 표시(04-ui §2·§5): 사용자 홈 아래 경로는 `~`로 줄여 보인다.
 *
 * 표시에만 쓴다 — 복사·재개·저장에는 항상 원문 경로를 쓰고, 툴팁에도 원문을
 * 남긴다. 홈은 Tauri가 알려 준 실제 홈(`homeDir()`)일 때만 넘긴다; 컨트롤러의
 * 대체값("/home"·"C:\\Users")은 홈들의 부모라 여기에 넣으면 안 된다.
 */

const DRIVE = /^[A-Za-z]:/;
const isSep = (ch: string | undefined): boolean => ch === "/" || ch === "\\";

export function abbreviateHome(path: string, home: string | null | undefined): string {
  if (!home) return path;
  // Tauri의 homeDir()는 끝에 구분자를 붙여 준다("/Users/x/").
  const root = home.replace(/[\\/]+$/, "");
  // 뿌리("/"·"C:")는 홈이 아니다 — 모든 경로를 ~ 아래로 만들어 버린다.
  if (root === "" || /^[A-Za-z]:$/.test(root)) return path;
  // Windows 경로는 대소문자와 구분자 모양을 가리지 않고 견준다(C:/Users/x == c:\\users\\x).
  const windows = DRIVE.test(root) || root.startsWith("\\\\");
  const norm = (s: string): string => (windows ? s.replace(/\//g, "\\").toLowerCase() : s);
  const p = norm(path);
  const h = norm(root);
  if (p === h) return "~";
  // 형제 경로("/Users/xy")를 홈("/Users/x")으로 오인하지 않는다 — 다음 글자가 구분자여야 한다.
  if (!p.startsWith(h) || !isSep(p[h.length])) return path;
  return "~" + path.slice(root.length);
}
