/**
 * 전송 계층(브리지·데몬 연결) 상태의 공유 관찰(05 §10).
 *
 * 세션 제어기(SessionController.checkTransport)가 주기적으로 읽은
 * `transportStatus()` 결과를 여기에 알리고, mission 화면 등은 따로 폴링하지 않고
 * 이 소식을 구독한다 — 같은 브리지 상태를 두 타이머가 따로 읽지 않게. 창이 숨은
 * 동안은 제어기가 점검을 늦추므로 구독자도 함께 느려진다.
 */

export interface TransportObservation {
  /** 제어·데이터 연결이 모두 살아 있는가. */
  connected: boolean;
  /** 연결 세대(재연결마다 바뀐다). 읽지 못했으면 null. */
  generation: number | null;
}

type Listener = (observation: TransportObservation) => void;

const listeners = new Set<Listener>();
let last: TransportObservation | null = null;

/** 새 관찰을 알린다(제어기 전용). */
export function publishTransportStatus(observation: TransportObservation): void {
  last = observation;
  for (const listener of [...listeners]) {
    try {
      listener(observation);
    } catch {
      // 한 구독자의 실패가 다른 구독자나 제어기의 점검을 막지 않는다.
    }
  }
}

/** 관찰을 구독한다 — 해제 함수를 돌려준다. */
export function subscribeTransportStatus(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 마지막 관찰(아직 없으면 null) — 늦게 붙은 구독자가 곧바로 상태를 맞춘다. */
export function lastTransportStatus(): TransportObservation | null {
  return last;
}

/** 시험 전용 초기화. */
export function resetTransportFeedForTests(): void {
  last = null;
  listeners.clear();
}
