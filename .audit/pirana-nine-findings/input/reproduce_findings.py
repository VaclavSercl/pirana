#!/usr/bin/env python3
"""Izolované kontrapříklady pro Piranu, commit dc613f41d1402577b49fd44928c92b1ef546146a.

DŮLEŽITÉ: Toto NENÍ originální Rust engine ani jeho testovací sada.
Jde o malé přepisy vybraných podmínek do Pythonu a deterministické modely
pořadí událostí. Ukazují důsledky čteného kódu; nepotvrzují reálné ztráty
na burze, stav serveru ani kompletní chování aplikace.

Bez sítě, API klíčů, obchodních příkazů a závislostí mimo standardní knihovnu.
Spuštění: python3 reproduce_findings.py
Úspěšná aserce znamená reprodukci PROBLÉMU, nikoli bezpečnost systému.
"""
from __future__ import annotations

import json
import math
import unittest
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

COMMIT = "dc613f41d1402577b49fd44928c92b1ef546146a"
BASE = f"https://github.com/VaclavSercl/pirana/blob/{COMMIT}/"
RESULTS: dict[str, dict[str, Any]] = {}


def result(number: str, source: str, **observed: Any) -> None:
    RESULTS[number] = {"source": BASE + source, "observed": observed}


def as_f64(value: Any) -> float | None:
    return float(value) if type(value) in (int, float) else None


def as_i64(value: Any) -> int | None:
    return value if type(value) is int and -(2**63) <= value < 2**63 else None


@dataclass
class Book:
    """Relevantní pozitivní konečné vstupy OrderBook; nikoli úplná knihovna."""
    bids: dict[int, tuple[float, float, int]] = field(default_factory=dict)
    asks: dict[int, tuple[float, float, int]] = field(default_factory=dict)

    def update(self, side: str, price: float, qty: float, count: int) -> None:
        key = math.floor(price / 0.01 + 0.5)  # Rust round pro kladné fixture ceny
        levels = self.bids if side == "buy" else self.asks
        if qty <= 0 or count == 0:
            levels.pop(key, None)
        else:
            levels[key] = (price, qty, count)

    def snapshot_branch(self, packet: list[Any]) -> None:
        """src/main.rs, book větev: rozhodnutí podle tvaru, bez chanId."""
        data = packet[1]
        if not isinstance(data, list):
            return
        is_snapshot = bool(data) and isinstance(data[0], list)
        if is_snapshot:
            self.bids.clear()
            self.asks.clear()
            for entry in data:
                if isinstance(entry, list) and len(entry) >= 3:
                    price = as_f64(entry[0]) or 0.0
                    count = (as_i64(entry[1]) or 0) & 0xFFFFFFFF
                    amount = as_f64(entry[2]) or 0.0
                    self.update("buy" if amount > 0 else "sell", price, abs(amount), count)

    def best_bid(self) -> float | None:
        return self.bids[max(self.bids)][0] if self.bids else None

    def vwap(self, taker_side: str, quantity: float) -> float | None:
        """Přepis crates/pirana-core/src/order_book.rs::vwap."""
        levels = self.asks if taker_side == "buy" else self.bids
        remaining, total_cost, total_qty = quantity, 0.0, 0.0
        for key in sorted(levels, reverse=taker_side == "sell"):
            price, available, _ = levels[key]
            fill_qty = min(remaining, available)
            total_cost += fill_qty * price
            total_qty += fill_qty
            remaining -= fill_qty
            if remaining <= 0:
                break
        return total_cost / total_qty if total_qty > 0 else None


@dataclass
class Position:
    entry: float
    quantity: float
    tp: float
    sl: float
    is_breakeven: bool = False


def should_close_buy_position(pos: Position, price: float, stop_enabled: bool) -> bool:
    """Přepis stejnojmenné funkce src/main.rs."""
    if price < pos.entry:
        return False
    if price >= pos.tp:
        return True
    if pos.is_breakeven and price <= pos.sl:
        return True
    if stop_enabled and price <= pos.sl:
        return True
    return False


