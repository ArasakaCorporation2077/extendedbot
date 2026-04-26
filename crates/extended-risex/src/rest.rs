//! RISEx REST client — minimal surface needed for live trading.
//!
//! Supports:
//!   - read-only:  getSystemConfig, getEip712Domain, getNonceState, getOrderbook, getOpenOrders
//!   - signed:     placeOrder, cancelOrder, cancelAllOrders
//!
//! Auth flow per request:
//!   1. compute action hash (encoder.rs)
//!   2. fetch (or reuse) NonceState
//!   3. build PermitParams via EIP-712 VerifyWitness signing (signing.rs)
//!   4. POST body with permit attached
//!
//! The REST server itself is on Cloudflare; the matching engine sits behind
//! `ws.rise.trade` (AWS Tokyo). Latency target: REST POST ~200-300ms (onchain
//! finalization), WS book inclusion ~70ms.

use std::time::Duration;

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{de::DeserializeOwned, Serialize};

use crate::encoder::{
    encode_cancel_all, encode_cancel_order, encode_order, CancelParams, OrderParams,
};
use crate::rest_types::*;
use crate::signing::{sign_witness, DomainConfig, WitnessParams};

pub const DEFAULT_BASE_URL: &str = "https://api.rise.trade";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_DEADLINE_SECS: u32 = 300;
const MAX_BITMAP_INDEX: u16 = 207;

/// Read-only RISEx REST client. Wraps `reqwest::Client` with a base URL.
#[derive(Clone)]
pub struct InfoClient {
    http: Client,
    base_url: String,
}

impl InfoClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .expect("reqwest client");
        Self { http, base_url: base_url.into() }
    }

    pub fn mainnet() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let res = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = res.status();
        let body = res.text().await.context("read body")?;
        if !status.is_success() {
            return Err(anyhow!("GET {url} -> {status}: {body}"));
        }
        unwrap_envelope::<T>(&body)
            .with_context(|| format!("decode {url}: {body}"))
    }

    pub async fn get_system_config(&self) -> Result<SystemConfig> {
        self.get("/v1/system/config").await
    }

    pub async fn get_eip712_domain(&self) -> Result<Eip712DomainResponse> {
        self.get("/v1/auth/eip712-domain").await
    }

    pub async fn get_nonce_state(&self, account: &Address) -> Result<NonceState> {
        self.get(&format!("/v1/nonce-state/{account:#x}")).await
    }

    pub async fn get_orderbook(&self, market_id: u16, limit: u16) -> Result<Orderbook> {
        self.get(&format!("/v1/orderbook?market_id={market_id}&limit={limit}")).await
    }

    pub async fn get_open_orders(&self, account: &Address, market_id: Option<u16>) -> Result<Vec<OpenOrder>> {
        let mut path = format!("/v1/orders/open?account={account:#x}");
        if let Some(m) = market_id { path.push_str(&format!("&market_id={m}")); }
        // After unwrap_envelope, `data` is the inner object {orders: [...]}
        let inner: serde_json::Value = self.get(&path).await?;
        let orders = inner.get("orders").cloned().unwrap_or(serde_json::json!([]));
        Ok(serde_json::from_value(orders)?)
    }
}

/// Authenticated client for placing/canceling orders.
/// Constructed via `ExchangeClient::init` which fetches the EIP-712 domain
/// and target contract from the API.
pub struct ExchangeClient {
    pub info: InfoClient,
    pub account: Address,
    pub signer_key: PrivateKeySigner,
    pub domain: DomainConfig,
    pub target: Address,
    pub is_erc1271: bool,
}

impl ExchangeClient {
    /// Initialise: fetches EIP-712 domain + system config to learn the target.
    pub async fn init(
        info: InfoClient,
        account: Address,
        signer_key: PrivateKeySigner,
        is_erc1271: bool,
    ) -> Result<Self> {
        let domain_resp = info.get_eip712_domain().await.context("fetch eip712 domain")?;
        let cfg = info.get_system_config().await.context("fetch system config")?;
        let target = cfg.target()
            .ok_or_else(|| anyhow!("system config missing router/orders_manager address"))?;

        let chain_id = domain_resp.chain_id_u64();
        let domain = DomainConfig {
            name: domain_resp.name,
            version: domain_resp.version,
            chain_id,
            verifying_contract: domain_resp.verifying_contract,
        };

        Ok(Self { info, account, signer_key, domain, target, is_erc1271 })
    }

