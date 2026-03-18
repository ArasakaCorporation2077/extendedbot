# Extended Market Maker — Project Status

## Overview
Rust market-making bot for Extended Exchange (x10xchange) perpetual futures on Starknet.
8-crate workspace: types, crypto, exchange, orderbook, risk, strategy, paper, bot.
Mainnet only (testnet 코드 제거됨).

---

## 거래소 정보

### Extended Exchange (x10xchange)
- **구조**: 하이브리드 off-chain CLOB + StarkNet on-chain settlement
- **API docs**: https://api.docs.extended.exchange
  - Order book stream: https://api.docs.extended.exchange/#order-book-stream
  - Trades stream: https://api.docs.extended.exchange/#trades-stream
  - Candles stream: https://api.docs.extended.exchange/#candles-stream
  - Mark price stream: https://api.docs.extended.exchange/#mark-price-stream
  - Private WS streams: https://api.docs.extended.exchange/#private-websocket-streams
- **GitHub (참고용 Python SDK)**: https://github.com/x10xchange
- **서버 위치**: AWS Tokyo (ap-northeast-1)
- **Rate limit**: 1000 req/min (default tier), 429 시 exponential backoff
- **WS 주문 미지원**: REST only for order/cancel (WS는 읽기 전용)
- **Maker fee**: 0% (체결 로그 `payedFee: 0.000000` 확인됨)
- **서명**: SNIP12 Poseidon hashing + StarkEx ECDSA (rust-crypto-lib-base 공식 라이브러리)
- **24h 거래량**: ~$270M (BTC-USD 기준, 유동성 충분)

### Binance (Reference Price)
- **용도**: fair value 기준가 (x10 자체 orderbook mid는 유동성에 따라 왜곡 가능)
- **WS**: `wss://fstream.binance.com/ws/btcusdt@bookTicker` (Futures BBO)
- **데이터**: best bid/ask → binance_mid = (bid + ask) / 2

---

## 수익 모델

### 핵심 전략: 스프레드 캡처 (Market Making)
x10 orderbook에 양쪽(bid/ask) limit order를 게시하고, 체결 시 스프레드 차이를 수익으로 가져간다.

**수익 공식:**
```
수익/fill = (스프레드 / 2) × 체결 수량 - 수수료
```

**현재 설정 기준 예시:**
- base_spread = 4bps (양쪽 합산 8bps)
- order_size = $100, BTC 가격 $73,000 → 0.00137 BTC
- 체결 시 수익 ≈ $100 × 4bps = $0.04/fill (한쪽)
- **Maker fee = 0%** → 수수료 없음
- 양쪽 체결 시 round-trip 수익 ≈ $0.08

**수익을 결정하는 핵심 요소:**
1. **fill rate** — 얼마나 자주 체결되는가 (스프레드 좁을수록 fill 많지만 역선택 위험)
2. **markout** — 체결 후 가격이 유리하게 움직이는가 (양수 = 좋음, 음수 = 역선택)
3. **inventory risk** — 한쪽 포지션이 쌓여서 손실 보는 리스크

### Fair Value 계산

```
fair_price   = binance_mid          (즉시 반응)
basis_offset = EWMA(x10_mid - binance_mid, alpha=0.01)  (느린 추적)
quote_price  = fair_price + basis_offset  (호가 위치 = x10 orderbook 기준)
```

- **fast cancel**: binance_mid 기준으로 즉시 반응 (급변 시 즉시 취소)
- **호가 위치**: quote_price가 x10 orderbook에 정확히 위치
- basis_offset ≈ x10이 Binance보다 얼마나 비싼지 (예: +$20)
- Binance 없으면 local_mid 그대로 사용 (basis_offset=0)

### Markout 측정 (체결 품질 판단)
체결 후 가격이 어떻게 움직였는지 5개 시점에서 측정:

```
raw_markout = (future_x10_mid - fill_price) × direction
adjusted_markout = raw_markout - binance_market_movement
```

