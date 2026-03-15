//! Extended Exchange REST API client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use tracing::{debug, error, warn};

use extended_crypto::{StarkSigner, OrderSignParams};
use extended_types::config::ExchangeConfig;
use extended_types::order::OrderRequest;

use crate::adapter::{ExchangeAdapter, OrderAck, CancelAck, MassCancelAck};
use crate::rate_limiter::RateLimiter;
use crate::rest_types::*;

/// Extended Exchange REST API client.
pub struct ExtendedRestClient {
    client: Client,
    base_url: String,
    api_key: String,
    user_agent: String,
    signer: Arc<dyn StarkSigner>,
    rate_limiter: Arc<RateLimiter>,
    nonce_counter: Arc<AtomicU32>,
    market_config: parking_lot::RwLock<Option<MarketConfigCache>>,
}

#[derive(Debug, Clone)]
struct MarketConfigCache {
    collateral_resolution: u64,
    synthetic_resolution: u64,
}

impl ExtendedRestClient {
    pub fn new(config: &ExchangeConfig, signer: Arc<dyn StarkSigner>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .pool_max_idle_per_host(10)
            .tcp_nodelay(true)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            base_url: config.rest_base_url().trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
            user_agent: config.user_agent.clone(),
            signer,
            rate_limiter: Arc::new(RateLimiter::default_extended()),
            nonce_counter: Arc::new(AtomicU32::new(1)),
            market_config: parking_lot::RwLock::new(None),
        }
    }

    pub fn shared_rate_limiter(&self) -> Arc<RateLimiter> {
        self.rate_limiter.clone()
    }

    fn next_nonce(&self) -> u32 {
        self.nonce_counter.fetch_add(1, Ordering::SeqCst)
    }

    async fn rate_limit_wait(&self) {
        if let Some(wait) = self.rate_limiter.try_acquire() {
            warn!(wait_ms = wait.as_millis(), "Rate limited, waiting");
            tokio::time::sleep(wait).await;
        }
    }

    fn auth_headers(&self) -> Vec<(&str, String)> {
        vec![
            ("X-Api-Key", self.api_key.clone()),
            ("User-Agent", self.user_agent.clone()),
        ]
    }

    // === Public endpoints (no auth) ===

    pub async fn get_markets(&self) -> Result<Vec<MarketResponse>> {
        self.rate_limit_wait().await;
        let url = format!("{}/api/v1/info/markets", self.base_url);
        let resp = self.client.get(&url)
            .header("User-Agent", &self.user_agent)
            .send().await
            .context("GET /info/markets failed")?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on GET /info/markets");
        }

        resp.json().await.context("Failed to parse markets response")
    }

    pub async fn get_orderbook(&self, market: &str) -> Result<OrderbookResponse> {
        self.rate_limit_wait().await;
        let url = format!("{}/api/v1/info/markets/{}/orderbook", self.base_url, market);
        let resp = self.client.get(&url)
            .header("User-Agent", &self.user_agent)
            .send().await
            .context("GET orderbook failed")?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on GET orderbook");
        }

        resp.json().await.context("Failed to parse orderbook")
    }

    pub async fn get_market_stats(&self, market: &str) -> Result<MarketStatsResponse> {
        self.rate_limit_wait().await;
        let url = format!("{}/api/v1/info/markets/{}/stats", self.base_url, market);
        let resp = self.client.get(&url)
            .header("User-Agent", &self.user_agent)
            .send().await?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited");
        }

        resp.json().await.context("Failed to parse market stats")
    }

    // === Private read-only endpoints (API key) ===

    async fn get_private<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.rate_limit_wait().await;
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (k, v) in self.auth_headers() {
            req = req.header(k, v);
        }
        let resp = req.send().await.context(format!("GET {} failed", path))?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on GET {}", path);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", path, status, body);
        }

        let text = resp.text().await?;
        serde_json::from_str(&text).context(format!("Failed to parse response from {}", path))
    }

    pub async fn get_balance_info(&self) -> Result<BalanceResponse> {
        self.get_private("/api/v1/user/balance").await
    }

    pub async fn get_positions_info(&self) -> Result<Vec<PositionResponse>> {
        self.get_private("/api/v1/user/positions").await
    }

    pub async fn get_open_orders_info(&self, market: Option<&str>) -> Result<Vec<OrderResponse>> {
        let path = match market {
            Some(m) => format!("/api/v1/user/orders?market={}", m),
            None => "/api/v1/user/orders".to_string(),
        };
        self.get_private(&path).await
    }

    pub async fn get_fees(&self, market: &str) -> Result<FeeResponse> {
        self.get_private(&format!("/api/v1/user/fees?market={}", market)).await
    }

    pub async fn get_leverage(&self, market: &str) -> Result<LeverageResponse> {
        self.get_private(&format!("/api/v1/user/leverage?market={}", market)).await
    }

    // === Private write endpoints (API key + Stark signature) ===

    pub fn cache_market_config(&self, collateral_resolution: u64, synthetic_resolution: u64) {
        *self.market_config.write() = Some(MarketConfigCache {
            collateral_resolution,
            synthetic_resolution,
        });
    }

    fn get_market_config(&self) -> Result<MarketConfigCache> {
        self.market_config.read().clone()
            .context("Market config not cached. Call cache_market_config first.")
    }

    fn build_order_sign_params(&self, req: &OrderRequest) -> Result<OrderSignParams> {
        let mc = self.get_market_config()?;
        let nonce = self.next_nonce();
        let salt = chrono::Utc::now().timestamp_millis() as u64;

        Ok(OrderSignParams {
            position_id: self.signer.vault_id(),
            side: req.side,
            base_asset: req.market.clone(),
            quote_asset: "USD".to_string(),
            base_qty: req.qty,
            quote_qty: req.price * req.qty,
            fee: req.max_fee,
            expiration_epoch_millis: req.expiry_epoch_millis,
            nonce,
            salt,
            collateral_resolution: mc.collateral_resolution,
            synthetic_resolution: mc.synthetic_resolution,
        })
    }

    async fn submit_order(&self, req: &OrderRequest) -> Result<CreateOrderResponse> {
        self.rate_limit_wait().await;

        let sign_params = self.build_order_sign_params(req)?;
        let signature = self.signer.sign_order(&sign_params)?;

        let body = CreateOrderRequest {
            id: req.external_id.clone(),
            market: req.market.clone(),
            r#type: "limit".to_string(),
            side: req.side.to_string(),
            qty: req.qty.to_string(),
            price: req.price.to_string(),
            fee: req.max_fee.to_string(),
            expiry_epoch_millis: req.expiry_epoch_millis,
            time_in_force: req.time_in_force.wire_value().to_string(),
            settlement: Settlement {
                r: signature.r,
                s: signature.s,
                stark_key: self.signer.public_key_hex().to_string(),
                collateral_position: self.signer.vault_id(),
            },
            post_only: if req.post_only { Some(true) } else { None },
            reduce_only: if req.reduce_only { Some(true) } else { None },
            cancel_id: req.cancel_id.clone(),
            nonce: Some(sign_params.nonce),
        };

        let url = format!("{}/api/v1/user/order", self.base_url);
        let mut http_req = self.client.post(&url);
        for (k, v) in self.auth_headers() {
            http_req = http_req.header(k, v);
        }

        let resp = http_req.json(&body).send().await
            .context("POST /user/order failed")?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on POST /user/order");
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %body_text, "Order creation failed");
            anyhow::bail!("Order creation failed: {} - {}", status, body_text);
        }

        resp.json().await.context("Failed to parse order response")
    }

    async fn cancel_by_id(&self, id: &str, by_external: bool) -> Result<CancelAck> {
        self.rate_limit_wait().await;

        // Extended Starknet endpoints:
        // - By exchange ID: DELETE /api/v1/user/order?orderId={orderId}
        // - By external ID: DELETE /api/v1/user/order?externalId={externalId}
        let url = if by_external {
            format!("{}/api/v1/user/order?externalId={}", self.base_url, id)
        } else {
            format!("{}/api/v1/user/order?orderId={}", self.base_url, id)
        };

        let mut req = self.client.delete(&url);
        for (k, v) in self.auth_headers() {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("DELETE order failed")?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on DELETE order");
        }

        let success = resp.status().is_success();
        let message = if !success {
            Some(resp.text().await.unwrap_or_default())
        } else {
            None
        };

        Ok(CancelAck {
            external_id: if by_external { Some(id.to_string()) } else { None },
            exchange_id: if !by_external { Some(id.to_string()) } else { None },
            success,
            message,
        })
    }

    pub async fn mass_cancel_orders(&self, market: &str) -> Result<MassCancelAck> {
        self.rate_limit_wait().await;
        let url = format!("{}/api/v1/user/order/massCancel", self.base_url);
        let body = serde_json::json!({ "market": market });

        let mut req = self.client.post(&url);
        for (k, v) in self.auth_headers() {
            req = req.header(k, v);
        }

        let resp = req.json(&body).send().await
            .context("POST /user/order/massCancel failed")?;

        if resp.status() == 429 {
            self.rate_limiter.on_rate_limited();
            anyhow::bail!("Rate limited on mass-cancel");
        }

        Ok(MassCancelAck {
            cancelled_count: 0, // Parsed from response in production
            success: resp.status().is_success(),
        })
    }

    pub async fn activate_dead_man_switch(&self, timeout_ms: u64) -> Result<()> {
        self.rate_limit_wait().await;
        let url = format!(
            "{}/api/v1/user/deadmanswitch?countdownTime={}",
            self.base_url, timeout_ms
        );

        let mut req = self.client.post(&url);
        for (k, v) in self.auth_headers() {
            req = req.header(k, v);
        }

        let resp = req.send().await
            .context("POST deadmanswitch failed")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Dead man's switch activation failed: {}", body);
        }

        debug!("Dead man's switch activated: {}ms", timeout_ms);
        Ok(())
    }

    pub async fn get_account_info(&self) -> Result<AccountInfoResponse> {
        self.get_private("/api/v1/user/account/info").await
    }
}

