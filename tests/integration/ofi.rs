//! Integration tests for PIRANA OFI feature

use pirana_core::types::*;
use pirana_features::ofi::OfiCalculator;

fn make_trade(price: f64, qty: f64, side: Side) -> Tick {
    Tick {
        symbol: Symbol::new("tBTCUSD"),
        price,
        quantity: qty,
        side,
        timestamp: chrono::Utc::now(),
        trade_id: 0,
    }
}

#[test]
fn test_ofi_buying_pressure() {
    let mut calc = OfiCalculator::new(10);
    let base_price = 60000.0;

    for i in 0..20 {
        let tick = make_trade(base_price + i as f64 * 10.0, 1.0, Side::Buy);
        let prev = if i == 0 { base_price } else { base_price + (i - 1) as f64 * 10.0 };
        calc.process_tick(&tick, prev);
    }

    assert!(calc.is_buying_pressure());
    assert!(calc.current_ofi() > 0.0);
}

#[test]
fn test_ofi_selling_pressure() {
    let mut calc = OfiCalculator::new(10);
    let base_price = 60000.0;

    for i in 0..20 {
        let tick = make_trade(base_price - i as f64 * 10.0, 1.0, Side::Sell);
        let prev = if i == 0 { base_price } else { base_price - (i - 1) as f64 * 10.0 };
        calc.process_tick(&tick, prev);
    }

    assert!(calc.is_selling_pressure());
    assert!(calc.current_ofi() < 0.0);
}