- **50ms, 200ms, 500ms, 1초, 5초** horizon
- **raw**: x10 mid 기준 (시장 전체 움직임 포함)
- **adjusted**: 바이낸스 움직임 차감 → 순수 체결 품질만 분리
- **양수** = 유리한 체결 (우리가 맞는 방향으로 가격 이동)
- **음수** = 역선택 (체결 직후 반대로 이동 = informed trader에게 당함)
- EWMA(alpha=0.2)로 평활화 → 5초 adjusted markout이 스프레드 피드백에 사용됨

**markout이 양수여야 수익이다.**

### 스프레드 동적 조절
```
spread = (base_spread + inventory_spread + markout_adj) × vpin_mult
```

- **base_spread**: 4bps (config)
- **inventory_spread**: 포지션 쏠림에 비례 (Avellaneda-Stoikov)
- **markout_adj**: 음수 markout → 스프레드 확대 (역선택 보호)
- **VPIN multiplier**: 독성 주문흐름 감지 시 1.5x~3x 확대
  - VPIN > 0.8 (Critical) → 3x
  - VPIN > 0.7 (High) → 2x
  - VPIN > 0.5 (Medium) → 1.5x

### Inventory Skew (포지션 관리)
Avellaneda-Stoikov 기반 비선형 skew:

```
skew = price_skew_bps × inventory_ratio² × fair_price
```

- Long 포지션 → 호가를 아래로 shift (팔기 쉽게)
- Short 포지션 → 호가를 위로 shift (사기 쉽게)
- Size skew: 반대편 주문 크기 키움 (unwind 유도)
- **Emergency flatten**: inventory 80% 넘으면 한쪽 호가 중단

### 리스크 관리
- **Circuit breaker**: 일일 손실 $500 → 봇 정지
- **Max position**: $500 (per market, skew 실효성 확보용)
- **Fast cancel**: 가격 3bps 이상 이동 시 즉시 취소 (1초 debounce)
- **Dead man's switch**: REST heartbeat 60초 간격, 실패 시 거래소가 전량 취소
- **Max order age**: 5초 초과 주문 자동 취소

### 레버리지 전략
- **설정**: 5배 레버리지
- **이유**: $100 주문 시 마진 $20만 필요 → 양쪽(bid+ask) $40 → 잔고 $104로 충분
- 1배면 $100 × 2 = $200 필요 → 잔고 부족으로 한쪽만 넣을 수 있음
- 거래소에 `PATCH /api/v1/user/leverage` 로 설정

---

## Completed

### Core Infrastructure
- [x] 8-crate workspace 구조 설계 + 빌드
- [x] Config 시스템 (default.toml + .env)
- [x] CLI (--paper, --smoke, --close 모드)
- [x] Graceful shutdown (Ctrl+C → mass cancel)
- [x] Dead man's switch (주기적 heartbeat)
- [x] Testnet 코드 제거, mainnet 전용

### Exchange Connectivity — REST API
- [x] API 인증 (X-Api-Key 헤더)
- [x] Account info → vault_id (l2Vault) 로딩
- [x] Market metadata → tick_size, size_step, collateral/synthetic resolution
- [x] Balance, positions, open orders 조회
- [x] Order creation (Stark ECDSA 서명 + settlement)
- [x] Cancel by exchange_id / external_id
- [x] Mass cancel (단일 REST로 전체 취소)
- [x] Leverage get/set API (`PATCH /api/v1/user/leverage`)
- [x] Rate limiter (token bucket + exponential backoff on 429)
- [x] HTTP connection pool warmup (4 concurrent connections)

### Exchange Connectivity — WebSocket
- [x] v1 개별 스트림 연결 (orderbooks, publicTrades, prices/mark)
- [x] Private WS: api.starknet.extended.exchange (X-Api-Key 인증)
- [x] Snapshot vs Delta 분리
- [x] Auto-reconnect with exponential backoff
- [x] JSON ping keepalive