@dataclass
class InputUpdates:
    """Model pořadí volání, nikoli matematika jednotlivých indikátorů."""
    tape: int = 0
    ofi: int = 0
    flow: int = 0
    hawkes: int = 0
    vpin: int = 0
    volume: float = 0.0

    def event(self, event: str, trade: list[Any], elapsed_ms: int, cooldown_ms: int) -> None:
        if event not in ("te", "tu"):
            return
        self.tape += 1
        self.ofi += 1
        if elapsed_ms < cooldown_ms:
            return
        self.flow += 1
        self.hawkes += 1
        self.vpin += 1
        self.volume += abs(float(trade[2]))


@dataclass
class Activity:
    """Sekvenční model skutečného interleavingu během await(get_wallets)."""
    generation: int = 0
    inflight: int = 0

    def begin(self) -> None:
        self.inflight += 1
        self.generation += 1

    def end(self) -> None:
        self.generation += 1
        self.inflight -= 1

    def idle_generation(self) -> int | None:
        return self.generation if self.inflight == 0 else None


class Reproductions(unittest.TestCase):
    def test_01_trade_snapshot_corrupts_book(self) -> None:
        book = Book()
        book.snapshot_branch([1, [[7244.8, 2, 0.5], [7245.0, 1, -0.6]]])
        self.assertEqual(book.best_bid(), 7244.8)
        # Formát a tento řádek dat jsou z oficiální Bitfinex dokumentace trades.
        book.snapshot_branch([17470, [[401597393, 1574694475039, 0.005, 7244.9]]])
        self.assertEqual(book.best_bid(), 401597393.0)
        self.assertFalse(book.asks)
        # Následující správný inkrement chybnou cenovou úroveň nesmaže.
        book.update("buy", 7244.8, 0.8, 2)
        self.assertEqual(book.best_bid(), 401597393.0)
        result("01", "src/main.rs#L1575-L1608", best_bid=book.best_bid(),
               expected_bid_before_trade_snapshot=7244.8, corrupted_level_survives_increment=True)

    def test_02_no_loss_trigger_does_not_protect_market_fill(self) -> None:
        pos = Position(100000.0, 0.01, 100025.0, 99600.0)
        last_trade_price = 100030.0
        # Hypotetický dosažitelný bid při provedení; nejde o naměřený obchod.
        market_fill = 99980.0
        self.assertTrue(should_close_buy_position(pos, last_trade_price, False))
        pnl = (market_fill - pos.entry) * pos.quantity
        self.assertLess(pnl, 0)
        result("02", "src/main.rs#L1380-L1455", trigger_allowed=True,
               submitted_type="EXCHANGE MARKET", hypothetical_fill=market_fill, pnl_usd=pnl)

    def test_03_cooldown_drops_feature_updates(self) -> None:
        updates = InputUpdates()
        for n in range(1, 21):
            updates.event("te", [n, n * 40, -0.01, 100000.0 - n],
                          elapsed_ms=n * 40, cooldown_ms=1000)
        self.assertEqual(updates.tape, 20)
        self.assertEqual(updates.ofi, 20)
        self.assertEqual((updates.flow, updates.hawkes, updates.vpin), (0, 0, 0))
        result("03", "src/main.rs#L1730-L1818", received_sell_events=20,
               ofi_updates=updates.ofi, flow_updates=updates.flow,
               hawkes_updates=updates.hawkes, vpin_updates=updates.vpin)

    def test_04_te_tu_same_id_counted_twice(self) -> None:
        updates = InputUpdates()
        trade = [12345, 1790000000000, 0.005, 100000.0]
        for event in ("te", "tu"):
            updates.event(event, trade, elapsed_ms=10000, cooldown_ms=1000)
        self.assertEqual(updates.flow, 2)
        self.assertAlmostEqual(updates.volume, 0.01)
        result("04", "src/main.rs#L1680-L1818", unique_trade_ids=1,
               flow_updates=updates.flow, counted_volume_btc=updates.volume,
               true_unique_volume_btc=0.005)

    def test_05_periodic_branch_restarts_receive_timeout(self) -> None:
        # Virtuální čas: odpovídá zrušení ostatních větví při každém select!.
        now, notifications, timed_out = 0, 0, False
        while now < 120:
            new_receive_deadline = now + 30
            next_periodic = now + 5
            if next_periodic < new_receive_deadline:
                now = next_periodic
                notifications += 1
                # Příští iterace vytváří ZNOVU timeout(now + 30).
            else:
                timed_out = True
                break
        self.assertFalse(timed_out)
        self.assertEqual(now, 120)
        self.assertEqual(notifications, 24)
        result("05", "src/main.rs#L1000-L1055", virtual_seconds_without_messages=now,
               timeout_fired=timed_out, watchdog_notifications=notifications,
               note="Deterministický model plánování; nikoli spuštění Tokio runtime.")

    def test_06_kappa_reset_before_trade_quote(self) -> None:
        gamma, sigma, dt = 0.10, 10.0, 1.0
        base_kappa, dynamic_kappa = 1.50, 0.30
        model_kappa = dynamic_kappa  # větev order book
        dashboard_kappa = dynamic_kappa
        model_kappa = base_kappa     # následující větev trade / ticker
        half = lambda k: 0.5 * gamma * sigma**2 * dt + math.log1p(gamma / k) / gamma
        self.assertNotEqual(model_kappa, dashboard_kappa)
        self.assertNotAlmostEqual(half(model_kappa), half(dynamic_kappa))
        result("06", "src/main.rs#L1630-L1720", dashboard_kappa=dashboard_kappa,
               quote_kappa=model_kappa, actual_half_spread=half(model_kappa),
               half_spread_with_dynamic_kappa=half(dynamic_kappa))

    def test_07_sale_can_create_unfunded_btc_reserve(self) -> None:
        wallet_btc, entry, fill = 0.01, 100000.0, 100100.0
        quantity = wallet_btc
        wallet_btc -= quantity
        pnl_usd = (fill - entry) * quantity
        locked_btc = (pnl_usd / fill) * 0.10
        lifetime_skimmed = locked_btc
        self.assertEqual(wallet_btc, 0)
        self.assertGreater(locked_btc, wallet_btc)
        # Skutečný reconcile_vault zde přepíše aktivní rezervu na nulu.
        active_after_reconciliation = 0.0 if wallet_btc <= 0.000001 else wallet_btc
        self.assertGreater(lifetime_skimmed, active_after_reconciliation)
        result("07", "src/main.rs#L1430-L1500", wallet_btc_after_sale=wallet_btc,
               booked_locked_btc=locked_btc, realized_profit_usd=pnl_usd,
               active_after_reconciliation=active_after_reconciliation,
               lifetime_skimmed_still_btc=lifetime_skimmed)

    def test_08_market_message_invalidates_wallet_observation(self) -> None:
        activity = Activity()
        discarded = 0
        for _ in range(10):
            observation = activity.idle_generation()
            # Během čekání na REST odpověď přijde pouze heartbeat / market zpráva.
            activity.begin()
            activity.end()
            if activity.idle_generation() != observation:
                discarded += 1
        self.assertEqual(discarded, 10)
        self.assertEqual(activity.inflight, 0)
        result("08", "src/main.rs#L805-L815", actual_executions=0,
               wallet_responses=10, discarded_responses=discarded,
               note="Konkrétní možné pořadí událostí; četnost na serveru nebyla měřena.")

    def test_09_vwap_returns_price_for_incomplete_quantity(self) -> None:
        book = Book()
        book.update("sell", 100000.0, 0.001, 1)
        requested = 0.01
        price = book.vwap("buy", requested)
        self.assertEqual(price, 100000.0)
        self.assertGreater(requested, 0.001)
        result("09", "crates/pirana-core/src/order_book.rs#L104-L137",
               requested_btc=requested, available_btc=0.001, returned_vwap=price,
               uncovered_quantity_btc=requested - 0.001,
               note="Nejde o chybný skutečný fill; chyba je nerozlišené částečné pokrytí v odhadu.")


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Reproductions)
    run = unittest.TextTestRunner(verbosity=2).run(suite)
    payload = {
        "repository": "VaclavSercl/pirana",
        "commit": COMMIT,
        "method": "Python reduced reproductions and deterministic event models; not native Rust tests",
        "network_used": False,
        "native_engine_run": False,
        "tests_run": run.testsRun,
        "reproductions_confirmed": run.wasSuccessful(),
        "findings": RESULTS,
    }
    destination = Path(__file__).with_name("reproduction_results.json")
    destination.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"\nVýsledky: {destination}")
    raise SystemExit(0 if run.wasSuccessful() else 1)
