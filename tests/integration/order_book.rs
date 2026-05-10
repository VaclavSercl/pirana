//! Integration tests for PIRANA order book

use pirana_core::order_book::OrderBook;
use pirana_core::types::*;

#[test]
fn test_order_book_bid_ask() {
    let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

    book.update_level(Side::Buy, 60000.0, 1.5, 10);
    book.update_level(Side::Buy, 59999.0, 2.0, 5);
    book.update_level(Side::Sell, 60001.0, 1.0, 8);
    book.update_level(Side::Sell, 60002.0, 0.5, 3);

    assert_eq!(book.best_bid().unwrap().price, 60000.0);
    assert_eq!(book.best_ask().unwrap().price, 60001.0);
    assert_eq!(book.spread().unwrap(), 1.0);
    assert_eq!(book.mid_price().unwrap(), 60000.5);
}

#[test]
fn test_order_book_imbalance() {
    let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

    book.update_level(Side::Buy, 60000.0, 3.0, 10);
    book.update_level(Side::Sell, 60001.0, 1.0, 8);

    let imbalance = book.book_imbalance(5);
    assert!(imbalance > 0.0);
}

#[test]
fn test_order_book_vwap() {
    let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

    book.update_level(Side::Buy, 60000.0, 1.0, 5);
    book.update_level(Side::Buy, 59999.0, 2.0, 3);
    book.update_level(Side::Sell, 60001.0, 1.0, 4);
    book.update_level(Side::Sell, 60002.0, 0.5, 2);

    let bid_vwap = book.vwap(Side::Buy, 1.5).unwrap();
    assert!(bid_vwap > 59999.0 && bid_vwap < 60000.0);

    let ask_vwap = book.vwap(Side::Sell, 1.5).unwrap();
    assert!(ask_vwap > 60001.0 && ask_vwap < 60002.0);
}

#[test]
fn test_order_book_remove_level() {
    let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

    book.update_level(Side::Buy, 60000.0, 1.5, 10);
    assert!(book.best_bid().is_some());

    // Remove by setting quantity to 0
    book.update_level(Side::Buy, 60000.0, 0.0, 0);
    assert!(book.best_bid().is_none());
}
