# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: u18-transient-retry.spec.ts >> 자동 재시도 대기와 모델 변경을 700 화면에서 확인한다
- Location: tests/mission-ui/u18-transient-retry.spec.ts:6:3

# Error details

```
Error: page.evaluate: TypeError: Cannot read properties of undefined (reading 'id')
    at MockDaemonClient.seedMissionEntities (http://localhost:5183/src/features/daemon/mockClient.ts:1609:31)
    at eval (eval at evaluate (:311:30), <anonymous>:10:14)
    at async <anonymous>:337:30
```

# Page snapshot

```yaml
- generic [ref=e3]:
  - banner [ref=e4]:
    - navigation "터미널 탭" [ref=e5]:
      - tab "탭 1 3.8코어 · 774 MiB 탭 1 닫기" [ref=e6] [cursor=pointer]:
        - generic [ref=e7]: 탭 1
        - generic "이 탭의 살아 있는 터미널 중 최대 사용량(CPU · RAM). 에이전트가 모델 응답을 기다리는 동안은 CPU가 0에 가깝습니다 — 살아 있어도 <0.1코어로 보일 수 있습니다." [ref=e8]: 3.8코어 · 774 MiB
        - button "탭 1 닫기" [ref=e9]: ×
      - tab "로그인 기능 실행 중 1 탭 2 닫기" [selected] [ref=e10] [cursor=pointer]:
        - generic [ref=e11]: 로그인 기능
        - generic "실행 중 1" [ref=e12]
        - button "탭 2 닫기" [ref=e13]: ×
      - button "새 탭" [ref=e14] [cursor=pointer]: +
    - generic [ref=e15]:
      - button "동시 입력" [ref=e16] [cursor=pointer]
      - button "배치 편집" [ref=e23] [cursor=pointer]
      - button "언어 전환" [ref=e26] [cursor=pointer]
      - button "+ 새 AI 작업" [ref=e30] [cursor=pointer]
      - button "AI 작업 목록" [ref=e31] [cursor=pointer]: ☰
      - generic [ref=e32]:
        - button "새 터미널(zsh)" [ref=e33] [cursor=pointer]: + 새 터미널 · zsh
        - button "셸 프로필 선택" [ref=e34] [cursor=pointer]: ▾
      - button "설정" [ref=e35] [cursor=pointer]
  - generic [ref=e41]:
    - banner [ref=e42]:
      - generic [ref=e43]:
        - heading "로그인 기능" [level=2] [ref=e44]
        - generic [ref=e45]: 실행 중
        - generic [ref=e46]: · 계획 중
        - generic "활성 시간은 AI 작업이 실행 중이거나 일시정지를 기다리는 시간입니다. 병렬 실행 시간을 합산하지 않으며, 일시정지와 데몬 중단 시간은 제외합니다." [ref=e47]: 활성 시간 0:00:00 / 24:00:00
      - button "실행 제어 ▾" [ref=e49] [cursor=pointer]
    - navigation "로그인 기능" [ref=e51]:
      - button "팀 작업" [pressed] [ref=e52] [cursor=pointer]
      - button "Lead 대화" [ref=e53] [cursor=pointer]
      - button "결과" [ref=e54] [cursor=pointer]
    - complementary "팀 작업" [ref=e56]:
      - region "할 일 목록" [ref=e57]:
        - generic [ref=e58]:
          - generic [ref=e59]:
            - heading "할 일" [level=3] [ref=e60]
            - generic [ref=e61]:
              - generic [ref=e62]: 실행 1
              - generic [ref=e63]: 완료 0
              - generic [ref=e64]: 결정 0
          - group "할 일 목록" [ref=e65]:
            - button "전체" [pressed] [ref=e66] [cursor=pointer]
            - button "실행 중" [ref=e67] [cursor=pointer]
            - button "응답 필요" [ref=e68] [cursor=pointer]
            - button "대기" [ref=e69] [cursor=pointer]
            - button "완료" [ref=e70] [cursor=pointer]
          - searchbox "할 일 검색" [ref=e71]
        - list [ref=e72]:
          - listitem [ref=e73]:
            - button "작업 1 GLM-5.3 실행 중" [ref=e74] [cursor=pointer]:
              - generic [aria-hidden] [ref=e75]: ▶
              - generic [ref=e76]: 작업 1
              - generic [ref=e77]: GLM-5.3
              - generic [ref=e79]: 실행 중
            - generic [ref=e80]: 실행 중 · 1분 미만
  - contentinfo [ref=e82]:
    - button "자원 그래프 열기(최근 5분)" [ref=e83] [cursor=pointer]:
      - 'generic "mock.cpu에서 측정하지 못했습니다 · CPU 압력: 사용 코어/전체 코어 비율 · 85% 경고 · 95% 위험 · 70% 이하 10초 지속 시 회복" [ref=e84]':
        - text: CPU — / 12코어
        - generic [ref=e85]: · 압력 보통
      - 'generic "사용률 65.3% · mock.host · 시스템 전체 물리 메모리: 사용 / 전체. ‘사용’은 OS 기준(앱+wired+압축, 활성 모니터와 같음)이며, 재활용 가능한 캐시는 뺀 값이라 이 프로세스 하나의 메모리가 아닙니다." [ref=e86]':
        - text: RAM 20.9 GiB / 32 GiB
        - generic [ref=e87]: · 압력 보통
      - generic "저장 공간 사용률 64.8% · 호스트 디스크 R/W 속도는 계약에 없어 제공하지 않습니다" [ref=e88]: Disk R/W — · 여유 180.1 GiB
      - generic "mock.net에서 측정하지 못했습니다" [ref=e89]:
        - text: NET ↓ — ↑ —
        - generic [ref=e90]: · ethernet
    - button "AI 잔여량 —" [ref=e92] [cursor=pointer]
    - button "작업 대기열 열기" [ref=e93] [cursor=pointer]: 에이전트 없음
```