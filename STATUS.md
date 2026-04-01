# Extended Market Maker — Project Status (2026-04-01)

## 현재 상태
- **마켓**: ETH-USD (이전: TAO-USD, BTC-USD, WIF-USD, CRCL_24_5-USD)
- **EC2**: 54.199.221.223 (ap-northeast-1, t2.small)
- **이전 EC2 IP**: 3.112.37.210 (변경됨)
- **잔고**: ~$53 (시작 $104, 총 손실 ~$51)
- **봇 상태**: 정지됨, 포지션 없음
- **`--close`**: `./target/release/extended-mm --close`로 포지션 종료 가능
- **SSH**: `ssh -i extendedMM.pem ec2-user@54.199.221.223`
- **Bot path**: `~/extendedMM/target/release/extended-mm`
- **Auto-restart**: `run.sh` (nohup wrapper, watchdog 3분)
- **GitHub**: https://github.com/ArasakaCorporation2077/extendedbot.git (private)

---

## 코드 구조 (8-crate workspace)

```
extended-types/     — 공유 타입 (Decimal, Side, OrderStatus, Config, BotEvent)
extended-crypto/    — Pedersen hash, ECDSA signing for Starknet (mainnet only)
extended-exchange/  — REST client + WebSocket client + Binance WS + OrderTracker
extended-orderbook/ — BTreeMap 기반 local orderbook
extended-risk/      — PositionManager, ExposureTracker, CircuitBreaker, MarkoutTracker, LatencyTracker
extended-strategy/  — FairPriceCalculator, SpreadCalculator, SkewCalculator, QuoteGenerator, VpinCalculator, TradeFlowTracker, DepthImbalanceTracker
extended-paper/     — Paper trading 시뮬레이션
extended-bot/       — MarketBot (메인 루프), Orchestrator (시작/종료), State, FillLogger
```

### 핵심 파일 & 역할

**crates/extended-bot/src/market_bot.rs** — 메인 트레이딩 루프
- `handle_event()`: 모든 이벤트(orderbook, trade, binance, fill, order update) 처리
- `on_orderbook_update()`: HOT PATH — orderbook 업데이트 → fair price → cancel → requote
- `requote()`: spread 계산 → skew 계산 → 필터(basis, time, inventory) → quote 생성 → 주문 제출
- `cancel_all_live()`: mass_cancel REST 1회로 전체 취소
- `on_fill()`: PnL 계산, fills.jsonl 기록, markout 등록

**crates/extended-bot/src/orchestrator.rs** — 시작/종료/이벤트 루프
- 봇 초기화: signer warmup → HTTP pool warmup → market config → leverage 설정 → state bootstrap
- WS 스폰: orderbook, trades, markPrice, private, binance(bookTicker, aggTrade, depth20)
- 메인 select! 루프: event_rx, cleanup(30s), reconcile(60s), markout tick(50ms), DMS heartbeat, watchdog(60s)
- watchdog: 3분 idle → exit(1) → run.sh가 재시작

**crates/extended-strategy/src/fair_price.rs** — Fair Value 계산
```
fair_price   = binance_mid          (즉시 반응, fast cancel 기준)
basis_offset = EWMA(x10_mid - binance_mid, alpha=0.01)  (느린 추적)
quote_price  = fair_price + basis_offset  (호가 위치 = x10 orderbook 기준)
```
- `update_local_mid()`: x10 orderbook mid 업데이트
- `update_reference_mid()`: 바이낸스 mid 업데이트
- `quote_price()`: fair_price + basis_offset (QuoteInput에 전달)

**crates/extended-strategy/src/spread.rs** — 스프레드 계산
```
spread = (base_spread + vol × sensitivity) × vpin_mult + inventory_spread + markout_adj
clamped to [min_spread, max_spread]
```
- `volatility_bps`: VolatilityEstimator (바이낸스 BBO mid 기반, 500 samples)
- `vpin_multiplier`: VPIN > 0.85 sustained 20+ bars → 3x
- `inventory_spread`: |inventory_ratio| × 2.0
- `markout_adj`: tox_score (max(0,-raw_500ms) + max(0,-adj_5s))

