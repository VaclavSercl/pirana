//! Public Bitfinex channel identity and bounded, reconnect-safe trade admission.
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

const MAX_TRADE_IDS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel { Ticker, Book, Trades }

pub struct RoutedFrame {
    pub data: Value,
    pub channel: Channel,
    pub previous_trade_price: f64,
    pub received_ms: i64,
    pub entry_ready: bool,
}

#[derive(Default)]
pub struct MarketEvents {
    channels: HashMap<u64, Channel>,
    book_snapshot: bool,
    trades_snapshot: bool,
    book_seen: Option<Instant>,
    ticker_seen: Option<Instant>,
    previous_trade_price: Option<f64>,
    seen: HashSet<i64>,
    ids: VecDeque<(i64, i64)>,
    retired_through_ms: i64,
    last_trade_key: Option<(i64, i64)>,
}

impl MarketEvents {
    pub fn reconnect(&mut self) {
        self.channels.clear();
        self.book_snapshot = false;
        self.trades_snapshot = false;
        self.book_seen = None;
        self.ticker_seen = None;
        self.previous_trade_price = None;
        // Economic identities survive a reconnect. No snapshot is executable.
    }

    pub fn ready(&self, now: Instant) -> bool {
        self.book_snapshot && self.trades_snapshot
            && self.book_seen.is_some_and(|t| now.checked_duration_since(t).is_some_and(|age| age <= Duration::from_secs(30)))
            && self.ticker_seen.is_some_and(|t| now.checked_duration_since(t).is_some_and(|age| age <= Duration::from_secs(30)))
    }

    pub fn route(&mut self, data: Value, now: Instant, received_ms: i64) -> Option<RoutedFrame> {
        if received_ms <= 0 || chrono::DateTime::from_timestamp_millis(received_ms).is_none() {
            return None;
        }
        if data.is_object() {
            if data["event"] == "subscribed" && data["symbol"] == "tBTCUSD" {
                let id = data["chanId"].as_u64()?;
                let channel = match data["channel"].as_str()? {
                    "ticker" => Channel::Ticker,
                    "book" if data["prec"] == "P0" => Channel::Book,
                    "trades" => Channel::Trades,
                    _ => return None,
                };
                if self.channels.values().any(|existing| *existing == channel) || self.channels.contains_key(&id) {
                    return None;
                }
                self.channels.insert(id, channel);
            }
            return None;
        }
        let a = data.as_array()?;
        let channel = *self.channels.get(&a.first()?.as_u64()?)?;
        let Some(payload) = a.get(1) else {
            self.invalidate(channel);
            return None;
        };
        if payload.as_str() == Some("hb") && a.len() == 2 {
            if channel == Channel::Book && self.book_snapshot { self.book_seen = Some(now); }
            return None;
        }
        if (channel != Channel::Trades && a.len() != 2)
            || (channel == Channel::Trades && a.len() != if payload.is_array() { 2 } else { 3 })
        {
            self.invalidate(channel);
            return None;
        }
        let mut previous_trade_price = 0.0;
        match channel {
            Channel::Ticker => {
                let ticker = match payload.as_array() {
                    Some(ticker) => ticker,
                    None => { self.ticker_seen = None; return None; }
                };
                if ticker.len() != 10 || !positive(ticker.get(6)?) {
                    self.ticker_seen = None;
                    return None;
                }
                self.ticker_seen = Some(now);
            }
            Channel::Book => {
                let rows = match a.get(1)?.as_array() {
                    Some(rows) => rows,
                    None => { self.book_snapshot = false; return None; }
                };
                if rows.is_empty() || rows.first()?.is_array() {
                    if !rows.iter().all(|r| r.as_array().is_some_and(|r| valid_book_row(r))) {
                        self.book_snapshot = false;
                        return None;
                    }
                    self.book_snapshot = true;
                } else if !self.book_snapshot || !valid_book_row(rows) {
                    // A rejected delta creates a gap: old depth is no longer a
                    // trustworthy book. Only a new snapshot can restore it.
                    self.book_snapshot = false;
                    return None;
                }
                self.book_seen = Some(now);
            }
            Channel::Trades => {
                if let Some(snapshot) = a.get(1)?.as_array() {
                    self.trades_snapshot = false;
                    if snapshot.len() > MAX_TRADE_IDS { return None; }
                    // Validate the entire snapshot before mutating economic IDs.
                    let trades: Option<Vec<_>> = snapshot.iter()
                        .map(|trade| {
                            let trade = valid_trade(trade.as_array()?)?;
                            (received_ms.checked_sub(trade.1)? >= -5_000).then_some(trade)
                        }).collect();
                    for (id, ms, _, _) in trades? {
                        self.remember(id, ms);
                    }
                    self.trades_snapshot = true;
                    return None;
                }
                if !self.trades_snapshot || !matches!(a.get(1)?.as_str(), Some("te" | "tu")) { return None; }
                let (id, ms, _, price) = valid_trade(a.get(2)?.as_array()?)?;
                let age_ms = received_ms.checked_sub(ms)?;
                if !(-5_000..=30_000).contains(&age_ms) || ms <= self.retired_through_ms
                    || self.seen.contains(&id) || self.last_trade_key.is_some_and(|key| (ms, id) <= key) {
                    return None;
                }
                self.remember(id, ms);
                self.last_trade_key = Some((ms, id));
                previous_trade_price = self.previous_trade_price.replace(price).unwrap_or(price);
            }
        }
        Some(RoutedFrame { data, channel, previous_trade_price, received_ms, entry_ready: self.ready(now) })
    }

