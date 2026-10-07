use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct StrategyConfig {
    #[serde(skip)]
    loaded_source: String,
    pub system: SystemConfig,
    pub trading: TradingConfig,
    pub strategy: StrategyParams,
    pub inventory: InventoryConfig,
    pub risk_management: RiskConfig,
    #[serde(default)]
    pub volatility: VolatilityStrategyConfig,
    #[serde(default)]
    pub order_book: OrderBookStrategyConfig,
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
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub reload_interval_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TradingConfig {
    #[allow(dead_code)]
    pub trade_size_btc: f64,
    pub max_open_orders: u32,
    pub max_live_positions: usize,
    pub max_live_buy_equity_pct: f64,
    pub min_impulse_spacing_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct StrategyParams {
    pub entry_zone_spread_usd: f64,
    pub take_profit_distance_usd: f64,
    pub stop_loss_distance_usd: f64,
    pub stop_loss_enabled: bool,
    pub ofi_trigger_threshold: f64,
    pub ofi_window_size: usize,
    pub trade_cooldown_ms: u64,
    pub min_confidence_score: f64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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
    pub range_inventory_pct: f64,
    pub trend_down_inventory_pct: f64,
    pub trend_up_inventory_pct: f64,
}

fn default_min_inventory_btc() -> f64 { 0.0001 }
fn default_max_inventory_btc() -> f64 { 0.05 }
fn default_target_inventory_btc() -> f64 { 0.01 }
fn default_target_inventory_pct() -> f64 { 30.0 }

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    pub max_daily_drawdown_pct: f64,
    pub max_weekly_drawdown_pct: f64,
    pub consecutive_loss_threshold: u32,
    pub vpin_toxicity_threshold: f64,
    pub max_slippage_bps: u32,
    pub position_size_pct: f64,
    pub max_aggregate_exposure_pct: f64,
    #[allow(dead_code)]
    pub max_single_trade_risk_pct: f64,
    pub use_dynamic_winrate_sizing: bool,
    pub min_position_size_pct: f64,
    pub max_position_size_pct: f64,
}


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
#[serde(deny_unknown_fields)]
pub struct TrailingStopConfig {
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
    /// Reject partial reloads that mix startup engines with a new policy.
    /// Odmítnout částečné přenačtení míchající startovní enginy s novou politikou.
    pub fn requires_restart(&self, desired: &Self) -> bool {
        self.loaded_source != desired.loaded_source
    }

    pub fn live_buy_equity_fraction(&self) -> f64 {
        self.trading.max_live_buy_equity_pct / 100.0
    }

    pub fn operator_limits(&self) -> pirana_risk_engine::operator_limits::OperatorLimits {
        pirana_risk_engine::operator_limits::OperatorLimits {
            max_aggregate_exposure: self.risk_management.max_aggregate_exposure_pct / 100.0,
            max_single_trade_risk: self.risk_management.max_single_trade_risk_pct / 100.0,
            max_daily_drawdown: self.risk_management.max_daily_drawdown_pct / 100.0,
            max_weekly_drawdown: self.risk_management.max_weekly_drawdown_pct / 100.0,
            consecutive_loss_threshold: self.risk_management.consecutive_loss_threshold,
            vpin_toxicity_threshold: self.risk_management.vpin_toxicity_threshold,
            baseline_pct: self.risk_management.position_size_pct,
        }
    }

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
        if !(1..=10).contains(&self.trading.max_live_positions)
            || self.trading.min_impulse_spacing_ms < 2000 {
            return Err(Self::invalid("entry capacity must be 1..=10 and spacing >= 2000ms"));
        }
        let entry_pct = finite("max_live_buy_equity_pct", self.trading.max_live_buy_equity_pct)?;
        if !(0.0 < entry_pct && entry_pct <= 10.0) {
            return Err(Self::invalid("max_live_buy_equity_pct must be in (0, 10]"));
        }
        self.operator_limits().validate()?;
        if self.risk_management.use_dynamic_winrate_sizing {
            return Err(Self::invalid("automatic win-rate tuning has been removed"));
        }
        let trigger = finite("trailing min_trigger_usd", self.trailing_stop.min_trigger_usd)?;
        let offset = finite("trailing be_offset_usd", self.trailing_stop.be_offset_usd)?;
        let multiplier = finite("trailing trail_multiplier", self.trailing_stop.trail_multiplier)?;
        if trigger <= 0.0 || offset <= 0.0 || offset >= trigger || multiplier <= 0.0
            || trigger > self.volatility.max_tp_usd {
            return Err(Self::invalid("invalid profit trailing trigger, offset or multiplier"));
        }
        let down = finite("trend_down_inventory_pct", self.inventory.trend_down_inventory_pct)?;
        let range = finite("range_inventory_pct", self.inventory.range_inventory_pct)?;
        let up = finite("trend_up_inventory_pct", self.inventory.trend_up_inventory_pct)?;
        if !(0.0 < down && down <= range && range <= up && up <= 90.0) {
            return Err(Self::invalid("regime inventory requires 0 < down <= range <= up <= 90"));
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
        let mut config: StrategyConfig = toml::from_str(&content)?;
        config.validate()?;
        config.loaded_source = content;
        Ok(config)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate() -> StrategyConfig {
        let source = include_str!("../strategy.toml");
        let mut config: StrategyConfig = toml::from_str(source).unwrap();
        config.loaded_source = source.to_owned();
        config
    }
    #[test]
    fn canonical_operator_ceilings() {
        let config = candidate();
        config.validate().unwrap();
        assert_eq!(config.trading.max_live_positions, 10);
        assert_eq!(config.live_buy_equity_fraction(), 0.10);
        assert_eq!(config.operator_limits().max_daily_drawdown, 0.01);
        assert_eq!(config.operator_limits().max_aggregate_exposure, 0.60);
        assert!(!config.strategy.stop_loss_enabled);
    }
    #[test]
    fn missing_explicit_policy_is_an_error() {
        let source: toml::Value = toml::from_str(include_str!("../strategy.toml")).unwrap();
        let roundtrip: StrategyConfig = toml::from_str(&toml::to_string(&source).unwrap()).unwrap();
        roundtrip.validate().unwrap();
        assert!(!roundtrip.strategy.stop_loss_enabled);
        assert!(roundtrip.trailing_stop.enabled);
        let fields = [
            ("trading", "max_live_positions"), ("trading", "max_live_buy_equity_pct"),
            ("trading", "min_impulse_spacing_ms"), ("trading", "max_open_orders"),
            ("inventory", "range_inventory_pct"), ("inventory", "trend_down_inventory_pct"),
            ("inventory", "trend_up_inventory_pct"), ("strategy", "stop_loss_enabled"),
            ("trailing_stop", "enabled"),
            ("risk_management", "max_daily_drawdown_pct"),
            ("risk_management", "max_weekly_drawdown_pct"),
            ("risk_management", "consecutive_loss_threshold"),
            ("risk_management", "vpin_toxicity_threshold"),
            ("risk_management", "max_slippage_bps"),
            ("risk_management", "position_size_pct"),
            ("risk_management", "max_aggregate_exposure_pct"),
            ("risk_management", "max_single_trade_risk_pct"),
            ("risk_management", "use_dynamic_winrate_sizing"),
            ("risk_management", "min_position_size_pct"),
            ("risk_management", "max_position_size_pct"),
        ];
        for (section, field) in fields {
            let mut edited = source.clone();
            assert!(edited[section].as_table_mut().unwrap().remove(field).is_some());
            assert!(toml::from_str::<StrategyConfig>(&toml::to_string(&edited).unwrap()).is_err(),
                    "missing explicit {section}.{field}");
        }
    }
    #[test]
    fn missing_trailing_section_is_an_error() {
        let mut source: toml::Value = toml::from_str(include_str!("../strategy.toml")).unwrap();
        assert!(source.as_table_mut().unwrap().remove("trailing_stop").is_some());
        assert!(toml::from_str::<StrategyConfig>(&toml::to_string(&source).unwrap()).is_err());
        assert!(candidate().trailing_stop.enabled);
    }
    #[test]
    fn unknown_operator_fields_and_sections_are_errors() {
        let source: toml::Value = toml::from_str(include_str!("../strategy.toml")).unwrap();
        for section in ["system", "trading", "strategy", "inventory", "risk_management", "trailing_stop"] {
            let mut edited = source.clone();
            edited[section].as_table_mut().unwrap().insert(
                "misspelled_operator_setting".to_owned(), toml::Value::Integer(10));
            assert!(toml::from_str::<StrategyConfig>(&toml::to_string(&edited).unwrap()).is_err(),
                    "ignored unknown field in {section}");
        }
        let mut edited = source.clone();
        edited.as_table_mut().unwrap().insert("traling_stop".to_owned(), source["trailing_stop"].clone());
        assert!(toml::from_str::<StrategyConfig>(&toml::to_string(&edited).unwrap()).is_err());
    }
    #[test]
    fn invalid_settings_fail_closed() {
        let mut c = candidate(); c.trading.max_live_positions = 11;
        assert!(c.validate().is_err());
        c = candidate(); c.trading.max_live_buy_equity_pct = f64::NAN;
        assert!(c.validate().is_err());
        c = candidate(); c.inventory.trend_down_inventory_pct = 50.0;
        assert!(c.validate().is_err());
        c = candidate(); c.trading.min_impulse_spacing_ms = 1999;
        assert!(c.validate().is_err());
    }
    #[test]
    fn removed_tuning_and_invalid_trailing_are_rejected() {
        let mut config = candidate();
        config.risk_management.use_dynamic_winrate_sizing = true;
        assert!(config.validate().is_err());
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, 121.0] {
            let mut config = candidate();
            config.trailing_stop.min_trigger_usd = invalid;
            assert!(config.validate().is_err());
        }
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, 25.0] {
            let mut config = candidate();
            config.trailing_stop.be_offset_usd = invalid;
            assert!(config.validate().is_err());
        }
        assert!(!candidate().risk_management.use_dynamic_winrate_sizing);
        assert!(candidate().trailing_stop.enabled);
    }

    #[test]
    fn edited_policy_is_pending_restart() {
        let applied = candidate(); let mut desired = applied.clone();
        assert!(!applied.requires_restart(&desired));
        desired.loaded_source.push_str("\n# Operator edit\n# Úprava operátora\n");
        assert!(applied.requires_restart(&desired));
    }
}