**crates/extended-strategy/src/skew.rs** — Avellaneda-Stoikov 기반 스큐
```
skew = price_skew_bps × inventory_ratio² × mid_price
Long → 호가 아래로 shift (팔기 쉽게)
Short → 호가 위로 shift (사기 쉽게)
Size skew: 반대편 크기 키움
Emergency flatten: 80% → 한쪽 중단
```

**crates/extended-strategy/src/quote_generator.rs** — 호가 생성
```
bid = quote_price × (1 - half_spread) + skew_offset
ask = quote_price × (1 + half_spread) + skew_offset
```
- `best_price_tighten`: BBO에서 margin_bps 뒤에 서기 (현재 1.0bps)
- `post_only_no_cross`: 절대 taker 안 됨
- `num_levels`: 다중 레벨 (size_step×10 미만이면 자동 스킵)
- 포지션 있을 때 unwind margin = 0 (BBO 최우선)

**crates/extended-strategy/src/trade_flow.rs** — 바이낸스 aggTrade 기반 buy/sell 불균형
- 5초 rolling window, imbalance [-1, +1]
- sensitivity 0.5bps (regression 최적값)

**crates/extended-strategy/src/depth_imbalance.rs** — 바이낸스 depth20 top 3 level 불균형
- EWMA alpha=0.3, imbalance [-1, +1]
- sensitivity 0.3bps (regression 최적값)

**crates/extended-strategy/src/vpin.rs** — Volume-Synced Probability of Informed Trading
- 바이낸스 aggTrade 기반 (x10이 아님)
- bucket 5.0, 50 buckets
- sustained toxic: threshold 0.85, 20+ bars → spread 3x
- spread_multiplier: Low 1x, Medium 1.5x, High 2x, Critical/Sustained 3x

**crates/extended-risk/src/markout.rs** — 체결 품질 측정
```
raw = (future_x10_mid - fill_price) × direction
adj = raw - binance_market_movement
```
- 5개 horizon: 50, 200, 500, 1000, 5000ms
- EWMA alpha=0.2
- tox_score = max(0, -raw_500ms) + max(0, -adj_5s)
- feedback_bps → spread 확대에 사용 (항상 ≥ 0, 축소 안 함)
- markouts.jsonl 기록

**crates/extended-exchange/src/binance_ws.rs** — 바이낸스 WS 클라이언트
- 3개 스트림: bookTicker(BBO), aggTrade(체결), depth20(오더북 top 20)
- `from_market()`: x10 심볼 → 바이낸스 심볼 자동 매핑 (날짜 접미사 제거)
- depth20: top 3 level만 합산 (research: top-of-book이 예측력 높음)

**crates/extended-exchange/src/websocket.rs** — x10 WS 클라이언트
- 호스트: `wss://api.starknet.extended.exchange` (NOT app.extended.exchange)
- 4개 스트림: orderbook, trades, markPrice, private(account)
- session timeout ~15초 → 5초 후 재연결
- max backoff 60초

**crates/extended-exchange/src/rest.rs** — x10 REST 클라이언트
- nonce: unix_timestamp % 1B (재시작 시 중복 방지)
- rate limiter: proactive token bucket (16.67 req/sec, burst 30)
- leverage get/set API
- mass_cancel (단일 REST로 전체 취소)
- cancel-to-place sleep: 5ms

**crates/extended-bot/src/fill_logger.rs** — fills.jsonl 기록
- ts, market, side, price, qty, fee, is_maker, realized_pnl
- fair_price, local_mid, binance_mid, order_to_fill_ms
- flow_imbalance, depth_imbalance (regression용)

---

## 거래소 정보

### x10 (Extended Exchange)
- **구조**: 하이브리드 off-chain CLOB + StarkNet on-chain settlement
- **API docs**: https://api.docs.extended.exchange
- **서버**: AWS Tokyo (ap-northeast-1)
- **Rate limit**: 1000 req/min (default), 4000/8000/12000 상위 티어
- **Maker fee**: 0% | Taker fee: 0.025%
- **Maker rebate**: ≥0.5% market share → 0.002% (현재 미달, 일일 $1.35M 필요)
- **WS 주문 미지원**: REST only for order/cancel
- **WS 호스트**: `wss://api.starknet.extended.exchange` (app.extended.exchange는 UI용, WAF 차단됨)
- **REST 호스트**: `https://api.starknet.extended.exchange`
- **WS session timeout**: ~15초마다 끊김 → 자동 재연결
- **TCP RTT**: 11ms (ELB 포함, 같은 AZ면 0.5ms지만 ELB 경유)
- **서명**: SNIP12 Poseidon hash + StarkEx ECDSA, domain: Perpetuals/v0/SN_MAIN/1

