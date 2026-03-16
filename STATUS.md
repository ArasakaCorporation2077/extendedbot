# Extended Market Maker — Project Status

## Overview
Rust market-making bot for Extended Exchange (x10xchange) perpetual futures on Starknet.
8-crate workspace: types, crypto, exchange, orderbook, risk, strategy, paper, bot.

---

## Completed

### Core Infrastructure
- [x] 8-crate workspace 구조 설계 + 빌드
- [x] Config 시스템 (default.toml / testnet.toml + .env 환경 분리)
- [x] CLI (--config, --paper, --smoke 모드)
- [x] Graceful shutdown (Ctrl+C → mass cancel)
- [x] Dead man's switch (주기적 heartbeat)

### Exchange Connectivity — REST API
- [x] API 인증 (X-Api-Key 헤더)
- [x] 응답 래퍼 파싱 (`{"status":"OK","data":...}`)
- [x] Account info → vault_id (l2Vault) 로딩
- [x] Market metadata → tick_size, size_step, collateral/synthetic resolution
- [x] Balance, positions, open orders 조회
- [x] Order creation (Stark ECDSA 서명 + settlement)
- [x] Cancel by exchange_id / external_id
- [x] Mass cancel
- [x] Environment-specific .env (.env.mainnet, .env.testnet)
- [x] Stark private key 직접 사용 (from_stark_private_key)

### Exchange Connectivity — WebSocket
- [x] v1 개별 스트림 연결 (orderbooks, publicTrades, prices/mark)
- [x] 호스트: app.extended.exchange (메인넷), starknet.sepolia... (테스트넷)
- [x] Private WS: api.starknet.extended.exchange (X-Api-Key 인증)
- [x] ?keepAlive=true 파라미터 + JSON ping keepalive
- [x] Snapshot vs Delta 분리 (SNAPSHOT → apply_snapshot, DELTA → apply_delta)
- [x] Orderbook level: c 필드 (absolute size) 우선, q fallback
- [x] Sequence number gap detection
- [x] Auto-reconnect with exponential backoff (session timeout 대응)

### Orderbook
- [x] BTreeMap 기반 local orderbook (bid desc, ask asc)
- [x] Snapshot/Delta 적용
- [x] Mid price, best bid/ask, spread 계산
- [x] Sequence gap detection + resync 요청

### Crypto / Signing ✅ RESOLVED
- [x] StarkNet ECDSA 서명 (Poseidon hash + SNIP12)
- [x] rust-crypto-lib-base 공식 라이브러리 통합 (get_order_hash + sign_message)
- [x] 14일 expiry buffer (exchange adds ceil(ms/1000) + 14*86400 to hash)
- [x] Deterministic k via ecdsa_sign (RFC 6979)
- [x] 로컬 서명 검증 (verify_ok = true 확인 후 전송)
- [x] l2Key vs derived public key 비교 검증
- [x] Domain: Perpetuals/v0/SN_MAIN(or SN_SEPOLIA)/1
- [x] Stark key derivation from ETH key (grind_key) + direct private key
- [x] AtomicU64 vault_id (REST 로딩 후 업데이트)
- [x] **메인넷 첫 체결 성공** (SELL 0.00138 BTC @ $72,484, maker fee $0)

### Strategy
- [x] Fair price calculator (local mid, EWMA basis for external ref)
- [x] Spread calculator (base + volatility + VPIN + markout feedback)
- [x] Skew calculator (Avellaneda-Stoikov reservation price shift, nonlinear, emergency flatten)
- [x] Quote generator (multi-level, tick/step rounding, post-only guard)
- [x] VPIN calculator (volume-bucketed, buy/sell classification)
- [x] Fast cancel (BBO adverse detection)

### Risk Management
- [x] Position manager (per-market tracking, mark price updates)
- [x] Exposure tracker (worst-case: positions + pending orders)
- [x] Circuit breaker (daily loss, error rate, order rate, cooldown)
- [x] Markout tracker (5 horizons: 50ms/200ms/500ms/1s/5s, EWMA)
- [x] Markout → spread feedback (negative markout widens spread)

### Paper Trading
- [x] PaperExchange adapter (no HTTP, simulated fills)
- [x] Fill simulation against market BBO
- [x] Position tracking + realized PnL
- [x] Reduce-only constraint

### Order Tracking
- [x] Dual-map lookup (external_id ↔ exchange_id)
- [x] Ghost order detection + cleanup
- [x] State transition validation (reject invalid)
- [x] Pending exposure calculation

### Code Quality
- [x] Custom review agents (.claude/agents/)
- [x] 82+ unit tests passing
- [x] 4 rounds of code review + bug fixes

---

## TODO — High Priority

### Positions API 파싱 수정
- [ ] GET /api/v1/user/positions 응답 파싱 실패 수정
- [ ] 부트스트랩 시 기존 포지션 로딩

### Binance Reference Price (fair value 핵심)
- [ ] Binance BTC/USDT bookTicker WS 연결
- [ ] fair_price_calc.update_reference_mid() 연결
- [ ] EWMA basis tracking (Binance - Extended 스프레드)
- [ ] Binance 급변 시 fast cancel 트리거

### Markout 데이터 수집
- [ ] 실제 체결 후 markout curve 기록
- [ ] fills.jsonl 로깅 (오프라인 분석용)
- [ ] horizon별 markout 분포 확인

---

## TODO — Medium Priority

### 전략 개선
- [ ] Markout 기반 동적 skew 강도 조절
- [ ] Toxicity score (500ms raw + 5s adjusted 기반)
- [ ] Order flow imbalance signal
- [ ] Funding rate signal

### 인프라
- [ ] 도쿄 리전 colocate (RTT → 1-3ms)
- [ ] Prometheus 메트릭 export + Grafana 대시보드
- [ ] 커널 튜닝 (busy_poll, tcp_nodelay, CPU pinning)

### 마켓 확장
- [ ] Multi-market 지원 (ETH-USD, altcoins)
- [ ] 마켓별 독립 파라미터

---

## TODO — Low Priority

### 고급 최적화
- [ ] SIMD JSON 파싱
- [ ] Kernel bypass (io_uring)
- [ ] Adjusted markout (외부 레퍼런스 대비)

### 운영
- [ ] 자동 재시작 (systemd/supervisor)
- [ ] 슬랙/텔레그램 알림
- [ ] 일별 PnL 리포트
- [ ] Config hot-reload

---

## Architecture

```
                    ┌──────────────┐
                    │ Binance WS   │ (TODO)
                    │ bookTicker   │
                    └──────┬───────┘
                           │ reference mid
┌──────────────┐   ┌───────▼───────┐   ┌──────────────┐
│ Extended WS  │──▶│   MarketBot   │──▶│ Extended REST │
│ orderbook    │   │               │   │ create_order  │
│ trades       │   │ fair_price    │   │ cancel_order  │
│ mark_price   │   │ spread_calc   │   └──────────────┘
│ account      │   │ skew_calc     │
└──────────────┘   │ quote_gen     │
                   │ markout       │
                   │ risk_mgmt     │
                   └───────────────┘
```

## Key Commits
```
4ce23b6 Fix 1101: add 14-day expiry buffer + use official sign_message
458a8ac Use x10 official rust-crypto-lib-base for order hash
4d2beff WS auto-reconnect on session timeout + initial requote trigger
4263c68 Fix P0 skew direction + bootstrap position sign
```
