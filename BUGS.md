# Bug History — Extended Market Maker

핵심 버그 기록. 발생 순서대로 정리.

---

## P0 버그 (실제 손실 유발)

### BUG-001: Exposure tracker 방향성 미고려 — LONG 포지션 시 ASK 주문 차단
**커밋**: `110881b`, `77b5cac`
**증상**: LONG 포지션이 max_position_usd의 80% 이상 쌓이면 ASK(SELL) 주문까지 차단됨. 포지션을 줄여야 하는 주문이 막혀서 포지션이 계속 쌓임.
**원인**: `prepare_order_with_batch_exposure()`에서 `worst_case + batch + order_notional > max` 체크 시 주문 방향 구분 없이 모든 주문에 notional을 더함.
**수정**: BUY 주문만 `batch_contribution`에 누적. SELL은 0으로 처리. SHORT 포지션일 때는 반대로 SELL만 누적.
**손실**: 반복적으로 포지션 쌓임 → 수동 `--close` 필요.

---

### BUG-002: tox_score 부호 반전 — 역선택 심할수록 spread가 좁아짐
**커밋**: `0cae294`
**증상**: adverse selection이 심해질수록 spread가 넓어져야 하는데, 반대로 좁아짐.
**원인**: `tox_score_bps()`의 부호가 반전되어 feedback이 음수로 계산됨. spread 계산 시 tox_score를 빼야 하는데 더하거나, 반대 부호로 적용됨.
**수정**: 윈도우에서 부호 반전 수정.
**영향**: 봇 가동 내내 역선택 상황에서 spread를 오히려 좁혀서 손실 가속.

---

### BUG-003: Startup flatten 가격 precision 오류
**커밋**: `6233c80`
**증상**: 재시작 시 startup auto-flatten에서 "Invalid price precision (1125)" 에러로 청산 실패.
**원인**: `round_dp(1)`로 소수점 1자리 반올림 → tick_size=1인 시장에서 `73549.9` 같은 가격 생성됨.
**수정**: `round_dp(0)`으로 정수 반올림.

---

### BUG-004: Fair price에 Binance weight 적용 — 포지션 손실
**증상**: `binance_weight=0.7`로 fair price 계산 시 포지션 지속 손실.
**원인**: Binance mid를 직접 블렌딩하면 x10 orderbook 위치와 괴리 발생. 스프레드 내 주문이 잘못된 방향으로 편향됨.
**수정**: `fair_price = binance_mid`, `basis_offset = EWMA(x10_mid - binance_mid)`, `quote_price = fair_price + basis_offset` 방식으로 재설계.

---

## P1 버그 (기능 오작동)

### BUG-005: Ghost orders — 빈 WS 스냅샷으로 오더 초기화
**커밋**: `3d149f6`
**증상**: 간헐적으로 모든 오픈 오더가 사라진 것으로 인식 → 불필요한 재주문.
**원인**: 빈 WS 스냅샷(`bids=0, asks=0`)을 오더북 초기화로 처리.
**수정**: 빈 스냅샷 무시.

### BUG-006: WS thundering herd — 동시 스트림 연결로 rate limit 초과
**커밋**: `75513c3`
**증상**: 재시작 시 여러 WS 스트림이 동시 연결 → rate limit 에러.
**수정**: 스트림 연결 stagger + reconnect delay 추가.

### BUG-007: REST rate limit 미관리
**커밋**: `e619b62`
**증상**: 빠른 주문 제출 시 rate limit 초과 에러.
**수정**: proactive token bucket + fast cancel debounce 추가.

---

## 운영 이슈

### OPS-001: EC2 바이너리 아키텍처 불일치
**증상**: 맥(arm64)에서 빌드한 바이너리를 EC2(x86_64)에 올리면 "cannot execute binary file".
**수정**: `reqwest`/`tokio-tungstenite`를 `rustls`로 교체 후 `x86_64-unknown-linux-gnu` 크로스컴파일 설정. 맥에서 빌드 후 scp로 업로드.
**커밋**: `73ef943`

### OPS-002: 봇 중지 시 포지션/오더 미청산
**증상**: 봇 프로세스를 kill하면 거래소에 오픈 오더와 포지션이 그대로 남음.
**해결**: 항상 `--close` 먼저 실행 후 프로세스 종료. 봇 중지 = `--close` + `kill`.

---

## 데이터 / 분석

### DATA-001: markout 데이터 파일 미기록
**증상**: markout 계산이 메모리에서만 이루어지고 디스크에 저장 안 됨. 재시작 시 모든 markout 히스토리 소실.
**수정**: `markouts.jsonl` 추가. 각 fill의 horizon별 평가 시점에 한 줄씩 기록.
**커밋**: `f1a2ca8`

---

## 현재 관찰 중인 문제

### OBS-001: Adverse selection — markout 5s에 -1bps
**증상**: fills 후 5초 markout이 평균 -1.0~-1.1bps. spread 2.5bps 대비 크게 불리.
**상태**: tox_score 피드백으로 spread 자동 확대 중. 추가 튜닝 필요.