### Binance (Reference Price)
- bookTicker: `wss://fstream.binance.com/ws/{symbol}@bookTicker` (BBO, 초당 30+회)
- aggTrade: `wss://fstream.binance.com/ws/{symbol}@aggTrade` (체결, VPIN + trade flow)
- depth20: `wss://fstream.binance.com/ws/{symbol}@depth20@100ms` (오더북 depth)
- 심볼 매핑: `TAO-USD` → `taousdt`, `CRCL_24_5-USD` → `crclusdt` (날짜 접미사 자동 제거)

### 조사한 다른 거래소
- **gTrade (Gains)**: 오더북 없음(AMM), 수수료 5bps, 온체인 주문 → MM 불가
- **Reya**: 수수료 4bps (검색 결과 0%는 오류), 100ms 블록, REST/WS API → x10보다 비쌈

---

## 수익 모델

### 핵심: 스프레드 캡처 (Market Making)
```
수익/fill = 캡처 스프레드(bps) - markout 손실(bps)
캡처 스프레드 ≈ best_price_margin_bps (현재 1.0bps)
```

### 문제: BTC-USD에서 수익 전환 실패
- fill당 -$0.005 → 거의 손익분기지만 양수 전환 못 함
- 원인: informed trader와 같은 RTT(11ms), 시그널 edge 없음 (R²=0.03)
- 현재: TAO-USD로 이동 (스프레드 넓고 경쟁 적은 마켓)

### 해결 방향: edge 확보 필요
파라미터 튜닝만으로는 한계. 수익 전환하려면 다음 중 하나 이상 필요:
1. **Fair price edge** — 바이낸스 mid보다 나은 가격 예측. microprice, VAMP, 다중 거래소 리드-래그
2. **시그널 edge** — R²=0.03을 높일 feature 발굴. order flow conditional on fill, regime detection
3. **구조적 edge** — 포인트 파머 상대 MM, funding rate 캐리, 경쟁 적은 마켓에서 스프레드 캡처
4. **실행 edge** — 큰 resting order 뒤에 서기 (order placement), fill 후 즉시 unwind

현재 3번(구조적 edge) 방향으로 TAO-USD 테스트 중. 데이터 모아서 1-4번 중 가능한 것 찾기.

---

## 마켓별 성과 기록

### BTC-USD (1,777 fills, 시작 $104)
- **총 PnL**: -$9.50 (fill당 -$0.005)
- **markout (5s adj)**: -1.41bps → 최적화 후 -0.34bps
- **win rate**: 10%
- **결론**: 역선택 심함. 파라미터 튜닝으로 손익분기 근처까지 개선했지만 수익 전환 못 함

### WIF-USD (11 fills)
- **markout (5s adj)**: -7.91bps
- **결론**: 유동성 없어서 역선택 극심. basis ±22bps. 즉시 폐기

### CRCL_24_5-USD (3 fills)
- **markout (5s adj)**: -1.44bps
- **결론**: WS 호스트 잘못 사용(app → api) + 주식 perp 유동성 부족

### TAO-USD (진행 중)
- spread 7.6bps, 볼륨 $4.5M, V/OI 4.4x
- 바이낸스 TAOUSDT vol $675M
- num_levels=2 min size 문제 해결 (자동 스킵), nonce 문제 해결 (timestamp 기반)

---

## 레이턴시 (도쿄 EC2)

| 메트릭 | 값 | 설명 |
|--------|-----|------|
| order_rtt | 7-17ms | 순수 REST 왕복 |
| cancel_rtt | 5ms | mass_cancel REST |
| cancel-to-place sleep | 5ms | cancel 후 대기 |
| total requote cycle | ~27ms | binance tick → order response |
| TCP RTT to x10 | 11ms | ELB 포함 |
| binance_age | 0.2-2ms | BBO tick 나이 |
| compute | 0.02ms | fair price + spread 계산 |

