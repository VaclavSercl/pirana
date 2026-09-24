# PIRANA — SECURITY REMEDIATION REPORT #2

**Datum a čas:** 2026-09-19 11:48 CEST  
**Hostitel:** `caslav` (10.0.1.197, aarch64, Ubuntu 24.04 raspi)  
**Repozitář:** `VaclavSercl/pirana`  
**Pracovní adresář:** `/home/wwwenda/workspace/pirana`  
**Auditor / SRE:** Čáslav Autonomous SRE & Security Auditor  

---

## 1. PR #2

* **Final HEAD:** `27caee5b5488b1a93670072be0984904be4b210d`
* **CI Run ID:** `35434613131` (Workflow: *Pirana CI*, Job: `105875061580`)
* **CI Result:** `SUCCESS` (`COMPLETED` — unit testy, clippy, pytest i syntax validace prošly bez jediné chyby)
* **Merge Commit:** `30c5578088b7d31bf53be502a3cbce23c75319ad`
* **Main HEAD:** `30c5578088b7d31bf53be502a3cbce23c75319ad` (pracovní strom je čistý, synchronizován fast-forwardem s `origin/main`)

---

## 2. Hermes sandbox

* **NoNewPrivileges:** `yes`
* **RestrictSUIDSGID:** `yes`
* **InaccessiblePaths:** `InaccessiblePaths=-/home/wwwenda/workspace/pirana/.env`
* **Zda Hermes může použít sudo:** **NE**
* **Důkaz:**
  1. V `/etc/systemd/system/pirana-daily-check.service` jsou aktivní direktivy:
     ```ini
     [Service]
     User=wwwenda
     Group=wwwenda
     NoNewPrivileges=true
     RestrictSUIDSGID=true
     InaccessiblePaths=-/home/wwwenda/workspace/pirana/.env
     ```
  2. Linux kernel při nastavení `NoNewPrivileges=yes` a `RestrictSUIDSGID=yes` striktně blokuje získání nových privilegií přes libovolnou setuid/setgid binárku. Jakékoliv volání `sudo` z tohoto procesu selže s chybou `EPERM`.
  3. Skript `scripts/daily_check.sh` byl v PR #2 upraven: byly z něj zcela odstraněny instrukce a příkazy pro restart služby (`sudo systemctl restart pirana.service`). Pokud je služba neaktivní, skript ji pouze diagnostikuje read-only příkazy a reportuje stav `CRITICAL` na Telegram bez pokusu o eskalaci privilegií.

---

## 3. Sudo

* **Všechny relevantní NOPASSWD entries:**
  Příkaz `sudo -l -n` vrací:
  ```text
  User wwwenda may run the following commands on caslav:
      (ALL : ALL) ALL
      (root) NOPASSWD: /usr/bin/systemctl start pirana.service
      (root) NOPASSWD: /usr/bin/systemctl stop pirana.service
      (root) NOPASSWD: /usr/bin/systemctl restart pirana.service
  ```
  *(Poznámka: `(ALL : ALL) ALL` pochází z členství v systémové skupině `sudo` a vyžaduje zadání hesla; bez hesla funguje výhradně správa `pirana.service`).*
* **Stav `gemini-ruler`:**
  Pravidlo `/etc/sudoers.d/gemini-ruler` (`wwwenda ALL=(ALL) NOPASSWD:ALL`) bylo zazálohováno do `/var/backups/pirana/gemini-ruler.backup` a z `/etc/sudoers.d/` trvale smazáno.
* **Výsledek `sudo -n /usr/bin/true`:**
  **SELHALO** (kód 1, `sudo: interactive authentication is required`). Tento výsledek je záměrný a prokazuje, že control-plane ani AI agenti nemají volný přístup k rootu.
* **Výsledek `visudo`:**
  Ověřeno přes `visudo -cf /etc/sudoers` i `visudo -cf /etc/sudoers.d/pirana-ops` s výsledkem `syntax OK`.