### Binance Reference Price
- [x] Binance Futures bookTicker WS 연결
- [x] fair_price = binance_mid - EWMA(binance_mid - x10_mid)
- [x] Binance 급변 시 fast cancel 트리거 (requote는 x10 tick에서만)
- [x] BinanceBbo 이벤트에 `received_at: Instant` 포함 (latency 측정용)

### Latency Optimization
- [x] Signing warmup (Poseidon tables 초기화: 106ms → 2ms)
- [x] HTTP connection pool warmup
- [x] Hot path cancel: fair price 계산 직후, quote pipeline 전에 cancel 발사
- [x] Mass cancel 사용 (개별 cancel N회 → 1회)
- [x] select! fair scheduling (biased 제거로 timer starvation 해결)
- [x] tick_to_trade/cancel: 바이낸스 tick 기준 측정
- [x] Latency tracker: 7 metrics

### Crypto / Signing
- [x] StarkNet ECDSA 서명 (Poseidon hash + SNIP12)
- [x] rust-crypto-lib-base 공식 라이브러리 (get_order_hash + sign_message)
- [x] 로컬 서명 검증 후 전송
- [x] Domain: Perpetuals/v0/SN_MAIN/1
- [x] **메인넷 첫 체결 성공** (SELL 0.00138 BTC @ $72,484, maker fee $0)

### Strategy
- [x] Fair price calculator (Binance reference + EWMA basis tracking)
- [x] Spread calculator (base + volatility + VPIN + markout feedback)
- [x] Skew calculator (Avellaneda-Stoikov, nonlinear, emergency flatten)
- [x] Quote generator (multi-level, tick/step rounding, post-only guard)
- [x] VPIN calculator (volume-bucketed)
- [x] Fast cancel (BBO adverse detection)

### Risk Management
- [x] Position manager (per-market tracking, mark price updates)
- [x] Exposure tracker (worst-case: positions + pending orders)
- [x] Circuit breaker (daily loss $500, error rate, order rate, cooldown)
- [x] Markout tracker — raw + Binance-adjusted (5 horizons)
- [x] Adjusted markout → spread feedback

### Close Mode
- [x] Mass cancel → fetch positions → aggressive close (mark ± 0.5% slippage)
- [x] 체결 대기 polling (2초 간격, 최대 30초)

### EC2 Tokyo 배포
- [x] AWS ap-northeast-1 (t2.small), Amazon Linux 2023
- [x] Rust 빌드 환경 구축 (rustup + gcc + openssl-devel)
- [x] 바이너리 + config + .env 배포 완료

---

## 레이턴시 측정 결과

### 서울 → 도쿄 비교 (2026-03-16)

| 메트릭 | 서울 (로컬) | 도쿄 EC2 | 개선 |
|--------|------------|----------|------|
| order_rtt | 41ms | **7-17ms** | 2-6x |
| cancel_rtt | 39ms | **5ms** | 8x |
| tick_to_trade | 44ms | **18-22ms** | 2x |
| tick_to_cancel | 79ms | **10ms (p50)** | 8x |
| HTTP warmup | 162ms | **13ms** | 12x |

### 레이턴시 정의
- **tick_to_trade**: 바이낸스 bookTicker 수신 → x10 주문 REST 응답 (전체 end-to-end)
- **tick_to_cancel**: 바이낸스 bookTicker 수신 → x10 취소 REST 응답
- **order_rtt**: 순수 x10 REST 주문 왕복
- **cancel_rtt**: 순수 x10 REST 취소 왕복
- **order_to_fill**: 주문 전송(local_send) → WS FILLED 이벤트 수신 (체류시간 포함)
- **fill_delivery**: 거래소 체결시각(updatedTime) → 로컬 WS 수신 (시계 동기화 필요)
- **ws_confirm**: 주문 전송 → WS ORDER status=NEW 수신 (snapshot 오염 주의)

