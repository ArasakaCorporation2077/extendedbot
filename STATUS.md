# Extended Market Maker — Project Status (2026-03-28)

## 현재 상태
- **마켓**: TAO-USD (이전: BTC-USD, WIF-USD, CRCL_24_5-USD)
- **EC2**: 54.199.221.223 (ap-northeast-1, t2.small)
- **잔고**: ~$57
- **봇 상태**: 실행 중, TAO fill 수집 중

---

## 거래소 정보

### x10 (Extended Exchange)
- **구조**: 하이브리드 off-chain CLOB + StarkNet on-chain settlement
- **API docs**: https://api.docs.extended.exchange
- **서버**: AWS Tokyo (ap-northeast-1)
- **Rate limit**: 1000 req/min (default)
- **Maker fee**: 0% | Taker fee: 0.025%
- **Maker rebate**: ≥0.5% market share → 0.002% (현재 미달)
- **WS 주문 미지원**: REST only
- **WS 호스트**: `wss://api.starknet.extended.exchange` (NOT app.extended.exchange — UI용은 WAF 차단됨)
- **REST 호스트**: `https://api.starknet.extended.exchange`
- **WS session timeout**: ~15초마다 끊김 → 재연결 필요

### Binance (Reference Price)
- bookTicker: `wss://fstream.binance.com/ws/{symbol}@bookTicker`
- aggTrade: `wss://fstream.binance.com/ws/{symbol}@aggTrade`
- depth20: `wss://fstream.binance.com/ws/{symbol}@depth20@100ms`
- 심볼 매핑: `TAO-USD` → `taousdt`, `CRCL_24_5-USD` → `crclusdt` (날짜 접미사 제거)

### 조사한 다른 거래소
- **gTrade (Gains)**: 오더북 없음(AMM), 수수료 5bps, 온체인 주문 → MM 불가
- **Reya**: 수수료 4bps (0%가 아님), 100ms 블록, REST/WS API 있음 → x10보다 비쌈

---

## 수익 모델

### 핵심: 스프레드 캡처
```
수익/fill = 캡처 스프레드(bps) - markout 손실(bps)
```

### Fair Value 계산
```
fair_price   = binance_mid          (즉시 반응)
basis_offset = EWMA(x10_mid - binance_mid, alpha=0.01)
quote_price  = fair_price + basis_offset + flow_shift + depth_shift
```

### 시그널 (regression R²=0.03 — 효과 미미)
- **trade flow imbalance**: 바이낸스 aggTrade buy/sell 비율 (sensitivity 0.5bps)
- **depth imbalance**: 바이낸스 depth20 top 3 level bid/ask 비율 (sensitivity 0.3bps)
- **VPIN**: 바이낸스 aggTrade 기반 volume-synced informed trading 확률

### Markout 측정
```
raw = (future_mid - fill_price) × direction
adj = raw - binance_market_movement
```
- 5개 horizon: 50ms, 200ms, 500ms, 1s, 5s
- tox_score = max(0, -raw_500ms) + max(0, -adj_5s)
- feedback_bps → 스프레드 확대에 사용

---

## 마켓별 성과 기록

### BTC-USD (1,777 fills)
- **총 PnL**: -$9.50 (fill당 -$0.005)
- **5s adj markout**: -1.41bps → -0.34bps (최적화 후)
- **win rate**: 10%
- **결론**: 역선택 심함. informed trader가 같은 RTT(11ms)에서 더 빠른 시그널. fill당 손익분기 근처까지 개선했지만 수익 전환 못 함.

### WIF-USD (11 fills)
- **5s adj markout**: -7.91bps
- **결론**: 유동성 없음. 스프레드 넓은 이유가 있었음. basis ±22bps로 극단적. 즉시 폐기.

### CRCL_24_5-USD (3 fills)
- **5s adj markout**: -1.44bps
- **결론**: V/OI 14.7x로 파머 활동 의심했지만, WS가 CloudFront 차단당함 (잘못된 WS 호스트 사용). 주식 perp라 미국 장 외 유동성 없음.

### TAO-USD (진행 중)
- spread 7.6bps, 볼륨 $4.5M, V/OI 4.4x
- 바이낸스 TAOUSDT vol $675M (reference 안정적)
- **num_levels=2 → min size reject 문제 발견 → num_levels=1로 수정**

---

## 레이턴시 (도쿄 EC2)

| 메트릭 | 값 |
|--------|-----|
| order_rtt | 7-17ms |
| cancel_rtt | 5ms |
| cancel-to-place sleep | 5ms |
| total requote cycle | ~27ms |
| TCP RTT to x10 | 11ms (ELB 포함) |
| binance_age | 0.2-2ms |
| compute | 0.02ms |

---

## 핵심 버그 기록

### BUG-001: Exposure tracker 방향성 미고려 (2026-03-18)
포지션 줄이는 주문까지 차단 → 포지션 쏠림. 수정: 같은 방향만 차단.

### BUG-002: tox_score 부호 반전 (2026-03-18)
역선택 시 스프레드 축소 → 손실 가속. 수정: 부호 제거.

### BUG-003: WS 호스트 잘못 사용 (2026-03-27)
`app.extended.exchange`(UI용) → CloudFront WAF 차단. 수정: `api.starknet.extended.exchange`.