* **Zda broad root path stále existuje:**
  - V sudoers konfiguraci: **NE**.
  - **Architektonický nález:** Systémová služba `/etc/systemd/system/pirana-tick-research.service` je spouštěna jako `User=root`, avšak provádí skript `/home/wwwenda/workspace/pirana/scripts/tick_research.py`, který je vlastněn neprivilegovaným uživatelem `wwwenda:wwwenda`. Tento výzkumný sidecar nezasahuje do obchodování, avšak pro dosažení ideálního stavu se doporučuje upravit jeho systemd unit na `User=wwwenda`.

---

## 4. Network

Skutečné bind adresy zjištěné nástrojem `ss -H -ltn`:

| Port | Služba | Bind Adresa | Stav z pohledu auditu |
| :--- | :--- | :--- | :--- |
| **3000** | Grafana dashboard | `*:3000` | ⚠️ **NON-LOOPBACK / WILDCARD** (Blocker) |
| **8080** | Pirana API / Dashboard | `127.0.0.1:8080` | ✅ **LOOPBACK ONLY** |
| **9090** | Prometheus Server | `*:9090` | ⚠️ **NON-LOOPBACK / WILDCARD** (Blocker) |
| **9091** | Pirana accounting exporter | `127.0.0.1:9091` | ✅ **LOOPBACK ONLY** |
| **9100** | Pirana Rust Prometheus metrics | `127.0.0.1:9100` | ✅ **LOOPBACK ONLY** |
| **9102** | Prometheus Node Exporter | `127.0.0.1:9102` | ✅ **LOOPBACK ONLY** |

*Vysvětlení:* Porty 3000 a 9090 naslouchají na wildcard rozhraní `*`. K jejich přepnutí na `127.0.0.1` je nutný zápis do konfigurace `/etc/default/prometheus` a `/etc/grafana/grafana.ini` s následným restartem služeb. Protože uživatelský účet `wwwenda` byl v kroku 3 úspěšně zbaven neomezeného roota, vyžaduje tento krok jednorázové spuštění příkazů operátorem (viz sekce 14).

---

## 5. Operational accounting

* **Historical status:** `incomplete` (správný stav — fail-closed izolace historických vkladů před zářím 2026, u nichž chybí cost basis).
* **Operational status:** `complete`
* **Operational ID:** `operational-20260917`
* **Operational Scope:** `operational:tBTCUSD:excludes_opening_reserve`
* **Operational Start:** `1789677493253` (2026-09-17)
* **Cursor Age:** Čerstvý (`operational.sync.cursor_ms = 1789811183014`, zpoždění vůči burze je v řádu sekund).
* **Reserved BTC:** `0.00051000` BTC (quarantined trezorový zůstatek).
* **Open operational lots:** `0`
* **Wallet reconciliation:**
  $$\text{Exchange BTC (0.00051000)} = \text{Reserved BTC (0.00051000)} + \text{Operational Lots (0.00000000)}$$
  Variance: přesně **`0.00000000`** (shoda na 8 desetinných míst, tolerance $10^{-10}$).
* **Active period:**
  - ID: `operator-20260917`
  - Start: `1789674342769`
  - Status: `complete`
  - Obchody: 72 uzavřených round-tripů (30 výher, 42 proher)
  - Čistý realizovaný PnL: `+0.008202 USD`
  - Issues: `[]` (žádné účetní anomálie v aktivní periodě).

---

## 6. Postdeploy gate

Úplný JSON výstup příkazu `python3 scripts/postdeploy_gate.py --json`:

```json
{
  "status": "BLOCKED",
  "checks": [
    {
      "name": "no_general_passwordless_sudo",
      "ok": true,
      "detail": null
    },
    {
      "name": "loopback_listeners",
      "ok": false,
      "detail": "port 3000 has non-loopback listener(s): ['*']; production monitoring/trading endpoints must be local/private by policy"
    },
    {
      "name": "runtime_snapshot",
      "ok": true,
      "detail": {
        "system_mode": "Active"
      }
    },
    {
      "name": "canonical_accounting_report",
      "ok": true,
      "detail": {
        "db": "/var/lib/pirana/accounting.sqlite3"
      }
    },
    {
      "name": "operational_recovery",
      "ok": true,
      "detail": {
        "operational_id": "operational-20260917",
        "cursor_ms": 1789811183014,
        "reserved_btc": 0.00051,
        "open_lot_count": 0,
        "expected_btc": 0.00051
      }
    }
  ]
}
```

Při spuštění s `--require-flat --json` navíc úspěšně prošla kontrola ploché pozice:
```json
    {
      "name": "flat_execution_state",
      "ok": true,
      "detail": null
    }
```

---

## 7. Execution

* **system_mode:** `Active`
* **execution_block_reason:** `null`
* **exposure:** `0.00 %`
* **pending intents/orders:**
  - `pending_entry`: `null`
  - `pending_exit`: `null`
  - `pending_intent`: `null`
  - `open_orders`: `[]`
* **active position:** `null`

Systém je plně funkční, připojen na WebSocket kanály (Bitfinex, Binance, Coinbase) a aktivně vyhodnocuje orderflow bez blokací.

---

## 8. Risk calibration

* **generation:** `0`
* **persisted state:** `/opt/caslav/risk/risk_state.json` (obsahuje bezpečný výchozí seed: `max_aggregate_exposure=0.9`, `max_single_trade_risk=0.05`, `vpin=0.650`).
* **Poslední calibration log:**
  Při startu služby: `Risk Engine: kalibrace nactena z /opt/caslav/risk/risk_state.json — gen=0`.
  V rekonciliační smyčce je volána periodická rekalibrace každých 15 minut (`recalibrate_and_log`).
* **Zda kalibrace skutečně proběhla po deploymentu:**
  **ANO**, rekalibrační smyčka je pravidelně spouštěna. Důvod, proč `generation` zůstává na hodnotě `0`, byl forenzně objasněn v kódu `crates/pirana-risk-engine/src/trade_ledger.rs`:
  - Algoritmus `build_stats()` vyžaduje kromě počtu round-tripů také minimálně `MIN_COMPLETED_DAYS = 5` dokončených kalendářních dnů pro korektní výpočet denní volatility a střežení Kellyho zlomku.
  - Vzhledem k restartu procesu nemá runtime k dispozici 5 dokončených denních uzávěrek (`have: 0, need: 5`).
  - Risk engine proto vrací `RiskError::InsufficientSample`, loguje tuto skutečnost na úrovni `debug!` a odmítá spekulativní přenastavení parametrů z neúplných dat. Toto chování je korektní a přímo chrání invariant $P(\text{ruin}) \to 0$.

---

## 9. Trailing-stop evidence

* **Stav:** **`UNVERIFIED STRATEGY JUSTIFICATION`**
* **Stručný důkaz:**
  - V commitu `14a736e` a v předchozím auditu bylo tvrzeno, že konfigurace `min_trigger_usd = 6.0` a `be_offset_usd = 2.0` vedla ke 100% ztrátovosti (21 z 21 exitů, celková ztráta -0.020276 USD), protože hodnoty byly menší než tržní spread (8 USD).
  - V perzistovaném obchodním deníku (`trade_ledger.jsonl`) ani v SQLite databázi se nenachází explicitní telemetrický štítek identifikující přesnou sadu těchto 21 obchodů s jejich dílčími parametry (entry cena, tržní bid při triggeru, realizovaný fill a přesný PnL).
  - Tržní mechanika tohoto jevu (trigger menší než spread vede při prodeji market orderem na bid k realizaci pod nákupní cenou) je logicky a matematicky platná, avšak číselné tvrzení nelze z dostupných logů zpětně exaktně doložit.
  - V souladu se zadáním nebyla konfigurace měněna a zůstává bezpečná hodnota `trailing_stop.enabled = false`.