### x10 WS 타임스탬프 구조
- ORDER 이벤트: `createdTime` = 매칭엔진 접수, `updatedTime` = 체결/상태변경
- TRADE 이벤트: `createdTime` = 매칭엔진 체결 (ORDER의 updatedTime과 동일)
- envelope `ts` = updatedTime과 동일 (별도 server send timestamp 없음)
- fill은 별도 TRADE type이 아닌 **ORDER status=FILLED**로 옴 (TRADE도 직후에 옴)

### Markout 초기 데이터 (fill 2건 기준)
- 50ms: raw=+0.07bps, adj=+0.07bps
- 5s: raw=+0.72bps, adj=+0.72bps

---

## TODO — Pending (미완료)

### 즉시 필요
- [ ] **봇 재시작**: fair price 수정 + 빌드 완료 (2026-03-18), EC2에서 재시작 필요
- [ ] **포지션 모니터링**: --close로 청산 완료, 새 cycle 시작
- [ ] **fill 모니터링**: 100개 목표 (현재 ~14개)

### High Priority
- [ ] fills.jsonl 로깅 (오프라인 분석용)
- [ ] Markout 기반 동적 skew 강도 조절
- [ ] ws_confirm 메트릭에서 snapshot 이벤트 필터링
- [ ] 자금 추가 (현재 $104, 양쪽 호가에 부족)

### Medium Priority
- [ ] Prometheus 메트릭 export + Grafana 대시보드
- [ ] Multi-market 지원 (ETH-USD)
- [ ] systemd 자동 재시작
- [ ] 슬랙/텔레그램 알림
- [ ] 일별 PnL 리포트

### Low Priority
- [ ] SIMD JSON 파싱
- [ ] Kernel bypass (io_uring)
- [ ] Config hot-reload

---

## 핵심 버그 기록 (실제 손실 유발)

### BUG-001: Exposure tracker 방향성 미고려 — 포지션 쌓임 반복 (2026-03-18)
**커밋**: `110881b`, `77b5cac`
**증상**: LONG $400 쌓이면 ASK(SELL) 주문까지 차단됨. 포지션 줄이는 주문이 막혀 계속 쌓임. SHORT도 동일.
**원인**: `prepare_order_with_batch_exposure()`에서 BUY/SELL 구분 없이 `worst_case + order_notional > max` 체크.
**수정**: net_position 방향 기준으로 같은 방향 주문만 `batch_contribution`에 누적.
**손실**: 반복적 포지션 쏠림 → 수동 `--close` 필요, 약 $26 손실.

### BUG-002: tox_score 부호 반전 — 역선택 심할수록 spread 좁아짐 (2026-03-18)
**커밋**: `0cae294`
**증상**: adverse selection 심해질수록 spread가 넓어져야 하는데 반대로 좁아짐.
**원인**: tox_score 부호 반전 → feedback이 spread를 오히려 줄이는 방향으로 작용.
**영향**: 봇 가동 내내 역선택 상황에서 손실 가속.

### BUG-003: Startup flatten 가격 precision 오류 (2026-03-18)
**증상**: 재시작 시 auto-flatten에서 "Invalid price precision (1125)" 에러 → 청산 실패.
**원인**: `round_dp(1)` → tick_size=1 시장에서 소수점 가격 생성.
**수정**: `round_dp(0)`.

### BUG-004: markout 데이터 파일 미기록 (2026-03-18)
**커밋**: `f1a2ca8`
**증상**: markout 계산이 메모리에서만 이루어지고 재시작 시 전부 소실.
**수정**: `markouts.jsonl` 추가. horizon별 평가 시점마다 한 줄씩 기록.

---

## 발견된 이슈 & 해결 기록

