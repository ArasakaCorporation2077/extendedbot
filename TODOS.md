# TODOS

## TODO-1: REST Observer — 저유동성 알트코인 시장 구조 관찰
**Priority:** P0 (다음 작업)
**Status:** Not started
**What:** REST polling 기반 market observer. get_market_stats() + get_orderbook()를 분당 1회 폴링하여 spread, volume, trade frequency, basis를 48-96시간 수집.
**Why:** $104→$53 손실 후 BTC/ETH에서 edge를 찾지 못함. 저유동성 알트코인($200K-$500K/day)에서 구조적 edge 가설을 $0 리스크로 검증 필요.
**Implementation:**
- Pre-step: get_markets() → get_market_stats(market) loop → $200K-$500K volume 필터 → Binance 페어 존재 확인 → min_order_size 확인 → top 5 선택
- Polling: 분당 1회 get_orderbook(market) for each candidate → JSONL 기록
- JSONL schema: timestamp, pair, x10_mid, spread_bps, binance_mid, basis_bps, reference_source, book_depth_usd, seconds_since_last_trade, min_order_size
- Decision gates: GO (spread>5bps + trades<5min apart + basis stable + 500+ trades), NO-GO (spread<3bps OR trades>15min), INCONCLUSIVE (5-15min → 96h)
- Critical: 시작 시 후보 마켓 0개이면 early exit with message
**Depends on:** Nothing
**Blocked by:** Nothing
**Design doc:** ~/.gstack/projects/ArasakaCorporation2077-extendedbot/user-master-design-20260402-000429.md

## TODO-2: Sparse-fill 아키텍처 수정
**Priority:** P1 (observer GO 이후)
**Status:** Not started
**What:** 저유동성 마켓용 quoting 전략 수정. Quote aging guard, passive-only mode, time-based spread widening.
**Why:** 현재 aggressive/reducing 패턴은 BTC/ETH의 빠른 fill rate를 전제. 10-30분 fill 간격에서는 quote가 adverse selection에 노출됨. WIF-USD -7.91bps가 이 패턴의 증거.
**Depends on:** TODO-1 (observer가 GO 신호를 내야 의미 있음)
**Blocked by:** TODO-1 결과
