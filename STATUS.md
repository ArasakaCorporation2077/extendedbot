# Extended Market Maker — Project Status

## Overview
Rust market-making bot for Extended Exchange (x10xchange) perpetual futures on Starknet.
8-crate workspace: types, crypto, exchange, orderbook, risk, strategy, paper, bot.
Mainnet only (testnet 코드 제거됨).

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

### Binance Reference Price ✅
- [x] Binance Futures bookTicker WS 연결 (`wss://fstream.binance.com/ws/btcusdt@bookTicker`)
- [x] fair_price = binance_mid - EWMA(binance_mid - x10_mid)
- [x] Binance 급변 시 fast cancel 트리거 (requote는 x10 tick에서만)
- [x] BinanceBbo 이벤트에 `received_at: Instant` 포함 (latency 측정용)

### Latency Optimization ✅
- [x] Signing warmup (Poseidon tables 초기화: 106ms → 2ms)
- [x] HTTP connection pool warmup
- [x] Hot path cancel: fair price 계산 직후, quote pipeline 전에 cancel 발사
- [x] Mass cancel 사용 (개별 cancel N회 → 1회)
- [x] select! fair scheduling (biased 제거로 timer starvation 해결)
- [x] tick_to_trade/cancel: 바이낸스 tick 기준 측정
- [x] Latency tracker: 7 metrics (tick_to_trade, tick_to_cancel, cancel_rtt, order_rtt, ws_confirm, fill_delivery, order_to_fill)

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
- [x] Markout tracker — raw + Binance-adjusted (5 horizons: 50/200/500/1000/5000ms)
- [x] Adjusted markout → spread feedback (역선택 감지 시 스프레드 확대)

### Close Mode
- [x] Mass cancel → fetch positions → aggressive close (mark ± 0.5% slippage)
- [x] 체결 대기 polling (2초 간격, 최대 30초)

### EC2 Tokyo 배포 ✅
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

### Markout 초기 데이터 (fill 2건 기준)
- 50ms: raw=+0.07bps, adj=+0.07bps
- 5s: raw=+0.72bps, adj=+0.72bps

---

## TODO — Pending (미완료)

### 즉시 필요
- [ ] **Leverage 5배 EC2 배포**: API 파싱 수정 완료, EC2에 미반영
- [ ] **1시간 테스트 실행**: markout 데이터 수집 + 수익 모델 검증
- [ ] **Rate limit 수정 배포**: min_requote_interval 500ms + mass_cancel

### High Priority
- [ ] fills.jsonl 로깅 (오프라인 분석용)
- [ ] Markout 기반 동적 skew 강도 조절
- [ ] ws_confirm 메트릭에서 snapshot 이벤트 필터링

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

## Architecture

```
                    ┌──────────────┐
                    │ Binance WS   │ ✅ Connected
                    │ bookTicker   │
                    └──────┬───────┘
                           │ reference mid
┌──────────────┐   ┌───────▼───────┐   ┌──────────────┐
│ Extended WS  │──▶│   MarketBot   │──▶│ Extended REST │
│ orderbook    │   │               │   │ create_order  │
│ trades       │   │ fair_price    │   │ mass_cancel   │
│ mark_price   │   │ spread_calc   │   │ set_leverage  │
│ account      │   │ skew_calc     │   └──────────────┘
└──────────────┘   │ quote_gen     │
                   │ markout(adj)  │   ┌──────────────┐
                   │ risk_mgmt     │   │ EC2 Tokyo    │
                   └───────────────┘   │ ap-northeast │
                                       │ RTT ~5-17ms  │
                                       └──────────────┘
```

## Key Config (default.toml)
```toml
leverage = 5
order_size_usd = 100
base_spread_bps = 4.0
min_requote_interval_ms = 500
max_position_usd = 5000
max_daily_loss_usd = 500
```

## Key Commits
```
3256ba1 Add Binance reference feed, latency optimization, adjusted markout, leverage control
b0bd972 Parallelize order submission + add latency instrumentation
53a60f5 Code review fixes: verify signature before send, normalize key comparison
4ce23b6 Fix 1101: add 14-day expiry buffer + use official sign_message
458a8ac Use x10 official rust-crypto-lib-base for order hash
```

## Infra
- **EC2**: 3.112.37.210 (ap-northeast-1, t2.small)
- **SSH**: `ssh -i extendedMM.pem ec2-user@3.112.37.210`
- **Bot path**: `~/extended-mm/target/release/extended-mm`
- **x10 rate limit**: 1000 req/min (default tier)
- **x10 WS**: REST only for orders (WS 주문 미지원)