### Binance Weighted Blend → 포지션 손실 (2026-03-18)
- **원인**: 코드리뷰 수정으로 fair_price = 0.7×binance + 0.3×local 적용
- **증상**: Binance mid > x10 mid → fair price 올라감 → bid 높게 → LONG 계속 체결 → LONG 0.00535 BTC 쌓임, unrealisedPnl -$2.26
- **해결**: binance_weight = 0.0으로 되돌림 (fair_price.rs `new()` 기본값)
- **교훈**: fair price를 단순 blend하면 두 거래소 가격차가 포지션 편향을 만든다. 사용하려면 basis EWMA 방식으로 해야 함.

### WS Thundering Herd → 429 루프 (2026-03-18)
- **원인**: 4개 WS 스트림이 동시에 Session timeout → 동시 재연결 → REST 폭발 (10+회/초)
- **해결**:
  1. Session timeout (Ok) 재연결에 2초 딜레이 추가 (websocket.rs)
  2. Err 경로 최소 backoff 1s → 2s
  3. Orchestrator에서 스트림 간 500ms 스태거 추가
- **결과**: Private WS 429는 초기 버스트 후 안정화됨

### Ghost Orders / 주문 쌓임 (2026-03-18)
- **원인1**: Private WS 재연결 시 빈 ORDER SNAPSHOT → 봇이 "주문 없음"으로 착각 → 새 주문 계속 넣음
- **원인2**: mass_cancel WS confirmation timeout → 주문이 tracker에 live로 남음 → 다시 cancel → 루프
- **해결**:
  1. 빈 ORDER SNAPSHOT 무시 (websocket.rs)
  2. cancel_all_live timeout 후 강제 Cancelled 처리
  3. PendingCancel 주문 스킵 (중복 cancel 방지)

### Fast Cancel 과다 호출 (2026-03-18)
- **원인**: fast_cancel_threshold(3bps) == update_threshold(3bps) → 매 orderbook tick마다 mass_cancel 트리거
- **해결**:
  1. fast_cancel에 1초 debounce 추가 (`last_fast_cancel: Option<Instant>`)
  2. Proactive token bucket rate limiter (16.67 req/sec, burst 30)

### best_price_tighten 하드코딩 (2026-03-18)
- **원인**: config에서 `best_price_tighten_enabled = true`인데 QuoteGenerator에 false 하드코딩
- **해결**: `with_best_price_tighten()` builder 메서드 추가, market_bot에서 config 값 전달

### Cancel → Place 갭 (2026-03-18)
- **원인**: cancel 후 WS confirmation 대기(200ms) → 불필요한 지연
- **해결**: WS wait 제거, 50ms 고정 sleep으로 단축

### Rate Limit 폭주 (2026-03-16)
- **원인**: 바이낸스 bookTicker가 초당 수십 회 → 매번 requote 트리거 → cancel(N회) + order(2회) = 초당 30+ REST
- **x10 rate limit**: 1000 req/min = 초당 ~16.7회
- **해결**:
  1. 바이낸스 핸들러에서 requote 제거, fast cancel만 유지
  2. min_requote_interval 100ms → 500ms
  3. 개별 cancel → mass_cancel (N회 → 1회)
  4. requote는 x10 orderbook tick에서만 발생

### Leverage 1배 문제 (2026-03-16)
- **증상**: $100 주문 시 마진 $100 잡힘 (leverage 미적용)
- **원인**: 거래소 leverage=1 상태, 봇에서 set_leverage 호출 안 함
- **해결**: `PATCH /api/v1/user/leverage` API 추가, 봇 시작 시 자동 설정
- **주의**: API 응답이 `{"data":[{...}]}` 배열 형식

### 잔고 부족 반복 (2026-03-16)
- **증상**: 한쪽 주문 체결 후 available $5 → 나머지 주문 전부 reject
- **원인**: leverage 1배 + $100 주문 → 마진 전액 사용
- **해결**: leverage 5배 → $100 주문에 마진 $20 → 양쪽 넣어도 $40

---

## Architecture

