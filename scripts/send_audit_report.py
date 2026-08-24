#!/usr/bin/env python3
"""
Čáslav :: Odeslání forenzního auditního reportu v5.1 na Telegram.
Rozdělí report na části podle limitu Telegramu (4096 znaků) a odešle je v pořadí.
"""

import os
import sys
import time
import urllib.request
import urllib.parse

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"
MAX_LEN = 3800  # rezerva pod limit 4096


def load_env():
    env = {}
    if os.path.exists(ENV_FILE):
        with open(ENV_FILE, "r") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    k, v = line.split("=", 1)
                    env[k.strip()] = v.strip().strip('"').strip("'")
    return env


PARTS = [
# ── ČÁST 1 ────────────────────────────────────────────────────────────
"""🔍 <b>FORENZNÍ AUDIT — PIRANA / ČÁSLAV v5.1</b>
📅 <code>23. 08. 2026 12:45 CEST</code>
━━━━━━━━━━━━━━━━━━━━━━

<b>1. VERDIKT ÚVODEM</b>

Nasazení je <b>reálné a konzistentní</b>. Git, binárka i běžící proces sedí na sebe. Testy prochází. Dvě ze tří klíčových oprav v5.1 jsou prokazatelně funkční v produkci.

Ale nalezl jsem <b>jeden zásadní rozpor</b> mezi tím, co commit tvrdí, a tím, co systém dělá — a jednu riskantní změnu, která se ještě nestihla projevit.

━━━━━━━━━━━━━━━━━━━━━━
<b>2. INTEGRITA NASAZENÍ</b> ✅

• Lokální HEAD: <code>a320e891fc1a0b6c...</code>
• origin/main: <code>a320e891fc1a0b6c...</code> ✅ <b>shodné</b>
• Pracovní strom: 0 změn ✅ čistý
• Commit rozsah: 7 souborů, +1377 / −11
• Binárka: 12:16:01 ✅ novější než všechny zdrojáky
• <code>pirana.service</code>: active od 12:17:47, <b>NRestarts=0</b> ✅
• <code>pirana-exporter</code>: active ✅
• Paměť: 28,3 MB ✅
• <code>cargo test --workspace</code>: <b>59/59 passed</b> ✅
• <code>cargo build --release</code>: 0 varování ✅
• SHA-256 master promptu: <code>96b717b5...b3d18</code> ✅ sedí s CHANGELOGem

Chronologie bezchybná: zdrojáky (12:15–12:19) → build (12:16) → commit (12:22) → služba běží od 12:17 bez restartu. <b>Žádný drift.</b>""",

# ── ČÁST 2 ────────────────────────────────────────────────────────────
"""<b>3. CO SKUTEČNĚ FUNGUJE</b> ✅
<i>(ověřeno v produkci)</i>

<b>3.1 Win-rate: matematicky opraveno</b>

Kód <code>main.rs:1025-1036</code> je korektní — otevírací fill s PnL==0 se do statistiky nepočítá.

Živá data poprvé v historii souhlasí:
<pre>closed_trades  = 8
winning_trades = 4
win_rate       = 0.5   ✅ přesně 4/8</pre>

Dřív dashboard hlásil 57 % proti 35,7 % na burze. Rozpor je pryč. Zároveň <code>trades_today == closed_trades</code>, takže oprava z Fáze 1 zůstala zachována — <b>obě opravy do sebe zapadly bez konfliktu</b>.

<b>3.2 VPIN guard: aktivní a reálně blokuje</b>

<pre>12:19:27 WARN ⚠️ [VPIN HIGH TOXICITY]
VPIN=65.2% >= 65%
Adverse selection guard active,
skipping standard noise entries</pre>

Aktuální VPIN <b>75,2 %</b> — nad emergency prahem. Logika je promyšlená: blokuje šumové vstupy, ale propustí signál potvrzený lead-lag nebo Hawkes kaskádou.

<b>3.3 Opravy z Fáze 1 &amp; 2 přežily</b>

Fill-price accounting, <code>reanchor_equity()</code>, governance gate i <code>update_order()</code> jsou netknuté. Commit v5.1 na ně navázal, nepřepsal je.""",

# ── ČÁST 3 ────────────────────────────────────────────────────────────
"""🔴 <b>4. KRITICKÝ NÁLEZ</b>
<b><code>self_calibration.rs</code> je mrtvý kód</b>

Commit <code>a320e89</code> prohlašuje:
<i>„Limity nejsou konstanty, ale funkce odvozené z měřených dat… Brána P(ruin) jako FUNKCE EXPOZICE… Rate limit +30 %/cyklus"</i>

<b>Realita:</b> modul o 730 řádcích je v <code>lib.rs</code> deklarován, ale <b>nikde se nevolá</b>:

<pre>grep -rn "recalibrate|SelfCalibration|
TradingStats|DerivedParam" src/ crates/
→ jediné výskyty jsou uvnitř
  self_calibration.rs samotného</pre>

Risk engine dál používá <b>staré tvrdé konstanty</b>:
• <code>MAX_AGGREGATE_EXPOSURE = 0.90</code> → engine.rs:194
• <code>MAX_SINGLE_TRADE_RISK = 0.05</code> → engine.rs:176
• <code>CONSECUTIVE_LOSS_THRESHOLD = 5</code> → engine.rs:133
• <code>MAX_DAILY_DRAWDOWN = 0.03</code> → engine.rs:95

<b>Dopad:</b> Kelly sizing, volatility targeting, adaptivní drawdown, kalibrovaný VPIN práh ani opravená brána P(ruin) <b>nemají v ostrém provozu žádný efekt</b>. Systém běží na stejné statické konfiguraci jako před v5.1.

Je to dobře napsaný a otestovaný modul (18 testů vč. regrese <code>v50_bug_derisking...</code>) — ale zatím jen knihovna na polici. Chybí sběr <code>TradingStats</code> a periodické volání <code>recalibrate()</code>.

<i>Ironie: modul zavádí princip „hodnota bez vzorce je neplatná a runtime ji odmítne". Runtime ji neodmítá, protože tento typ vůbec nezná.</i>""",

# ── ČÁST 4 ────────────────────────────────────────────────────────────
"""🟠 <b>5. VYSOKÉ RIZIKO</b>
<b>20× větší pozice bez aktivní kalibrace</b>

<pre>position_size_pct     1.0 → 20.0  (20×)
min_position_size_pct 1.0 → 5.0   (5×)
max_position_size_pct 15.0 → 25.0 (+67%)</pre>

Živá data: velikost obchodu vyskočila <b>14×</b>, z 0.000052 na <b>0.000726 BTC</b>. Expozice <b>69,98 %</b>.

Odůvodnění v commitu je věcně správné („89 % kapitálu leželo ladem"). <b>Problém je načasování:</b> tento skok měl být krytý právě Kelly sizingem a volatility targetingem ze <code>self_calibration.rs</code>. Ty ale neběží (§4).

Výsledek = <b>nejagresivnější konfigurace v historii systému bez samoregulační smyčky</b>.

Konkrétní čísla:
• Nejhorší dnešní obchod: <b>−0.0378 USD</b>
  (dřív typicky −0.0015 → <b>25× větší</b>)
• Nejlepší: +0.0807 USD
• Denní PnL: <b>+0.1134 USD</b> ✅ zatím pozitivní
• <code>consecutive_losses = 1</code> (práh Defensive = 5)

Pojistky formálně existují, ale jsou to statické konstanty, ne kalibrované hodnoty. Při VPIN 75 % a 70% expozici si to zaslouží dohled.""",

# ── ČÁST 5 ────────────────────────────────────────────────────────────
"""<b>6. DALŠÍ ZJIŠTĚNÍ</b>

# P1 vyreseno 24.8. — stary master_system_prompt.md byl odstranen z disku.

🟡 <b>P2</b> — <code>Cargo.toml.example</code> deklaruje verzi <code>5.0.0</code>, zatímco vše ostatní je v5.1.

🟡 <b>P2</b> — <code>daily_loss_limit_usd = 50.0</code> zůstává v USD, ačkoli doktrína v5.1 nařizuje účtování v satoshi.

🟡 <b>P2</b> — Nálezy z mého auditu stále otevřené: OFI z ticker cen, A-S target inventory, mrtvý <code>PiranaConfig</code>.

ℹ️ <code>Ai-komunikace.json</code> — 329 zpráv, 1,2 MB. Doložený audit trail vzniku v5.1.

━━━━━━━━━━━━━━━━━━━━━━
<b>7. ZÁVĚREČNÉ HODNOCENÍ</b>

• Integrita nasazení — ✅ bezchybná
• Kvalita kódu v5.1 — ✅ výborná (59/59, 0 warn)
• Win-rate oprava — ✅ ověřena v produkci
• VPIN guard — ✅ aktivní
• Samokalibrační smyčka — 🔴 napsaná, nezapojená
• Risk/reward konfigurace — 🟠 agresivní bez pojistky
• Soulad commit ↔ realita — 🔴 commit slibuje víc

<b>Shrnutí jednou větou:</b>
<i>v5.1 je poctivě odvedená práce s jednou nedodělanou spojkou — samokalibrační motor je postavený a otestovaný, ale není připojený k převodovce, přičemž plyn byl mezitím přidán na dvacetinásobek.</i>""",

# ── ČÁST 6 ────────────────────────────────────────────────────────────
"""<b>8. DOPORUČENÉ POŘADÍ NÁPRAVY</b>

<b>1. P0 — Zapojit <code>self_calibration.rs</code></b>
Sběr <code>TradingStats</code> z uzavřených round-tripů + periodické <code>recalibrate()</code> (např. každých 50 obchodů nebo 1×/h), s aplikací <code>RiskState</code> do risk engine místo statických konstant.

<b>2. P0/dočasně — Snížit sizing</b>
Než kalibrace poběží, zvážit <code>position_size_pct</code> z 20 na ~8–10 jako přechodový kompromis. Aktuální VPIN 75 % tomu nahrává.

<b>3. P1 — Vyjasnit master prompt</b>
Odstranit/archivovat <code>master_system_prompt.md</code>, aby bylo jednoznačné, co se načítá.

<b>4. P2 — Dokončit satoshi doktrínu</b>
<code>daily_loss_limit_usd</code> → <code>daily_loss_limit_sats</code>.

<b>5. P2 — Uzavřít zbylé nálezy</b>
OFI trade flow, A-S target inventory, <code>PiranaConfig</code>.

━━━━━━━━━━━━━━━━━━━━━━
👑 <b>ČÁSLAV :: KONEC REPORTU</b>
<i>Audit provedl Hermes Agent — ověřeno proti živému systému, gitu i GitHubu.</i>""",
]


