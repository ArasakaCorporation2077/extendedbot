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
- [x] ?keepAlive=true 파라미터
- [x] Snapshot vs Delta 분리 (SNAPSHOT → apply_snapshot, DELTA → apply_delta)
- [x] Orderbook level: c 필드 (absolute size) 우선, q fallback
- [x] Sequence number gap detection
- [x] Auto-reconnect with exponential backoff (healthy 연결 후 리셋)
- [x] Ping/Pong 응답

### Orderbook
- [x] BTreeMap 기반 local orderbook (bid desc, ask asc)
- [x] Snapshot/Delta 적용
- [x] Mid price, best bid/ask, spread 계산
- [x] Sequence gap detection + resync 요청

### Crypto / Signing
- [x] StarkNet ECDSA 서명 (Poseidon hash + SNIP12)
- [x] CSPRNG random k-value (rejection sampling, k < EC_ORDER)
- [x] Stark key derivation from ETH key (grind_key)
- [x] Stark private key 직접 입력 지원
- [x] AtomicU64 vault_id (REST 로딩 후 업데이트)
- [x] Domain separation (Sepolia vs Mainnet)

### Strategy
- [x] Fair price calculator (local mid, EWMA basis for external ref)
- [x] Spread calculator (base + volatility + VPIN + markout feedback)
- [x] Skew calculator (reservation price shift, nonlinear, emergency flatten)
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
- [x] check_fills (inherent method, not infinite recursion)

### Order Tracking
- [x] Dual-map lookup (external_id ↔ exchange_id)
- [x] Ghost order detection + cleanup
- [x] State transition validation (reject invalid)
- [x] Pending exposure calculation
- [x] exchange_id → external_id reverse resolution

### Code Quality
- [x] Custom review agents (.claude/agents/)
- [x] 82 unit tests passing
- [x] 4 rounds of code review + bug fixes

---

## In Progress

### WS Private Stream Authentication
- [ ] Private account WS 연결 (현재 401)
- [ ] X-Api-Key 헤더 vs JWT token 방식 확인
- [ ] 주문/체결/포지션/잔고 실시간 수신

---

## TODO — High Priority

### Binance Reference Price (fair value 핵심)
- [ ] Binance BTC/USDT bookTicker WS 연결
- [ ] fair_price_calc.update_reference_mid() 연결
- [ ] EWMA basis tracking (Binance - Extended 스프레드)
- [ ] Binance 급변 시 fast cancel 트리거
- [ ] 레이턴시 측정 (Binance → 전략 반영까지)

### 첫 주문 제출 테스트
- [ ] 메인넷에서 최소 사이즈 주문 1건 제출
- [ ] 서명 검증 (거래소 accept/reject 확인)
- [ ] 주문 라이프사이클 확인 (place → open → cancel)
- [ ] 잔고 $100+ 입금 필요 (BTC-USD 최소 주문 $100)

### Markout 데이터 수집
- [ ] 실제 체결 후 markout curve 기록
- [ ] fills.jsonl 로깅 (오프라인 분석용)
- [ ] horizon별 markout 분포 확인
- [ ] adverse selection 패턴 분석

---

## TODO — Medium Priority

### 전략 개선
- [ ] 비대칭 skew (한쪽만 넓히기, 현재는 reservation price shift)
- [ ] Markout 기반 동적 skew 강도 조절
- [ ] Toxicity score (500ms raw + 5s adjusted 기반)
- [ ] Order flow imbalance signal
- [ ] Funding rate signal (높은 funding → skew 조정)

### 인프라
- [ ] 도쿄 리전 colocate (RTT 20-30ms → 1-3ms)
- [ ] 커널 튜닝 (busy_poll, tcp_nodelay, CPU pinning)
- [ ] 채널 교체 (tokio mpsc → crossbeam/rtrb)
- [ ] Prometheus 메트릭 export
- [ ] Grafana 대시보드

### 마켓 확장
- [ ] Multi-market 지원 (ETH-USD, altcoins)
- [ ] 마켓별 독립 파라미터 (spread, skew, size)
- [ ] 유동성 낮은 altcoin에서 wider spread MM

---

## TODO — Low Priority

### 고급 최적화
- [ ] SIMD JSON 파싱 (simdjson)
- [ ] Kernel bypass (DPDK/io_uring)
- [ ] WS 프레임 직접 파싱 (tungstenite 우회)
- [ ] Adjusted markout (외부 레퍼런스 대비)
- [ ] Regression 기반 fair value 조정 (multiple features)

### 운영
- [ ] 자동 재시작 (systemd/supervisor)
- [ ] 슬랙/텔레그램 알림 (체결, 에러, circuit breaker trip)
- [ ] 일별 PnL 리포트 자동 생성
- [ ] Config hot-reload (재시작 없이 파라미터 변경)

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
│ account(TODO)│   │ skew_calc     │
└──────────────┘   │ quote_gen     │
                   │ markout       │
                   │ risk_mgmt     │
                   └───────────────┘
```

## Commits
```
406d7af Initial commit
3b0580a Fix P0/P1/P2 bugs + review agents
96e3a65 WS v1 streams + REST API alignment + markout tracker
e734d2e Fix WS data flow: snapshot/delta split
4263c68 Fix P0 skew direction + bootstrap position sign
```