```
                    ┌──────────────┐
                    │ Binance WS   │ ✅ Connected
                    │ bookTicker   │
                    └──────┬───────┘
                           │ reference mid (fair value)
┌──────────────┐   ┌───────▼───────┐   ┌──────────────┐
│ Extended WS  │──▶│   MarketBot   │──▶│ Extended REST │
│ orderbook    │   │               │   │ create_order  │
│ trades       │   │ fair_price    │   │ mass_cancel   │
│ mark_price   │   │ spread_calc   │   │ set_leverage  │
│ account      │   │ skew_calc     │   └──────────────┘
└──────────────┘   │ quote_gen     │
                   │ markout(adj)  │   ┌──────────────┐
                   │ vpin          │   │ EC2 Tokyo    │
                   │ risk_mgmt     │   │ ap-northeast │
                   └───────────────┘   │ RTT ~5-17ms  │
                                       └──────────────┘
```

## Key Config (default.toml)
```toml
leverage = 5
order_size_usd = 100
base_spread_bps = 4.0
min_spread_bps = 2.5
max_spread_bps = 20.0
min_requote_interval_ms = 500
ewma_alpha = 0.01
update_threshold_bps = 3.0
fast_cancel_threshold_bps = 3.0
max_order_age_s = 5.0
max_position_usd = 500      # 5000→500 (skew 실효성 확보)
max_daily_loss_usd = 500
emergency_flatten_ratio = 0.8
binance_weight = 0.0        # 0.7→0.0 (local_mid only)
```

## Key Commits
```
42b86c0 Fix P0/P1 issues, tune strategy params, add Market Making Team agents
0d4b32c Update STATUS.md: profit model, exchange docs, issue history, detailed strategy docs
1486b90 Update STATUS.md with full project state for session handoff
3256ba1 Add Binance reference feed, latency optimization, adjusted markout, leverage control
b0bd972 Parallelize order submission + add latency instrumentation
```

## 이번 세션 변경사항 (2026-03-18)
- **fair_price.rs**: `fair_price=binance_mid`, `basis_offset=EWMA(x10-binance)`, `quote_price=fair+basis`
- **market_bot.rs**: QuoteInput에 quote_price 사용, fast cancel은 raw binance_mid 기준 유지
- **orchestrator.rs**: 시작 시 포지션 50% 초과 시 자동 flatten (재시작 시 ghost position 방지)
- **fill_logger.rs**: fills.jsonl 로깅 (ts, side, price, qty, fee, realized_pnl, fair_price, mid, binance_mid, order_to_fill_ms)
- **websocket.rs**: Session timeout 재연결 2초 딜레이 + 빈 ORDER SNAPSHOT 무시
- **orchestrator.rs**: WS 스트림 간 500ms 스태거 (thundering herd 방지)
- **rate_limiter.rs**: Reactive → Proactive token bucket (16.67 req/sec, burst 30)
- **market_bot.rs**: fast_cancel 1초 debounce, PendingCancel 스킵, cancel timeout 강제처리, best_price_tighten config 전달, cancel→place 갭 50ms로 단축
- **quote_generator.rs**: with_best_price_tighten() builder 추가
- **config/default.toml**: max_position_usd 5000→500, min_spread_bps 2.5

## Infra
- **EC2**: 3.112.37.210 (ap-northeast-1, t2.small)
- **SSH**: `ssh -i /Users/kimtaeyeon/Downloads/extendedMM.pem ec2-user@3.112.37.210`
- **Bot path**: `~/extendedMM/target/release/extended-mm`
- **Log**: `~/bot.log`
- **Sync**: `rsync -avz --exclude target --exclude .git -e "ssh -i ~/Downloads/extendedMM.pem" /Users/kimtaeyeon/extendedMM/ ec2-user@3.112.37.210:~/extendedMM/`
- **Build**: `cd ~/extendedMM && cargo build --release -p extended-bot`
- **Run**: `cd ~/extendedMM && nohup ./target/release/extended-mm >> ~/bot.log 2>&1 &`
- **GitHub**: https://github.com/ArasakaCorporation2077/extendedbot.git (private)