    fn signer_address(&self) -> Address {
        self.signer_key.address()
    }

    /// Build a PermitParams for a given action hash. Fetches nonce state if
    /// not provided; advances anchor when bitmap is exhausted.
    async fn build_permit(
        &self,
        action_hash: alloy_primitives::B256,
        nonce: Option<NonceState>,
    ) -> Result<PermitParams> {
        let nonce = match nonce {
            Some(n) => n,
            None => self.info.get_nonce_state(&self.account).await
                .context("fetch nonce state")?,
        };

        let mut anchor = nonce.nonce_anchor_u64();
        let mut bitmap_index = nonce.current_bitmap_index;
        if bitmap_index > MAX_BITMAP_INDEX {
            anchor += 1;
            bitmap_index = 0;
        }

        let deadline_unix = (chrono::Utc::now().timestamp() as u32) + DEFAULT_DEADLINE_SECS;

        let w = WitnessParams {
            account: self.account,
            target: self.target,
            hash: action_hash,
            nonce_anchor: anchor,
            nonce_bitmap: bitmap_index as u8,
            deadline: deadline_unix,
        };

        let sig_hex = sign_witness(&self.signer_key, &self.domain, &w)?;
        let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x"))
            .context("decode signature hex")?;
        use base64::Engine;
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(&sig_bytes);

        Ok(PermitParams {
            account: self.account,
            signer: self.signer_address(),
            nonce_anchor: anchor,
            nonce_bitmap_index: bitmap_index,
            deadline: deadline_unix,
            signature: sig_b64,
            is_erc1271: if self.is_erc1271 { Some(true) } else { None },
        })
    }

    pub async fn place_order(&self, p: &OrderParams) -> Result<OrderResponse> {
        let mut p = *p;
        p.is_erc1271 = self.is_erc1271;
        let hash = encode_order(&p);
        let permit = self.build_permit(hash, None).await?;

        let req = PlaceOrderRequest {
            market_id: p.market_id,
            side: p.side,
            order_type: p.order_type,
            price_ticks: p.price_ticks,
            size_steps: p.size_steps,
            time_in_force: p.time_in_force,
            post_only: p.post_only,
            reduce_only: p.reduce_only,
            stp_mode: p.stp_mode,
            ttl_units: p.ttl_units,
            client_order_id: p.client_order_id.to_string(),
            builder_id: p.builder_id,
            permit,
        };
        self.post("/v1/orders/place", &req).await
    }

    pub async fn cancel_order(&self, market_id: u16, order_id: &str, resting_order_id: u64) -> Result<CancelResponse> {
        let hash = encode_cancel_order(&CancelParams { market_id, resting_order_id });
        let permit = self.build_permit(hash, None).await?;
        let req = CancelOrderRequest {
            market_id,
            order_id: order_id.to_string(),
            permit,
        };
        self.post("/v1/orders/cancel", &req).await
    }

    pub async fn cancel_all_orders(&self, market_id: u16) -> Result<CancelResponse> {
        let hash = encode_cancel_all(market_id);
        let permit = self.build_permit(hash, None).await?;
        let req = CancelAllOrdersRequest { market_id, permit };
        self.post("/v1/orders/cancel-all", &req).await
    }

    async fn post<Req: Serialize, Res: DeserializeOwned>(&self, path: &str, body: &Req) -> Result<Res> {
        let url = format!("{}{}", self.info.base_url, path);
        let res = self.info.http.post(&url).json(body).send().await
            .with_context(|| format!("POST {url}"))?;
        let status = res.status();
        let text = res.text().await.context("read body")?;
        if !status.is_success() {
            return Err(anyhow!("POST {url} -> {status}: {text}"));
        }
        unwrap_envelope::<Res>(&text)
            .with_context(|| format!("decode {url}: {text}"))
    }
}

/// RISEx wraps every response in `{"data": <T>, "request_id": "..."}`.
/// Some endpoints return just the inner shape (older paths or during errors),
/// so we accept both forms.
fn unwrap_envelope<T: DeserializeOwned>(body: &str) -> Result<T> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    if let Some(data) = v.get("data") {
        Ok(serde_json::from_value(data.clone())?)
    } else {
        Ok(serde_json::from_value(v)?)
    }
}