### 레이턴시 breakdown (debug 로그)
```
binance tick → event queue(0.2ms) → orderbook apply(0.02ms) → fair price(0.01ms)
→ cancel(5ms RTT + 5ms sleep) → spread/skew/quote(0.1ms) → order REST(7-17ms)
= total ~27ms
```

---

## 필터 & 보호 로직

### Basis filter (active_side == Both일 때만)
- basis > +3bps → buy 차단 (AskOnly)
- basis < -2bps → sell 차단 (BidOnly)
- inventory skew(AskOnly/BidOnly)가 이미 설정됐으면 오버라이드 안 함

### Time filter (11-14 UTC, KST 20-23시)
- spread에 +2bps 추가 (additive, 곱하기 아님)
- inventory_spread는 영향 안 받음

### Unwind acceleration
- 포지션 > 10% inventory ratio → margin = 0 (BBO 최우선)
- 포지션 없으면 → margin = config 값 (1.0bps)

### Watchdog
- 60초마다 체크
- 2분 idle → 경고 로그
- 3분 idle → emergency mass_cancel + exit(1) → run.sh 재시작

### Circuit breaker
- daily loss $500 → 봇 정지
- max_orders_per_minute 300
- max_errors_per_minute 10

---

## Config 파라미터 설명 (default.toml)

```toml
[trading]
market = "TAO-USD"              # 거래 마켓
order_size_usd = 40.0           # 주문 크기 (USD)
min_order_usd = 35.0            # 최소 주문 (동적 사이징 하한)
max_order_usd = 50.0            # 최대 주문 (동적 사이징 상한)
leverage = 5                    # 거래소 레버리지 (시작 시 자동 설정)

expiry_days = 7                 # 주문 만료 (GTT)
dead_man_switch_timeout_ms = 60000  # DMS heartbeat 간격

# Fair price
ewma_alpha = 0.01               # basis_offset EWMA 속도 (느림)
update_threshold_bps = 3.0       # requote 트리거 threshold
min_requote_interval_ms = 500    # requote 최소 간격

# Spread
base_spread_bps = 4.0           # 기본 스프레드
min_spread_bps = 2.0            # 최소 스프레드
max_spread_bps = 20.0           # 최대 스프레드
volatility_sensitivity = 0.5     # vol → spread 가중치
latency_vol_multiplier = 2.0     # 레이턴시 기반 spread floor
markout_sensitivity = 0.5        # tox_score → spread 가중치

# Skew
price_skew_enabled = true
price_skew_bps = 7.0            # inventory ratio당 skew 강도
size_skew_enabled = true
size_skew_factor = 1.0
min_size_multiplier = 0.2
max_size_multiplier = 1.8
emergency_flatten_ratio = 0.8    # 이 이상이면 한쪽 quoting 중단

# VPIN (바이낸스 aggTrade 기반)
vpin_bucket_volume = 5.0         # 버킷 크기 (TAO 기준)
vpin_num_buckets = 50            # rolling window
# 코드: threshold 0.85, sustained_bars 20

# Multi-level quoting
num_levels = 2                   # 호가 레벨 수
level_spacing_bps = 3.0          # 레벨 간 간격
level_size_decay = 0.7           # 레벨별 크기 감소 (자동 min size 체크)

# Fast cancel
fast_cancel_threshold_bps = 3.0  # 가격 이동 시 즉시 취소
max_order_age_s = 5.0            # 5초 초과 주문 취소

# Best price tighten
best_price_tighten_enabled = true
best_price_margin_bps = 1.0      # BBO에서 이 거리 뒤에 서기

# Trade flow / depth imbalance (regression 최적값)
trade_flow_sensitivity_bps = 0.5  # flow imbalance → fair price shift
depth_imbalance_sensitivity_bps = 0.3  # depth imbalance → fair price shift

# Inventory thresholds
one_side_inventory_ratio = 0.45
hard_one_side_inventory_ratio = 0.70

[risk]
max_position_usd = 50.0         # 최대 포지션 (TAO용)
max_daily_loss_usd = 500.0      # 일일 손실 한도
max_orders_per_minute = 300
max_errors_per_minute = 10
stale_price_s = 5.0
cooldown_s = 60
```

