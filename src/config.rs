use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub system: SystemConfig,
    pub trading: TradingConfig,
    pub strategy: StrategyParams,
    pub inventory: InventoryConfig,
    pub risk_management: RiskConfig,
    #[serde(default)]
    pub volatility: VolatilityStrategyConfig,
    #[serde(default)]
    pub order_book: OrderBookStrategyConfig,
    #[serde(default)]
    pub trailing_stop: TrailingStopConfig,
    #[serde(default)]
    pub profit_skimmer: ProfitSkimmerConfig,
    #[serde(default)]
    pub adaptive_cooldown: AdaptiveCooldownConfig,
    #[serde(default)]
    pub lead_lag: pirana_features::cross_exchange::LeadLagConfig,
    #[serde(default)]
    pub hawkes_process: pirana_features::hawkes::HawkesConfig,
    #[serde(default)]
    pub vpin_guard: pirana_features::vpin::VpinConfig,
    #[serde(default)]
    pub avellaneda_stoikov: pirana_execution::avellaneda_stoikov::AvellanedaStoikovConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SystemConfig {
    pub reload_interval_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TradingConfig {
    #[allow(dead_code)]
    pub trade_size_btc: f64,
    pub max_open_orders: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyParams {
    pub entry_zone_spread_usd: f64,
    pub take_profit_distance_usd: f64,
    pub stop_loss_distance_usd: f64,
    pub ofi_trigger_threshold: f64,
    pub ofi_window_size: usize,
    pub trade_cooldown_ms: u64,
    pub min_confidence_score: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InventoryConfig {
    #[serde(default = "default_min_inventory_btc")]
    pub min_inventory_btc: f64,
    #[serde(default = "default_max_inventory_btc")]
    pub max_inventory_btc: f64,
    #[serde(default = "default_true")]
    pub use_dynamic_inventory: bool,
}

fn default_min_inventory_btc() -> f64 { 0.0001 }
fn default_max_inventory_btc() -> f64 { 0.05 }

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    #[serde(default = "default_max_slippage_bps")]
    pub max_slippage_bps: u32,
    #[serde(default = "default_position_size_pct")]
    pub position_size_pct: f64,
    #[serde(default = "default_max_aggregate_exposure_pct")]
    pub max_aggregate_exposure_pct: f64,
    #[serde(default = "default_max_single_trade_risk_pct")]
    #[allow(dead_code)]
    pub max_single_trade_risk_pct: f64,
    #[serde(default = "default_true")]
    pub use_dynamic_winrate_sizing: bool,
    #[serde(default = "default_min_position_size_pct")]
    pub min_position_size_pct: f64,
    #[serde(default = "default_max_position_size_pct")]
    pub max_position_size_pct: f64,
}

fn default_max_slippage_bps() -> u32 { 5 }
fn default_position_size_pct() -> f64 { 5.0 }
fn default_max_aggregate_exposure_pct() -> f64 { 90.0 }
fn default_max_single_trade_risk_pct() -> f64 { 5.0 }
fn default_min_position_size_pct() -> f64 { 1.0 }
fn default_max_position_size_pct() -> f64 { 15.0 }

#[derive(Debug, Deserialize, Clone)]
pub struct VolatilityStrategyConfig {
    #[serde(default = "default_true")]
    pub use_dynamic_atr: bool,
    #[serde(default = "default_atr_period")]
    pub atr_period: usize,
    #[serde(default = "default_ticks_per_bar")]
    pub ticks_per_bar: usize,
    #[serde(default = "default_atr_tp_multiplier")]
    pub atr_tp_multiplier: f64,
    #[serde(default = "default_atr_sl_multiplier")]
    pub atr_sl_multiplier: f64,
    #[serde(default = "default_min_tp_usd")]
    pub min_tp_usd: f64,
    #[serde(default = "default_max_tp_usd")]
    pub max_tp_usd: f64,
    #[serde(default = "default_min_sl_usd")]
    pub min_sl_usd: f64,
    #[serde(default = "default_max_sl_usd")]
    pub max_sl_usd: f64,
}

fn default_true() -> bool { true }
fn default_atr_period() -> usize { 14 }
fn default_ticks_per_bar() -> usize { 50 }
fn default_atr_tp_multiplier() -> f64 { 0.5 }
fn default_atr_sl_multiplier() -> f64 { 4.0 }
fn default_min_tp_usd() -> f64 { 4.0 }
fn default_max_tp_usd() -> f64 { 25.0 }
fn default_min_sl_usd() -> f64 { 25.0 }
fn default_max_sl_usd() -> f64 { 80.0 }

impl Default for VolatilityStrategyConfig {
    fn default() -> Self {
        Self {
            use_dynamic_atr: true,
            atr_period: 14,
            ticks_per_bar: 50,
            atr_tp_multiplier: 0.5,
            atr_sl_multiplier: 4.0,
            min_tp_usd: 4.0,
            max_tp_usd: 25.0,
            min_sl_usd: 25.0,
            max_sl_usd: 80.0,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct OrderBookStrategyConfig {
    #[serde(default = "default_true")]
    pub use_l2_depth_imbalance: bool,
    #[serde(default = "default_l2_depth_levels")]
    pub l2_depth_levels: usize,
    #[serde(default = "default_l2_weight_decay")]
    pub l2_weight_decay: f64,
    #[serde(default = "default_l2_weight_alpha")]
    pub l2_weight_alpha: f64,
    #[serde(default = "default_min_l2_imbalance_threshold")]
    pub min_l2_imbalance_threshold: f64,
}

fn default_l2_depth_levels() -> usize { 5 }
fn default_l2_weight_decay() -> f64 { 0.5 }
fn default_l2_weight_alpha() -> f64 { 0.40 }
fn default_min_l2_imbalance_threshold() -> f64 { 0.15 }

impl Default for OrderBookStrategyConfig {
    fn default() -> Self {
        Self {
            use_l2_depth_imbalance: true,
            l2_depth_levels: 5,
            l2_weight_decay: 0.5,
            l2_weight_alpha: 0.40,
            min_l2_imbalance_threshold: 0.15,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct TrailingStopConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_trailing_min_trigger_usd")]
    pub min_trigger_usd: f64,
    #[serde(default = "default_trailing_be_offset_usd")]
    pub be_offset_usd: f64,
    #[serde(default = "default_trailing_trail_multiplier")]
    pub trail_multiplier: f64,
}

fn default_trailing_min_trigger_usd() -> f64 { 4.0 }
fn default_trailing_be_offset_usd() -> f64 { 1.0 }
fn default_trailing_trail_multiplier() -> f64 { 0.5 }

impl Default for TrailingStopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_trigger_usd: 4.0,
            be_offset_usd: 1.0,
            trail_multiplier: 0.5,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProfitSkimmerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_btc_lock_pct")]
    pub btc_lock_pct: f64,
    #[serde(default = "default_true")]
    pub exclude_from_trading_margin: bool,
}

fn default_btc_lock_pct() -> f64 { 10.0 }

impl Default for ProfitSkimmerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            btc_lock_pct: 10.0,
            exclude_from_trading_margin: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AdaptiveCooldownConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_min_cooldown_ms")]
    pub min_ms: u64,
    #[serde(default = "default_max_cooldown_ms")]
    pub max_ms: u64,
}

fn default_min_cooldown_ms() -> u64 { 8000 }
fn default_max_cooldown_ms() -> u64 { 60000 }

impl Default for AdaptiveCooldownConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_ms: 8000,
            max_ms: 60000,
        }
    }
}

impl StrategyConfig {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string("strategy.toml")?;
        let config: StrategyConfig = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::error!("Failed to load strategy.toml, using safe defaults: {}", e);
            StrategyConfig {
                system: SystemConfig { reload_interval_seconds: 60 },
                trading: TradingConfig { trade_size_btc: 0.0001, max_open_orders: 1 },
                strategy: StrategyParams {
                    entry_zone_spread_usd: 1.0,
                    take_profit_distance_usd: 5.0,
                    stop_loss_distance_usd: 50.0,
                    ofi_trigger_threshold: 0.75,
                    ofi_window_size: 100,
                    trade_cooldown_ms: 28000,
                    min_confidence_score: 0.95,
                },
                inventory: InventoryConfig {
                    min_inventory_btc: 0.0001,
                    max_inventory_btc: 0.05,
                    use_dynamic_inventory: true,
                },
                risk_management: RiskConfig {
                    max_slippage_bps: 5,
                    position_size_pct: 5.0,
                    max_aggregate_exposure_pct: 90.0,
                    max_single_trade_risk_pct: 5.0,
                    use_dynamic_winrate_sizing: true,
                    min_position_size_pct: 1.0,
                    max_position_size_pct: 15.0,
                },
                volatility: VolatilityStrategyConfig::default(),
                order_book: OrderBookStrategyConfig::default(),
                trailing_stop: TrailingStopConfig::default(),
                profit_skimmer: ProfitSkimmerConfig::default(),
                adaptive_cooldown: AdaptiveCooldownConfig::default(),
                lead_lag: pirana_features::cross_exchange::LeadLagConfig::default(),
                hawkes_process: pirana_features::hawkes::HawkesConfig::default(),
                vpin_guard: pirana_features::vpin::VpinConfig::default(),
                avellaneda_stoikov: pirana_execution::avellaneda_stoikov::AvellanedaStoikovConfig::default(),
            }
        })
    }
}
