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
    #[serde(default = "default_target_inventory_btc")]
    pub target_inventory_btc: f64,
    /// Cilovy podil obchodovatelne equity drzeny v BTC (v procentech).
    ///
    /// Nahrazuje pevnou konstantu `target_inventory_btc`, ktera pri malem
    /// uctu prekracovala 100 % kapitalu a nutila bota skoupit celou penezenku.
    /// Je-li 0 nebo zaporne, pouzije se zpetne kompatibilni
    /// `target_inventory_btc`.
    #[serde(default = "default_target_inventory_pct")]
    pub target_inventory_pct: f64,
    #[serde(default = "default_true")]
    pub use_dynamic_inventory: bool,
}

fn default_min_inventory_btc() -> f64 { 0.0001 }
fn default_max_inventory_btc() -> f64 { 0.05 }
fn default_target_inventory_btc() -> f64 { 0.01 }
fn default_target_inventory_pct() -> f64 { 30.0 }

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
    fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error> {
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()))
    }

    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        let finite = |name: &str, value: f64| -> Result<f64, Box<dyn std::error::Error>> {
            if !value.is_finite() {
                return Err(Self::invalid(format!("{name} must be finite")));
            }
            Ok(value)
        };

        if !(1..=3600).contains(&self.system.reload_interval_seconds) {
            return Err(Self::invalid("reload_interval_seconds must be in 1..=3600"));
        }
        if !(1..=10).contains(&self.trading.max_open_orders) {
            return Err(Self::invalid("max_open_orders must be in hard range 1..=10"));
        }
        if self.strategy.ofi_window_size == 0 || self.strategy.trade_cooldown_ms == 0 {
            return Err(Self::invalid("OFI window and trade cooldown must be > 0"));
        }
        let ofi = finite("ofi_trigger_threshold", self.strategy.ofi_trigger_threshold)?;
        let confidence = finite("min_confidence_score", self.strategy.min_confidence_score)?;
        if !(0.0 < ofi && ofi <= 1.0) {
            return Err(Self::invalid("ofi_trigger_threshold must be in (0, 1]"));
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(Self::invalid("min_confidence_score must be in [0, 1]"));
        }

        let max_exp = finite("max_aggregate_exposure_pct", self.risk_management.max_aggregate_exposure_pct)?;
        let max_single = finite("max_single_trade_risk_pct", self.risk_management.max_single_trade_risk_pct)?;
        let min_pos = finite("min_position_size_pct", self.risk_management.min_position_size_pct)?;
        let baseline = finite("position_size_pct", self.risk_management.position_size_pct)?;
        let max_pos = finite("max_position_size_pct", self.risk_management.max_position_size_pct)?;
        if !(0.01..=90.0).contains(&max_exp) {
            return Err(Self::invalid("max_aggregate_exposure_pct exceeds hard 90% cap"));
        }
        if !(0.01..=5.0).contains(&max_single) {
            return Err(Self::invalid("max_single_trade_risk_pct exceeds hard 5% cap"));
        }
        if !(1.0 <= min_pos && min_pos <= baseline && baseline <= max_pos && max_pos <= 25.0) {
            return Err(Self::invalid("position sizing must satisfy 1 <= min <= baseline <= max <= 25"));
        }
        if !(1..=10).contains(&self.risk_management.max_slippage_bps) {
            return Err(Self::invalid("max_slippage_bps must be in hard range 1..=10"));
        }

        if self.volatility.atr_period == 0 || self.volatility.ticks_per_bar == 0 {
            return Err(Self::invalid("atr_period and ticks_per_bar must be > 0"));
        }
        let min_tp = finite("min_tp_usd", self.volatility.min_tp_usd)?;
        let max_tp = finite("max_tp_usd", self.volatility.max_tp_usd)?;
        let min_sl = finite("min_sl_usd", self.volatility.min_sl_usd)?;
        let max_sl = finite("max_sl_usd", self.volatility.max_sl_usd)?;
        if !(0.0 < min_tp && min_tp <= max_tp && 0.0 < min_sl && min_sl <= max_sl) {
            return Err(Self::invalid("TP/SL min/max invariant violated"));
        }
        if self.adaptive_cooldown.min_ms == 0
            || self.adaptive_cooldown.min_ms > self.adaptive_cooldown.max_ms
        {
            return Err(Self::invalid("adaptive cooldown min_ms/max_ms invariant violated"));
        }
        Ok(())
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string("strategy.toml")?;
        let config: StrategyConfig = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

}
