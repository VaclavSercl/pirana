use pirana_core::types::Side;
use std::collections::VecDeque;

/// FlowCalculator — rolling normalized buy/sell flow + high-water mark.
///
/// Měří tok nákupní vs prodejní tlak v okně `window_size` ticků
/// a udržuje high-water mark (HWM) z posledních `hwm_window_size` ticků.
///
/// Flow = (buy_vol − sell_vol) / (buy_vol + sell_vol)  ∈ [−1, 1]
///
/// Použití: pullback_flow_signal = flow > threshold && price < hwm * factor
#[derive(Debug)]
pub struct FlowCalculator {
    /// Okno signed objemů (+ nákup, − prodej).
    window: VecDeque<f64>,
    window_size: usize,
    /// Okno cen pro HWM.
    hwm_window: VecDeque<f64>,
    hwm_window_size: usize,
    /// Aktualizovaná flow po každém ticku.
    current_flow: f64,
    /// Aktualizovaný HWM po každém ticku.
    current_hwm: f64,
}

impl FlowCalculator {
    pub fn new(window_size: usize, hwm_window_size: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            hwm_window: VecDeque::with_capacity(hwm_window_size),
            hwm_window_size,
            current_flow: 0.0,
            current_hwm: 0.0,
        }
    }

    /// Zpracuje nový trade tick.
    ///
    /// `side` určuje směr (Buy = +qty, Sell = −qty).
    /// `now_ms` je čas ticku (pro budoucí extenzi decay; zatím nevyužit).
    pub fn process_trade(&mut self, side: Side, qty: f64, _price: f64, _now_ms: u64) {
        let signed_vol = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };

        // Rolling okno flow.
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(signed_vol);

        // Rolling okno HWM (použijeme cenu ticku).
        if self.hwm_window.len() >= self.hwm_window_size {
            self.hwm_window.pop_front();
        }
        self.hwm_window.push_back(_price);

        // Výpočet normalizovaného flow.
        let sum: f64 = self.window.iter().sum();
        let abs_sum: f64 = self.window.iter().map(|v| v.abs()).sum();
        self.current_flow = if abs_sum > 0.0 { sum / abs_sum } else { 0.0 };

        // Výpočet HWM.
        self.current_hwm = self.hwm_window.iter().cloned().fold(0.0, f64::max);
    }

    /// Aktuální normalizovaná hodnota flow [−1, 1].
    /// Kladný = nákupní tlak, záporný = prodejní tlak.
    pub fn current_flow(&self) -> f64 {
        self.current_flow
    }

    /// Aktuální high-water mark (max cena z hwm okna).
    pub fn hwm(&self) -> f64 {
        self.current_hwm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_initial_state() {
        let calc = FlowCalculator::new(20, 100);
        assert_eq!(calc.current_flow(), 0.0);
        assert_eq!(calc.hwm(), 0.0);
    }

    #[test]
    fn test_all_buy_flow_is_positive() {
        let mut calc = FlowCalculator::new(20, 100);
        for i in 0..25 {
            calc.process_trade(Side::Buy, 1.0, 60_000.0 + i as f64, i as u64);
        }
        assert!(calc.current_flow() > 0.9);
    }

    #[test]
    fn test_all_sell_flow_is_negative() {
        let mut calc = FlowCalculator::new(20, 100);
        for i in 0..25 {
            calc.process_trade(Side::Sell, 1.0, 60_000.0 - i as f64, i as u64);
        }
        assert!(calc.current_flow() < -0.9);
    }

    #[test]
    fn test_mixed_flow_balanced() {
        let mut calc = FlowCalculator::new(20, 100);
        for i in 0..20 {
            calc.process_trade(Side::Buy, 1.0, 60_000.0, i as u64);
            calc.process_trade(Side::Sell, 1.0, 60_000.0, i as u64);
        }
        // 10 buy + 10 sell (stejný objem) → flow ≈ 0
        assert!(calc.current_flow().abs() < 0.01);
    }

    #[test]
    fn test_flow_rolling_window() {
        let mut calc = FlowCalculator::new(5, 100);
        // 5 sell ticků
        for i in 0..5 {
            calc.process_trade(Side::Sell, 1.0, 60_000.0, i as u64);
        }
        assert!(calc.current_flow() < -0.9);
        // 5 buy ticků — přepíší okno
        for i in 0..5 {
            calc.process_trade(Side::Buy, 1.0, 60_000.0, i as u64);
        }
        assert!(calc.current_flow() > 0.9);
    }

    #[test]
    fn test_hwm_tracks_max_price() {
        let mut calc = FlowCalculator::new(20, 5);
        let prices = [60_000.0, 60_100.0, 60_200.0, 60_150.0, 60_300.0];
        for (i, &p) in prices.iter().enumerate() {
            calc.process_trade(Side::Buy, 1.0, p, i as u64);
        }
        assert_eq!(calc.hwm(), 60_300.0);
    }

    #[test]
    fn test_hwm_rolling_window() {
        let mut calc = FlowCalculator::new(20, 3);
        // Okno 3: po 3 tickůch se nejstarší odstraní.
        calc.process_trade(Side::Buy, 1.0, 60_000.0, 0);
        calc.process_trade(Side::Buy, 1.0, 60_500.0, 1);
        calc.process_trade(Side::Buy, 1.0, 60_200.0, 2);
        assert_eq!(calc.hwm(), 60_500.0);
        // Nový tick vytlačí 60_000 z okna.
        calc.process_trade(Side::Buy, 1.0, 60_100.0, 3);
        // Okno: [60_500, 60_200, 60_100] → HWM = 60_500
        assert_eq!(calc.hwm(), 60_500.0);
        // Nový tick vytlačí 60_500.
        calc.process_trade(Side::Buy, 1.0, 60_400.0, 4);
        // Okno: [60_200, 60_100, 60_400] → HWM = 60_400
        assert_eq!(calc.hwm(), 60_400.0);
    }

    #[test]
    fn test_pullback_flow_signal_scenario() {
        // Simulace: po HWM 60_300 cena klesne na 60_100, pak mírně odraz.
        let mut calc = FlowCalculator::new(20, 100);
        // 10 buy ticků na rostoucí ceně → HWM = 60_300
        for i in 0..10 {
            calc.process_trade(Side::Buy, 1.0, 60_000.0 + i as f64 * 30.0, i as u64);
        }
        assert_eq!(calc.hwm(), 60_270.0);
        // 5 sell ticků → cena klesá
        for i in 0..5 {
            calc.process_trade(Side::Sell, 1.0, 60_200.0 - i as f64 * 20.0, 10 + i as u64);
        }
        // 5 buy ticků → odraz, kladný flow
        for i in 0..5 {
            calc.process_trade(Side::Buy, 1.0, 60_100.0 + i as f64 * 10.0, 15 + i as u64);
        }
        let flow = calc.current_flow();
        let hwm = calc.hwm();
        let price = 60_140.0; // aktuální cena
        let flow_threshold = 0.05;
        let hwm_factor = 0.999;
        let pullback_flow_signal = flow > flow_threshold && price < hwm * hwm_factor;
        assert!(
            pullback_flow_signal,
            "flow={}, hwm={}, price={}",
            flow, hwm, price
        );
    }

    #[test]
    fn test_empty_window_after_construction() {
        let calc = FlowCalculator::new(10, 50);
        assert_eq!(calc.current_flow(), 0.0);
        assert_eq!(calc.hwm(), 0.0);
    }

    #[test]
    fn test_single_trade() {
        let mut calc = FlowCalculator::new(10, 50);
        calc.process_trade(Side::Buy, 2.0, 60_000.0, 0);
        assert_eq!(calc.current_flow(), 1.0);
        assert_eq!(calc.hwm(), 60_000.0);
    }
}