---

## 10. Backup

* **Local backup:** `LOCAL BACKUP ONLY`
  Úplné zálohy konfigurace, databáze a systémových jednotek existují lokálně na serveru v adresářích `/var/backups/pirana/20260919-084544` a `/var/backups/pirana/gemini-ruler.backup`.
* **Off-host status:** `MANUAL ACTION REQUIRED — OFF-HOST BACKUP DESTINATION`
  Na hostiteli není nakonfigurován žádný bezpečný externí cíl (např. off-host Borg, rsync přes SSH nebo šifrovaný S3 bucket). Zálohy jsou závislé na lokálním disku.

---

## 11. Telegram credential rotation

* **Stav:** **`UNVERIFIED — MANUAL ACTION REQUIRED`**
* Token bota byl oddělen ze souboru `.env` a uložen pod právy `0600` do `/etc/pirana/telegram.env`. Zda byl však token skutečně zneplatněn a přegenerován u Telegram BotFather, nelze zevnitř serveru ověřit a vyžaduje potvrzení operátora.

---

## 12. Repo visibility

* **Stav:** **`PUBLIC`**
* Ověřeno voláním GitHub API (`gh repo view VaclavSercl/pirana --json visibility,isPrivate`). Hodnota nebyla autonomně měněna.

---

## 13. Nevyřešené body

1. **[BLOCKER] Wildcard listener na portu 3000 (Grafana) a 9090 (Prometheus):**
   Služby naslouchají na `*:3000` a `*:9090`. K jejich bezpečnému uzamčení na `127.0.0.1` je nutný jednorázový root zásah operátora.
2. **[MANUAL NON-BLOCKING] Privilege alignment pro `pirana-tick-research.service`:**
   Jednotka běží pod `root`, ale spouští skript vlastněný `wwwenda`. Doporučuje se změnit na `User=wwwenda`.
3. **[MANUAL NON-BLOCKING] Off-host zálohování:**
   Zřídit off-host synchronizaci pro adresář `/var/backups/pirana/`.
4. **[MANUAL NON-BLOCKING] Telegram BotFather rotace:**
   Operátorem ověřit rotaci Telegram API tokenu.

---

## 14. Verdikt

**`BLOCKED — SECURITY REMEDIATION INCOMPLETE`**

### Odůvodnění verdiktu:
Dle striktního zadání auditu:
> *„Pro první dvě varianty musí být základní: `postdeploy_gate.py` GREEN.*  
> *Broad `NOPASSWD:ALL`, wildcard listener na monitorovacích/Pirana portech, nekompletní operational recovery nebo wallet mismatch jsou **BLOCKER**. Nikdy je neoznačuj jen jako kosmetický manual action.“*

Protože automatizovaný post-deploy gate `scripts/postdeploy_gate.py` hlásí `[FAIL] loopback_listeners` kvůli přítomnosti listenerů `*:3000` (Grafana) a `*:9090` (Prometheus), nelze vydat verdikt GREEN, dokud operátor neprovede jejich svázání s loopbackem.

### Jednoduchý postup odblokování (3 příkazy pro operátora pod rootem):

```bash
# 1. Prometheus listener na 127.0.0.1:9090
sudo sed -i 's/^ARGS=.*/ARGS="--web.listen-address=127.0.0.1:9090"/' /etc/default/prometheus
sudo systemctl restart prometheus

# 2. Grafana listener na 127.0.0.1:3000
sudo sed -i 's/^;http_addr =.*/http_addr = 127.0.0.1/' /etc/grafana/grafana.ini
sudo systemctl restart grafana-server

# 3. Kontrola výsledku (postdeploy gate po tomto kroku vrátí GREEN)
python3 /home/wwwenda/workspace/pirana/scripts/postdeploy_gate.py --require-flat --json
```