    fn invalidate(&mut self, channel: Channel) {
        match channel {
            Channel::Book => self.book_snapshot = false,
            Channel::Trades => self.trades_snapshot = false,
            Channel::Ticker => self.ticker_seen = None,
        }
    }

    fn remember(&mut self, id: i64, ms: i64) {
        if self.seen.insert(id) { self.ids.push_back((id, ms)); }
        while self.ids.len() > MAX_TRADE_IDS {
            if let Some((old, timestamp)) = self.ids.pop_front() {
                self.seen.remove(&old);
                self.retired_through_ms = self.retired_through_ms.max(timestamp);
            }
        }
    }
}

fn positive(value: &Value) -> bool { value.as_f64().is_some_and(|v| v.is_finite() && v > 0.0) }
fn valid_book_row(row: &[Value]) -> bool {
    row.len() == 3 && positive(&row[0]) && row[1].as_u64().is_some_and(|c| c <= u32::MAX as u64)
        && row[2].as_f64().is_some_and(|v| v.is_finite() && v != 0.0
            && (row[1].as_u64() != Some(0) || v.abs() == 1.0))
}
fn valid_trade(row: &[Value]) -> Option<(i64, i64, f64, f64)> {
    if row.len() != 4 { return None; }
    let (id, ms, amount, price) = (row[0].as_i64()?, row[1].as_i64()?, row[2].as_f64()?, row[3].as_f64()?);
    (id > 0 && ms > 0 && chrono::DateTime::from_timestamp_millis(ms).is_some() && amount.is_finite() && amount != 0.0 && price.is_finite() && price > 0.0)
        .then_some((id, ms, amount, price))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn subscribe(r: &mut MarketEvents, channel: &str, id: u64) {
        r.route(json!({"event":"subscribed","symbol":"tBTCUSD","channel":channel,"chanId":id,"prec":"P0"}), Instant::now(), 10_000);
    }
    #[test]
    fn identities_snapshots_duplicates_and_reconnect() {
        let mut r = MarketEvents::default();
        let now = Instant::now();
        assert!(r.route(json!([4, [[123, 9000, 1, 70000]]]), now, 10000).is_none());
        subscribe(&mut r, "trades", 4);
        assert!(r.route(json!([4, [[123, 9000, 1, 70000]]]), now, 10000).is_none());
        assert!(!r.book_snapshot);
        assert!(r.route(json!([4,"te",[123,9000,1,70000]]), now,10000).is_none());
        let first = r.route(json!([4,"te",[124,10000,1,70001]]), now,10000).unwrap();
        assert_eq!(first.previous_trade_price, 70001.0);
        assert!(r.route(json!([4,"tu",[124,10000,1,70001]]), now,10000).is_none());
        assert_eq!(r.route(json!([4,"te",[125,10001,1,70002]]), now,10001).unwrap().previous_trade_price,70001.0);
        r.reconnect();
        assert!(!r.ready(now));
        subscribe(&mut r,"trades",5);
        r.route(json!([5,[]]),now,10001);
        assert!(r.route(json!([5,"te",[125,10001,1,70002]]),now,10001).is_none());
    }
    #[test]
    fn wrong_shape_and_stale_book_cannot_be_ready() {
        let mut r = MarketEvents::default();
        subscribe(&mut r,"book",1);
        subscribe(&mut r,"ticker",2);
        subscribe(&mut r,"trades",3);
        let now = Instant::now();
        assert!(r.route(json!([1,[[123,9000,1,70000]]]),now,10000).is_none());
        r.route(json!([1,[[70000,1,1],[70001,1,-1]]]),now,10000).unwrap();
        r.route(json!([2,[1,1,1,1,1,1,70000,1,1,1]]),now,10000).unwrap();
        r.route(json!([3,[]]),now,10000);
        assert!(r.ready(now));
        assert!(!r.ready(now + Duration::from_secs(31)));
    }
    #[test]
    fn malformed_book_delta_invalidates_until_complete_snapshot() {
        let mut r = MarketEvents::default();
        subscribe(&mut r, "book", 1);
        let now = Instant::now();
        r.route(json!([1, [[70000,1,1],[70001,1,-1]]]), now, 10000).unwrap();
        assert!(r.book_snapshot);
        assert!(r.route(json!([1, [70000,0,2]]), now, 10000).is_none());
        assert!(!r.book_snapshot); // deletion amounts must be +1 or -1
        assert!(r.route(json!([1, [70000,1,3]]), now, 10000).is_none());
        assert!(r.route(json!([1, "hb"]), now, 10000).is_none());
        assert!(!r.book_snapshot);
        r.route(json!([1, []]), now, 10000).unwrap();
        assert!(r.book_snapshot);
        assert!(r.route(json!([1]), now, 10000).is_none());
        assert!(!r.book_snapshot);
    }

    #[test]
    fn malformed_trade_snapshot_is_atomic_and_disables_admission() {
        let mut r = MarketEvents::default();
        subscribe(&mut r, "trades", 1);
        let now = Instant::now();
        r.route(json!([1, []]), now, 10000);
        r.route(json!([1, [[1,10000,1,70000],[2,i64::MAX,1,70000]]]), now, 10000);
        assert!(!r.trades_snapshot);
        assert!(r.seen.is_empty());
        r.route(json!([1, [[1,10000,1,70000],[2,15001,1,70000]]]), now, 10000);
        assert!(!r.trades_snapshot);
        assert!(r.seen.is_empty());
        assert!(r.route(json!([1,"te",[3,10000,1,70000]]), now,10000).is_none());
    }

    #[test]
    fn extreme_times_and_out_of_order_events_never_reach_chrono_expect() {
        let mut r = MarketEvents::default();
        subscribe(&mut r, "trades", 1);
        let now = Instant::now();
        r.route(json!([1, []]), now, 40000);
        for (ms, received) in [(i64::MAX,i64::MAX), (i64::MAX,40000), (1,i64::MAX),
            (40000,i64::MIN), (45001,40000), (9999,40000)] {
            assert!(r.route(json!([1,"te",[1,ms,1,70000]]),now,received).is_none());
        }
        r.route(json!([1,"te",[10,40000,1,70000]]), now,40000).unwrap();
        assert!(r.route(json!([1,"te",[9,40000,1,70000]]),now,40000).is_none());
        assert!(r.route(json!([1,"te",[11,39999,1,70000]]),now,40000).is_none());
    }

    #[test]
    fn bounded_identity_retirement_blocks_evicted_replays() {
        let mut r = MarketEvents::default();
        for id in 1..=MAX_TRADE_IDS as i64 + 1 { r.remember(id, id); }
        assert_eq!(r.ids.len(), MAX_TRADE_IDS);
        assert_eq!(r.seen.len(), MAX_TRADE_IDS);
        assert_eq!(r.retired_through_ms, 1);
        r.reconnect();
        subscribe(&mut r, "trades", 7);
        let now = Instant::now();
        r.route(json!([7,[]]), now, 10000);
        assert!(r.route(json!([7,"tu",[1,1,1,70000]]), now,10000).is_none());
    }

    #[test]
    fn receipt_and_previous_trade_price_survive_only_admitted_events() {
        let mut r = MarketEvents::default();
        subscribe(&mut r, "trades", 1);
        let now = Instant::now();
        r.route(json!([1,[]]),now,10000);
        let first = r.route(json!([1,"te",[1,10000,1,70000]]),now,10010).unwrap();
        assert_eq!(first.received_ms, 10010);
        assert_eq!(first.data[2][1], 10000);
        assert!(r.route(json!([1,"tu",[1,10000,1,70000]]),now,10020).is_none());
        let second = r.route(json!([1,"te",[2,10030,1,70001]]),now,10040).unwrap();
        assert_eq!(second.previous_trade_price, 70000.0);
    }

}