def send(token, chat_id, text, idx, total):
    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = urllib.parse.urlencode({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": "true",
    }).encode("utf-8")
    req = urllib.request.Request(url, data=payload, method="POST")
    with urllib.request.urlopen(req, timeout=20) as resp:
        ok = resp.status == 200
        print(f"[{'OK' if ok else 'FAIL'}] část {idx}/{total} "
              f"({len(text)} znaků) status={resp.status}")
        return ok


def main():
    env = load_env()
    token = env.get("TELEGRAM_BOT_TOKEN")
    chat_id = env.get("TELEGRAM_CHAT_ID", "1076582576")

    if not token:
        print("[ERROR] TELEGRAM_BOT_TOKEN není v .env", file=sys.stderr)
        return 1

    total = len(PARTS)
    sent = 0
    for i, part in enumerate(PARTS, start=1):
        if len(part) > 4096:
            print(f"[WARN] část {i} má {len(part)} znaků — nad limit!",
                  file=sys.stderr)
        try:
            if send(token, chat_id, part, i, total):
                sent += 1
        except Exception as e:
            print(f"[ERROR] část {i} selhala: {e}", file=sys.stderr)
        time.sleep(1.2)  # rate limit Telegramu

    print(f"\nOdesláno {sent}/{total} částí na chat_id={chat_id}")
    return 0 if sent == total else 1


if __name__ == "__main__":
    sys.exit(main())