#[async_trait]
impl ExchangeAdapter for ExtendedRestClient {
    async fn create_order(&self, req: &OrderRequest) -> Result<OrderAck> {
        let resp = self.submit_order(req).await?;
        Ok(OrderAck {
            external_id: req.external_id.clone(),
            exchange_id: resp.id,
            accepted: resp.status.as_deref() != Some("rejected"),
            message: resp.message,
        })
    }

    async fn cancel_order(&self, exchange_id: &str) -> Result<CancelAck> {
        self.cancel_by_id(exchange_id, false).await
    }

    async fn cancel_order_by_external_id(&self, external_id: &str) -> Result<CancelAck> {
        self.cancel_by_id(external_id, true).await
    }

    async fn mass_cancel(&self, market: &str) -> Result<MassCancelAck> {
        self.mass_cancel_orders(market).await
    }

    async fn mass_auto_cancel(&self, timeout_ms: u64) -> Result<()> {
        self.activate_dead_man_switch(timeout_ms).await
    }

    async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        self.get_positions_info().await
    }

    async fn get_open_orders(&self, market: Option<&str>) -> Result<Vec<OrderResponse>> {
        self.get_open_orders_info(market).await
    }

    async fn get_balance(&self) -> Result<BalanceResponse> {
        self.get_balance_info().await
    }
}
