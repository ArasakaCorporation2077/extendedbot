use rust_decimal::Decimal;
use serde::Deserialize;

/// Top-level application config, maps to config/*.toml + env overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub exchange: ExchangeConfig,
    pub trading: TradingConfig,
    pub risk: RiskConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeConfig {
    /// API key for Extended Exchange.
    #[serde(default)]
    pub api_key: String,
    /// Ethereum private key or seed for Stark key derivation.
    #[serde(default)]
    pub api_secret: String,
    #[serde(default)]
    pub paper_trading: bool,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}

fn default_user_agent() -> String {
    "extended-mm/0.1.0".into()
}

impl ExchangeConfig {
    pub fn rest_base_url(&self) -> &str {
        "https://api.starknet.extended.exchange"
    }

    pub fn ws_url(&self) -> &str {
        "wss://api.starknet.extended.exchange"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TradingConfig {
    /// Market to trade, e.g. "BTC-USD".
    pub market: String,

    // Sizing
    #[serde(default = "default_order_size")]
    pub order_size_usd: Decimal,
    #[serde(default = "default_min_order")]
    pub min_order_usd: Decimal,
    #[serde(default = "default_max_order")]
    pub max_order_usd: Decimal,
    #[serde(default = "default_leverage")]
    pub leverage: u32,

    // Expiry
    #[serde(default = "default_expiry_days")]
    pub expiry_days: u64,
    #[serde(default = "default_dms_timeout")]
    pub dead_man_switch_timeout_ms: u64,

    // Fair price
    #[serde(default = "default_ewma_alpha")]
    pub ewma_alpha: f64,
    /// Weight given to Binance mid in blended fair price (0.0 = local only, 1.0 = Binance only).
    #[serde(default = "default_binance_weight")]
    pub binance_weight: f64,
    #[serde(default = "default_update_threshold")]
    pub update_threshold_bps: f64,
    #[serde(default = "default_min_requote_interval")]
    pub min_requote_interval_ms: u64,

    // Spread
    #[serde(default = "default_base_spread")]
    pub base_spread_bps: f64,
    #[serde(default = "default_min_spread")]
    pub min_spread_bps: f64,
    #[serde(default = "default_max_spread")]
    pub max_spread_bps: f64,
    #[serde(default = "default_vol_sensitivity")]
    pub volatility_sensitivity: f64,
    #[serde(default = "default_latency_vol_multiplier")]
    pub latency_vol_multiplier: f64,
    #[serde(default = "default_markout_sensitivity")]
    pub markout_sensitivity: f64,

    // Skew
    #[serde(default = "default_true")]
    pub price_skew_enabled: bool,
    #[serde(default = "default_price_skew_bps")]
    pub price_skew_bps: f64,
    #[serde(default = "default_true")]
    pub size_skew_enabled: bool,
    #[serde(default = "default_size_skew_factor")]
    pub size_skew_factor: f64,
    #[serde(default = "default_min_size_mult")]
    pub min_size_multiplier: f64,
    #[serde(default = "default_max_size_mult")]
    pub max_size_multiplier: f64,
    #[serde(default = "default_emergency_flatten")]
    pub emergency_flatten_ratio: f64,

    // VPIN
    #[serde(default = "default_vpin_bucket")]
    pub vpin_bucket_volume: f64,
    #[serde(default = "default_vpin_buckets")]
    pub vpin_num_buckets: usize,

    // Multi-level quoting
    #[serde(default = "default_num_levels")]
    pub num_levels: u32,
    #[serde(default = "default_level_spacing")]
    pub level_spacing_bps: f64,
    #[serde(default = "default_level_decay")]
    pub level_size_decay: f64,

    // Fast cancel
    #[serde(default = "default_fast_cancel_bps")]
    pub fast_cancel_threshold_bps: f64,
    #[serde(default = "default_max_order_age")]
    pub max_order_age_s: f64,

    // Best price tighten
    #[serde(default = "default_true")]
    pub best_price_tighten_enabled: bool,
    #[serde(default = "default_best_price_margin")]
    pub best_price_margin_bps: f64,

    // Close mode
    #[serde(default = "default_close_threshold")]
    pub close_threshold_ratio: f64,
    #[serde(default = "default_close_spread")]
    pub close_spread_bps: f64,

    // Inventory thresholds
    #[serde(default = "default_one_side_ratio")]
    pub one_side_inventory_ratio: f64,
    #[serde(default = "default_hard_one_side_ratio")]
    pub hard_one_side_inventory_ratio: f64,

    // Trade flow imbalance signal
    /// Rolling window length in seconds for buy/sell volume imbalance (default 5.0).
    #[serde(default = "default_trade_flow_window")]
    pub trade_flow_window_s: f64,
    /// Sensitivity: max fair-price shift in bps when imbalance = 1.0 (default 1.0).
    #[serde(default = "default_trade_flow_sensitivity")]
    pub trade_flow_sensitivity_bps: f64,

    // Depth imbalance signal
    /// Max fair-price shift in bps when depth imbalance = 1.0 (default 1.5).
    #[serde(default = "default_depth_imbalance_sensitivity")]
    pub depth_imbalance_sensitivity_bps: f64,

    // ROC guard — pause quoting on fast price moves
    /// Rolling window in ms to measure price change (default 10000 = 10s).
    #[serde(default = "default_roc_window")]
    pub roc_window_ms: u64,
    /// Trigger threshold in bps (default 30.0).
    #[serde(default = "default_roc_threshold")]
    pub roc_threshold_bps: f64,
    /// Pause duration in ms after trigger (default 15000 = 15s).
    #[serde(default = "default_roc_pause")]
    pub roc_pause_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub max_position_usd: Decimal,
    #[serde(default = "default_max_daily_loss")]
    pub max_daily_loss_usd: Decimal,
    #[serde(default = "default_max_orders_per_min")]
    pub max_orders_per_minute: u32,
    #[serde(default = "default_max_errors_per_min")]
    pub max_errors_per_minute: u32,
    #[serde(default = "default_stale_price")]
    pub stale_price_s: f64,
    #[serde(default = "default_cooldown")]
    pub cooldown_s: u64,
}

// Default value functions
fn default_order_size() -> Decimal { Decimal::new(100, 0) }
fn default_min_order() -> Decimal { Decimal::new(10, 0) }
fn default_max_order() -> Decimal { Decimal::new(5000, 0) }
fn default_leverage() -> u32 { 10 }
fn default_expiry_days() -> u64 { 7 }
fn default_dms_timeout() -> u64 { 60000 }
fn default_ewma_alpha() -> f64 { 0.01 }
fn default_binance_weight() -> f64 { 0.7 }
fn default_update_threshold() -> f64 { 3.0 }
fn default_min_requote_interval() -> u64 { 100 }
fn default_base_spread() -> f64 { 4.0 }
fn default_min_spread() -> f64 { 1.0 }
fn default_max_spread() -> f64 { 20.0 }
fn default_vol_sensitivity() -> f64 { 0.5 }
fn default_latency_vol_multiplier() -> f64 { 2.0 }
fn default_markout_sensitivity() -> f64 { 0.5 }
fn default_true() -> bool { true }
fn default_price_skew_bps() -> f64 { 10.0 }
fn default_size_skew_factor() -> f64 { 1.0 }
fn default_min_size_mult() -> f64 { 0.2 }
fn default_max_size_mult() -> f64 { 1.8 }
fn default_emergency_flatten() -> f64 { 0.8 }
fn default_vpin_bucket() -> f64 { 1.0 }
fn default_vpin_buckets() -> usize { 20 }
fn default_num_levels() -> u32 { 2 }
fn default_level_spacing() -> f64 { 2.0 }
fn default_level_decay() -> f64 { 0.7 }
fn default_fast_cancel_bps() -> f64 { 3.0 }
fn default_max_order_age() -> f64 { 5.0 }
fn default_best_price_margin() -> f64 { 0.1 }
fn default_close_threshold() -> f64 { 0.25 }
fn default_close_spread() -> f64 { 4.0 }
fn default_one_side_ratio() -> f64 { 0.45 }
fn default_hard_one_side_ratio() -> f64 { 0.70 }
fn default_trade_flow_window() -> f64 { 5.0 }
fn default_trade_flow_sensitivity() -> f64 { 1.0 }
fn default_depth_imbalance_sensitivity() -> f64 { 1.5 }
fn default_roc_window() -> u64 { 10_000 }
fn default_roc_threshold() -> f64 { 30.0 }
fn default_roc_pause() -> u64 { 15_000 }
fn default_max_daily_loss() -> Decimal { Decimal::new(500, 0) }
fn default_max_orders_per_min() -> u32 { 200 }
fn default_max_errors_per_min() -> u32 { 10 }
fn default_stale_price() -> f64 { 5.0 }
fn default_cooldown() -> u64 { 60 }
