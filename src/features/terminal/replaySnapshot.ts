/**
 * 화면 스냅샷 저장소(앱 재시작 재생 단축).
 *
 * 저널 전체 재생은 수십~수백 MiB를 다시 파싱하지만 남는 것은 화면과 스크롤백
 * 몇천 줄뿐이다. pane 화면을 직렬화(SerializeAddon)해 반영된 마지막 저널 seq와
 * 함께 두면, 다음 attach는 스냅샷을 먼저 쓰고 그 뒤 레코드만 재생한다
 * (`AttachParams.resume_from_seq`, 02-runner §5).
 *
 * 스냅샷은 캐시다: 읽기·쓰기 실패, 모양이 어긋난 기록, 늦은 응답은 모두 "없음"으로
 * 다뤄 전체 재생으로 물러난다. 세션 id로만 찾으며 출력 원문 외의 비밀은 넣지 않는다.
 */

export interface ReplaySnapshot {
  sessionId: string;
  /** 스냅샷 화면에 반영된 마지막 저널 seq. 재생은 seq + 1부터 잇는다. */
  seq: number;
  /** 직렬화할 때의 격자. 쓰기 전에 xterm을 이 크기로 맞춘다. */
  cols: number;
  rows: number;
  /** 화면·스크롤백·대체 화면·모드를 되살리는 제어 시퀀스 문자열. */
  data: string;
  /** 뜰 때 이미 반영된 화면 지우기 지점(clearMarks.ts, 0이면 없음). */
  clearMark: number;
  /** OSC 0/2 제목 — 직렬화 결과에는 제목 시퀀스가 없어 따로 둔다. */
  terminalTitle: string | null;
  /** CLI가 기록을 지우고 동기 출력으로 다시 그리는가(zoomPreview 판단). */
  historyRebuilder: boolean;
  savedAt: number;
}

export interface ReplaySnapshotStore {
  load(sessionId: string): Promise<ReplaySnapshot | null>;
  save(snapshot: ReplaySnapshot): Promise<void>;
  /** `sessionIds`에 없는 세션의 스냅샷을 지운다(닫힌 pane의 잔여물 정리). */
  retain(sessionIds: ReadonlySet<string>): Promise<void>;
}

export const REPLAY_SNAPSHOT_DB = "iyagi.replay-snapshots.v1";
const TABLE = "snapshots";
/** 이보다 큰 직렬화 결과는 저장하지 않는다(스크롤백을 크게 늘린 경우). */
export const REPLAY_SNAPSHOT_MAX_CHARS = 16 * 1024 * 1024;
/** 저장소가 막히거나 느리면 attach를 붙잡지 않고 전체 재생으로 간다. */
const LOAD_TIMEOUT_MS = 1500;
const MAX_GRID = 4096;

function positiveInt(value: unknown, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= max;
}

/** 저장된 값을 믿지 않고 필요한 필드만 검사해 되살린다. */
export function decodeReplaySnapshot(raw: unknown, sessionId: string): ReplaySnapshot | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const r = raw as Record<string, unknown>;
  if (r.sessionId !== sessionId) return null;
  if (!positiveInt(r.seq, Number.MAX_SAFE_INTEGER) || !positiveInt(r.cols, MAX_GRID) || !positiveInt(r.rows, MAX_GRID)) {
    return null;
  }
  if (typeof r.data !== "string" || r.data.length > REPLAY_SNAPSHOT_MAX_CHARS) return null;
  if (r.terminalTitle !== null && typeof r.terminalTitle !== "string") return null;
  return {
    sessionId,
    seq: r.seq,
    cols: r.cols,
    rows: r.rows,
    data: r.data,
    clearMark: typeof r.clearMark === "number" && Number.isSafeInteger(r.clearMark) && r.clearMark > 0 ? r.clearMark : 0,
    terminalTitle: r.terminalTitle,
    historyRebuilder: r.historyRebuilder === true,
    savedAt: typeof r.savedAt === "number" && Number.isFinite(r.savedAt) ? r.savedAt : 0,
  };
}

function withTimeout<T>(promise: Promise<T>, ms: number, fallback: T): Promise<T> {
  return new Promise<T>((resolve) => {
    const timer = setTimeout(() => resolve(fallback), ms);
    promise.then(
      (value) => { clearTimeout(timer); resolve(value); },
      () => { clearTimeout(timer); resolve(fallback); },
    );
  });
}

function requestDone<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("snapshot storage request failed"));
  });
}

function transactionDone(tx: IDBTransaction): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error ?? new Error("snapshot storage transaction aborted"));
    tx.onerror = () => undefined; // onabort가 뒤따른다.
  });
}

export function createIndexedDbSnapshotStore(factory: IDBFactory = indexedDB): ReplaySnapshotStore {
  let opening: Promise<IDBDatabase> | undefined;
  const open = () => opening ??= new Promise<IDBDatabase>((resolve, reject) => {
    const request = factory.open(REPLAY_SNAPSHOT_DB, 1);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(TABLE)) request.result.createObjectStore(TABLE);
    };
    request.onerror = () => { opening = undefined; reject(request.error ?? new Error("snapshot storage could not be opened")); };
    request.onblocked = () => { opening = undefined; reject(new Error("snapshot storage is blocked")); };
    request.onsuccess = () => {
      const db = request.result;
      db.onversionchange = () => { db.close(); opening = undefined; };
      resolve(db);
    };
  });

  const load = async (sessionId: string): Promise<ReplaySnapshot | null> => {
    const db = await open();
    const tx = db.transaction(TABLE, "readonly");
    const raw = await requestDone(tx.objectStore(TABLE).get(sessionId));
    return decodeReplaySnapshot(raw, sessionId);
  };

  return {
    load: (sessionId) => withTimeout(load(sessionId), LOAD_TIMEOUT_MS, null),
    save: async (snapshot) => {
      if (snapshot.data.length > REPLAY_SNAPSHOT_MAX_CHARS) return;
      try {
        const db = await open();
        const tx = db.transaction(TABLE, "readwrite");
        tx.objectStore(TABLE).put({ ...snapshot }, snapshot.sessionId);
        await transactionDone(tx);
      } catch {
        // 캐시 저장 실패는 다음 시작의 전체 재생으로 충분하다.
      }
    },
    retain: async (sessionIds) => {
      try {
        const db = await open();
        const tx = db.transaction(TABLE, "readwrite");
        const done = transactionDone(tx);
        const store = tx.objectStore(TABLE);
        const keys = store.getAllKeys();
        // 삭제는 같은 트랜잭션이 살아 있는 요청 콜백 안에서 건다(await 뒤면 이미 닫혔을 수 있다).
        keys.onsuccess = () => {
          for (const key of keys.result) {
            if (typeof key !== "string" || !sessionIds.has(key)) store.delete(key);
          }
        };
        await done;
      } catch {
        // 정리 실패는 다음 시작에 다시 시도한다.
      }
    },
  };
}

/** 시험·브라우저 미리보기용 메모리 저장소. */
export function createMemorySnapshotStore(): ReplaySnapshotStore & { readonly entries: Map<string, ReplaySnapshot> } {
  const entries = new Map<string, ReplaySnapshot>();
  return {
    entries,
    load: async (sessionId) => decodeReplaySnapshot(entries.get(sessionId) ?? null, sessionId),
    save: async (snapshot) => {
      if (snapshot.data.length <= REPLAY_SNAPSHOT_MAX_CHARS) entries.set(snapshot.sessionId, { ...snapshot });
    },
    retain: async (sessionIds) => {
      for (const key of [...entries.keys()]) if (!sessionIds.has(key)) entries.delete(key);
    },
  };
}