### BUG-004: Toxic hour spread 곱하기 (2026-03-25)
VPIN 3x × toxic hour 2x = 6x → fill 불가. 수정: max(VPIN, time)로 변경 후 additive로.

### BUG-005: Basis filter가 inventory skew 오버라이드 (2026-03-25)
Long인데 basis filter가 AskOnly → BidOnly로 전환 → unwind 차단. 수정: active_side==Both일 때만 적용.

### BUG-006: num_levels=2에서 2nd level min size 미달 (2026-03-28)
TAO $314 × 0.13 × 0.7(decay) = 0.09 < min 0.1 → reject → consecutive_rejects → 전체 주문 중단. 수정: num_levels=1.

### BUG-007: consecutive_rejects 10초 영구 백오프 (2026-03-25)
3+ rejects → 10초 대기 → 또 reject → 무한 루프. 수정: 2초 + 5회 리셋.

### BUG-008: 봇 silent death — WS 재연결 실패 (2026-03-26)
WS 끊김 → 재연결 실패 → 이벤트 없음 → 봇 6시간 방치. 수정: watchdog 3분 idle → exit(1) + run.sh 자동 재시작.

### BUG-009: VPIN sustained toxic 과민 (2026-03-28)
TAO에서 VPIN 상시 0.7+ → spread 3x 영구 → fill 불가. 수정: threshold 0.7→0.85, bars 8→20.

---

## 시도한 것 & 결과

### 효과 있었던 것
- EC2 도쿄 이전: RTT 40ms→11ms
- cancel 대기 50ms→5ms: cycle 72ms→27ms
- WS 호스트 수정: CloudFront 차단 해결
- best_price_margin 1.0bps: 캡처 스프레드 확보
- toxic hour additive spread: 포지션 unwind 가능하게
- unwind margin 0: 포지션 보유 시간 단축
- basis filter: worst fill 제거

### 효과 없었던 것
- trade flow imbalance (R²=0.03)
- depth imbalance (계수 0.03)
- sell 비대칭 스프레드 (sell 안 팔림)
- VPIN sustained toxic (TAO에서 과민반응)
- 다른 마켓 이동 (WIF, CRCL — 각각 문제 있었음)

---

## 참고 자료 & 학습

### 읽은 글
- Quant Roadmap: fair value, spread, skew 기본
- beatzxbt/smm: Bollinger Band spread, inventory extreme, 2단계 주문 크기
- gamma-ray: Avellaneda-Stoikov 모델 (reservation price + optimal spread)
- VisualHFT: VPIN 계산, LOB imbalance, Market Resilience
- VPIN 글: sustained elevated (8+ bars) 시그널, LOB multi-level
- HFT Advisory: $0+ strategy, LOB architecture, order placement
- Liquidity Goblin 팟캐스트: "올바른 테이블에 앉아라", "좋은 다리만 하라", funding rate 캐리
- MM 리서치 논문: PULSE 알고리즘, 다중 거래소 리드-래그, VAMP

### 핵심 교훈
1. **"올바른 테이블"이 실력보다 중요** — BTC-USD는 프로 테이블
2. **속도 경쟁 이길 수 없으면 피해라** — 11ms RTT는 변경 불가
3. **파라미터 튜닝은 한계 있음** — edge 없으면 ±$0 맴돎
4. **flow/depth 시그널은 이 환경에서 효과 없음** — R²=0.03
5. **min order size 등 마켓별 제약 반드시 확인** — TAO num_levels=2 문제
6. **WS 호스트 확인** — API vs UI 엔드포인트 구분 필수

---

## 현재 Config (TAO-USD)
```toml
market = "TAO-USD"
order_size_usd = 40.0
min_order_usd = 35.0
max_order_usd = 50.0
leverage = 5
base_spread_bps = 4.0
min_spread_bps = 2.0
best_price_tighten_enabled = true
best_price_margin_bps = 1.0
num_levels = 1
trade_flow_sensitivity_bps = 0.5
depth_imbalance_sensitivity_bps = 0.3
max_position_usd = 50.0
vpin_bucket_volume = 5.0
vpin_num_buckets = 50
# VPIN threshold: 0.85, sustained_bars: 20
```

## Infra
- **EC2**: 54.199.221.223 (ap-northeast-1, t2.small)
- **SSH**: `ssh -i extendedMM.pem ec2-user@54.199.221.223`
- **Bot path**: `~/extendedMM/target/release/extended-mm`
- **Auto-restart**: `run.sh` (nohup wrapper)
- **Watchdog**: 3분 idle → exit(1) → run.sh 재시작
- **GitHub**: https://github.com/ArasakaCorporation2077/extendedbot.git (private)

## TODO
- [ ] TAO fill 200개 모아서 markout 분석
- [ ] regression 재실행 (flow/depth 포함 데이터)
- [ ] 포지션 보유 시간 분석 (1s markout 양수인데 5s 음수 — 빨리 팔면 수익)
- [ ] Reya 거래소 프로토타입 (수수료 높지만 공평한 속도)
- [ ] 포인트 파머 패턴 감지 로직
- [ ] funding rate 캐리 전략 결합