---

## 핵심 버그 기록 (실제 손실 유발)

### BUG-001: Exposure tracker 방향성 미고려 (2026-03-18)
포지션 줄이는 주문까지 차단. 수정: 같은 방향만 차단.

### BUG-002: tox_score 부호 반전 (2026-03-18)
역선택 시 스프레드 축소. 수정: `-feedback_bps` → `feedback_bps`.

### BUG-003: WS 호스트 잘못 사용 (2026-03-27)
`app.extended.exchange`(UI용) → CloudFront WAF 차단. 수정: `api.starknet.extended.exchange`.

### BUG-004: Toxic hour spread 곱하기 (2026-03-25)
VPIN 3x × toxic hour 2x = 6x. 수정: additive +2bps.

### BUG-005: Basis filter가 inventory skew 오버라이드 (2026-03-25)
Long인데 basis가 AskOnly→BidOnly 전환. 수정: Both일 때만 적용.

### BUG-006: num_levels=2 min size 미달 (2026-03-28)
TAO 2nd level qty < min trade size → reject storm. 수정: size_step×10 미만 자동 스킵.

### BUG-007: consecutive_rejects 영구 백오프 (2026-03-25)
3+ rejects → 10초 무한 루프. 수정: 2초 + 5회 리셋.

### BUG-008: 봇 silent death (2026-03-26)
WS 끊김 → 6시간 방치. 수정: watchdog 3분 → exit(1) + run.sh 재시작.

### BUG-009: VPIN sustained toxic 과민 (2026-03-28)
TAO VPIN 상시 0.7+ → spread 3x 영구. 수정: threshold 0.85, bars 20.

### BUG-010: Nonce 재시작 중복 (2026-03-28)
재시작마다 nonce=1 → "Duplicate Order". 수정: unix_timestamp % 1B.

---

## 데이터 분석 결과 (BTC-USD 1,777 fills)

### 시간대별 markout
- 06-08 UTC: adj -0.77bps, win 15-20% ← 최선
- 11-14 UTC: adj -1.7bps, win 7% ← 최악

### order_to_fill vs markout
- <100ms fill: adj -1.68bps, win 4% ← 빠른 fill = informed trader
- 2s-10s fill: adj -1.07bps, win 14% ← 느린 fill = 일반 trader

### basis vs markout
- basis 0~2bps: adj -1.02bps ← 가장 나음
- basis >5bps + buy: adj -3.10bps ← 최악

### buy vs sell
- sell이 buy보다 1.44bps 나쁨 (regression)

### Regression (R²=0.03, 효과 미미)
- flow_imbalance: 최적 0.18bps (현재 0.5)
- depth_imbalance: 최적 0.11bps (현재 0.3)
- 결론: flow/depth 시그널이 이 환경에서 예측력 없음

---

## 참고 자료 & 학습

### 핵심 교훈
1. **"올바른 테이블"이 실력보다 중요** — BTC-USD는 프로 테이블
2. **속도 경쟁 이길 수 없으면 피해라** — 11ms RTT 변경 불가
3. **파라미터 튜닝은 한계** — edge 없으면 ±$0 맴돎
4. **마켓별 제약 반드시 확인** — min order size, tick size, VPIN 특성
5. **WS 호스트 구분** — API vs UI 엔드포인트
6. **봇 재시작 전 반드시 --close + 확인** — 포지션 쌓임 방지

### 읽은 글 & 참고
- Quant Roadmap: fair value fitting, SVI, regressions, Wing Model
- beatzxbt/smm: Bollinger Band spread, 2단계 주문, inventory extreme
- gamma-ray: Avellaneda-Stoikov (reservation price + optimal spread)
- VisualHFT: VPIN, LOB imbalance, Market Resilience 구현
- HFT Advisory: VPIN sustained, LOB multi-level, order placement
- Liquidity Goblin 팟캐스트: 올바른 테이블, 좋은 다리, funding rate, 포인트 파머
- 0xDub: kernel tuning, rust channel benchmark
- MM 리서치 논문: PULSE, 다중 거래소 리드-래그, VAMP

---

## 2026-03-31~04-01 세션 변경사항

### 아키텍처 변경 (converge 패턴)
- **cancel-all+place-all → converge** (gamma-ray 기반): 매번 전부 취소하는 대신 바뀐 주문만 교체
- **cancel-replace 도입 후 제거**: x10 cancel_id가 WS CANCELLED 25초 지연 → ghost 원인. 개별 cancel + new order로 변경
- **aggressive/reducing 분리** (beatzxbt 기반): edge 있을 때만 진입, reducing은 시간 따라 공격적
- **이벤트 드리븐 requote**: 타이머 기반 → 가격 변화 기반
- **basis-adjusted edge**: 구조적 basis(-8.5bps) 보정하여 양쪽 대칭 edge 계산

### 버그 수정
- BUG-1: PendingCancel 주문이 exposure 이중계산 → 필터 추가
- BUG-2: cancel 전 신규 주문 제출 → cancel 먼저 실행, 확인 후 신규 주문
- BUG-3: RocGuard 미연결 → 재연결 (30bps/10s → 15s pause)
- Basis filter: quote_price 대신 x10_mid 사용, TAO 구조적 basis로 비활성화
- VPIN tuning: bucket 5→50, threshold 0.85→0.92, bars 20→30
- OrderResponse parse: camelCase rename + 누락 필드(cancelledQty 등)
- Position sync: 빈 WS snapshot → position 0으로 리셋
- Startup: mass_cancel 확인 루프 + REST position 최종 동기화
- WS ORDER snapshot sync: 20초마다 ghost order 정리
- Stale tracker 감지: >30초 ghost → 강제 정리
- Unknown fill: REST position 재조회

### 현재 Config
```toml
market = "ETH-USD"
order_size_usd = 30.0
max_position_usd = 80.0
aggressive_edge_bps = 3.0      # basis-adjusted
reducing_max_spread_bps = 4.0  # 시작 spread
reducing_min_spread_bps = 2.0  # 최소 spread (decay 후)
reducing_decay_s = 30.0
update_threshold_bps = 3.0
min_requote_interval_ms = 100  # 디바운스만
max_order_age_s = 300.0        # 사실상 비활성화
fast_cancel_threshold_bps = 3.0
roc_window_ms = 10000
roc_threshold_bps = 30.0
roc_pause_ms = 15000
```

### Markout 데이터 (ETH-USD, 3bps edge)
```
Horizon    Raw      Adj
50ms      -0.38    -0.32
200ms     -0.71    -0.56
500ms     -0.94    -0.55
1000ms    -1.29    -0.78
5000ms    -1.05    -1.47
```
- 전반적으로 마이너스 (역선택)
- adj가 raw보다 나음 (바이낸스 보정 효과)
- sell이 buy보다 나쁨 (reducing sell이 trending 시 손해)

### 미해결 문제
1. **Markout 음수**: 파라미터 튜닝 필요 (reducing spread, margin, fast cancel)
2. **Position manager 오염**: auto-flatten 후 리셋은 고쳤지만 런타임 중 WS snapshot 누락 시 불일치 가능
3. **REST reconcile 간헐적 실패**: WS snapshot sync로 보완했지만 완전하지 않음

### 읽은 글 추가
- Nanex 리서치: quote stuffing, fantaseconds, HFT manipulation, momentum ignition
- beatzxbt 블로그: aggressive/reducing 분리, micro alt MM 전략
- gamma-ray (hello2all): converge 패턴, quotes_are_same gate, AS 구현
- hummingbot: hanging orders, order_refresh_tolerance, filled_order_delay
- Crypto Chassis: 방어적 MM, ROC guard, skew sniffer

## TODO
- [ ] markout 양수 전환: reducing spread/margin/fast cancel 튜닝
- [ ] BUG-4: WsConnected 후 position 재동기화
- [ ] BUG-5~7: VPIN threshold 조정, markout→호가 차단 연동
- [ ] 미시 모멘텀 필터: 바이낸스 1초 내 2bps 이상 움직이면 해당 방향 호가 철수
- [ ] markout feedback → aggressive edge threshold 동적 조절
- [ ] 멀티마켓 동시 운영
- [ ] /plan-eng-review (gstack) 실행
