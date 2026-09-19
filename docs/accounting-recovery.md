# Účetnictví a obnova Pirany

Tato implementace nahrazuje odhady z RAM autoritativní historií plnění účtu
Bitfinex `tBTCUSD`. Nasazení binárky a první synchronizace jsou samostatné kroky;
přítomnost tohoto souboru neznamená, že produkce novou verzi používá.

## Kde jsou data

| Soubor (výchozí cesta) | Význam |
|---|---|
| `/var/lib/pirana/accounting.sqlite3` | Primární plnění, přesné desetinné částky, měny poplatků, ID obchodů a objednávek, transakční postup synchronizace, izolovaný originál legacy historie |
| `accounting.sqlite3-wal`, `accounting.sqlite3-shm` | Pracovní soubory SQLite; za běhu je nemazat ani nekopírovat samotnou databázi jako zálohu |
| `/var/lib/pirana/accounting_snapshot.json` | Odvozený atomicky publikovaný report pro API a dashboard; není primární historie |
| `/var/lib/pirana/positions.json` | Trvalé parametry pozic, čekající vstupní záměry s CID, kandidáti obnovy odebraných pozic |
| `/var/lib/pirana/positions.lock` | Procesový zámek; soubor neodstraňovat, kernel zámek po pádu uvolní |
| `/var/lib/pirana/trade_ledger.jsonl` | Původní diagnostické round-trip odhady; nepovažují se za ověřený čistý zisk |
| `/var/lib/pirana/state_snapshot.json` | Starý risk snapshot; jeho existence nedokazuje aktuální stav otevřených pozic |

Cesty lze změnit `PIRANA_ACCOUNTING_DB`, `PIRANA_ACCOUNTING_SNAPSHOT_PATH`,
`PIRANA_ACCOUNTING_SCRIPT`, `PIRANA_POSITION_SNAPSHOT_PATH`. Reporty musejí mít
stejnou cestu projekce jako obchodní proces. Python 3 musí mít SQLite a časovou
zónu Europe/Prague; používají se pouze moduly standardní knihovny.

## Co čísla znamenají

Výsledky jsou FIFO realizovaný PnL všech obchodů účtu s tímto párem, v USD.
Nejde o celkovou změnu hodnoty peněženky ani výsledek výhradně Pirany.
Vklady, výběry a nerealizované přecenění zde nejsou výnosy. Historie bez
doloženého vstupu nebo s neocenitelným poplatkem má stav `incomplete` a peněžní
výsledky `null`, nikoliv nulu. Papírové a legacy záznamy se nepřičítají.

Desetinné hodnoty se ukládají jako řetězce; výpočty používají Decimal. BTC
poplatky mění množství zásoby, USD poplatky pořizovací cenu nebo výnos prodeje.
`fees_usd` znamená poplatky přiřazené realizovanému výsledku (hrubý minus čistý
PnL), nikoliv všechny poplatky zaplacené v daný den. `closed_count` počítá
prodejní plnění, včetně částečných, nikoliv počet celých uzavřených objednávek.
Denní výsledek používá kalendářní den Europe/Prague. Neúplná historie,
projekce starší 120 sekund a přechod na další den zobrazují NEOVĚŘENO.

## Restart a pád

Plnění a oba kurzory (pokrytá historie i rozpracované stránkování) se potvrzují
jednou SQLite transakcí s WAL a `synchronous=FULL`. Stejná dvojice `(trade_id, order_id)` se stejným obsahem je idempotentní;
rozdílný obsah téže dvojice synchronizaci zastaví. Jeden burzovní match může
mít na účtu nákupní i prodejní plnění s různými order ID; ukládají se obě.
Databáze má schéma 2; starší schéma 1 se převádí jednou atomickou transakcí.
Překryv stránek včetně shodných časových značek nesmí přeskočit obchod.
Burzovní dotazy sdílejí nonce i limiter s obchodováním.

Vstupní záměr se zapíše a synchronizuje na disk před odesláním BUY. Po restartu
se záměry párují přes CID, pozice přes ID vstupní objednávky, s agregovanými skutečnými plněními objednávek. Každý výstupní záměr
obsahuje CID a identitu konkrétní strategie; její zbývající množství je skutečný
nákup minus přiřazené prodeje včetně BTC poplatků. FIFO určuje účetní PnL,
nikoliv identitu prodávané strategie. Bez doložených parametrů se nevymýšlí vstupní cena ani stop. Chyba
trvalého zápisu blokuje další odesílání objednávek. Po restartu se nejprve
ověří nepřítomnost otevřených burzovních objednávek, obnoví účetnictví a pozice,
teprve potom běží obchodní smyčka. Nejasný výsledek odeslání nebo nepřiřazená
ruční změna zásoby zastaví další objednávky; nelze slepě opakovat timeout.

Aktuální účet může obsahovat starší nebo ruční zásobu bez strategie a jejích
stopů. Taková zásoba vyžaduje doložené rozlišení od pozic Pirany před spuštěním
nového obchodního procesu; automatický prodej nebo umělá vstupní cena nejsou
migrace. Stejně tak dokončení prvotní vícestránkové synchronizace musí předcházet
úspěšnému obnovení pozic.

## Zachování starých dat a zálohy

Import pouze přidává originální bajty do oddělené tabulky s SHA-256; nevytváří
z nich autentizovaná plnění. Před migračním zápisem uchovejte originály.

```sh
python3 scripts/pirana_accounting.py --db /var/lib/pirana/accounting.sqlite3 import-legacy /var/lib/pirana/trade_ledger.jsonl
python3 scripts/pirana_accounting.py --db /var/lib/pirana/accounting.sqlite3 report
python3 scripts/pirana_accounting.py --db /var/lib/pirana/accounting.sqlite3 backup /bezpecna/cesta/accounting-zaloha.sqlite3
python3 scripts/pirana_accounting.py --db /bezpecna/cesta/accounting-zaloha.sqlite3 report
```

Záloha používá SQLite backup API, kontroluje integritu, synchronizuje soubor
i adresář a odmítá přepsat existující cíl. Parametry pozic zálohujte také;
pro konzistentní obnovu strategie musí být obchodní proces zastaven a záloha
pozic spárovaná s odpovídající či novější historií plnění. Obnovu nejprve
ověřte v odděleném adresáři. Obchodní proces nesmí běžet při výměně databáze.

Testy SIGKILL ověřují pád procesu. Nenahrazují fyzický test výpadku napájení,
záruku řadiče disku ani zálohu mimo tento disk. Pravidelná externí záloha
vyžaduje konkrétní cíl a samostatné provozní nastavení.

Při neúspěšné obnově má dashboard režim `Halted`. Samostatná synchronizace
plnění a dashboard zůstávají dostupné; proces udržuje watchdog, ale neposílá
nové objednávky. Veřejná projekce neobsahuje rozsáhlá pole `orders` a
`open_lots`; úplné podklady pro obnovu vrací účetní helper z databáze.


## Nové reportovací období

`active_period` je oddělený realizovaný výsledek od explicitního času
`start_ms` (včetně). Ukládá se v tabulce `reporting_period` téže SQLite DB.
Jeho založení nesmaže plnění, kurzory ani starý výsledek a neobnoví obchodování.

```sh
python3 scripts/pirana_accounting.py --db /var/lib/pirana/accounting.sqlite3 start-period --start-ms <UTC_milisekundy> --id <identifikator>
```

Příkaz přijme opakování stejného ID a času; jinou hranici odmítne. Budoucí čas
není povolen. SQLite backup zahrnuje i toto nastavení.

Na začátku má nové období realizovaný PnL 0 USD, pokud synchronizace již
pokrývá jeho začátek a je čerstvá. Nové prodeje se počítají podle skutečného
FIFO a poplatků. Nevyřešená historie znamená, že po novém prodeji bude výsledek
konzervativně neověřený; hranice období není pořizovací cena zásoby. Nula se
nikdy nedosazuje za chybějící náklady. Výsledek starší historie zůstává zvlášť.

API zveřejňuje `accounting.active_period` (samostatně též `/api/accounting`),
snapshot navíc `period_pnl` a `period_start_ms`. `total_pnl` nadále znamená
historii, nikoli nové období. Exporter má samostatné metriky
`pirana_period_net_pnl_usd` a `pirana_period_complete`.


## New trading with an unknown historical opening balance

A reporting period resets the displayed period result; it does not establish a
cost basis for previously held BTC. An optional immutable `trading_epoch` in the
same SQLite database separates subsequent execution legs from the old inventory.
The opening BTC balance is quarantined, never inserted as a zero-cost FIFO lot or
recovered as a strategy position. Sell submission checks retain that reserve.
Historical PnL remains unknown where original purchase evidence is missing.

Establish an epoch only while the bot is stopped and automatic starts are inhibited.
Use the read-only `accounting_migrate` example to capture authenticated exchange
wallets before and after a complete history sync and verify there are no active
orders. It must run in the production working directory with the service's actual
credentials. The exact sync cursor is the epoch boundary; both wallet observations
must agree. Back up the database with SQLite's backup API first. Then run:

```text
python3 scripts/pirana_accounting.py --db /var/lib/pirana/accounting.sqlite3 start-trading-epoch --id <unique-id> --start-ms <exact-sync-cursor> --reserved-btc <authenticated-opening-BTC>
```

The command rejects stale/partial sync, an occupied boundary, and conflicting
parameters. Repeating the same established epoch is idempotent. Do not delete or
reset the epoch to bypass recovery failures. New orders require fresh complete
operational accounting and durable strategy metadata. Restart recovery verifies
`exchange BTC = opening reserve + operational inventory`; an unexplained change
halts trading. Wallet observations that overlap an in-flight execution are retried,
so a bot's own fill is not classified as an external balance change.

The reporting period keeps its original boundary. Its totals may use operational
accounting only when no fills occur between that boundary and the trading epoch;
a subsequently discovered fill in that gap invalidates the shortcut. SQLite history,
position intents and metadata must be retained through rollback. Never restore an
older database over newly captured executions.

The doctor classifies an explicit `Halted` mode before testing price/feed health.
It reports the reconciliation requirement instead of restarting the bot for the
zero price associated with an intentionally stopped feed. `Active` with zero price
continues to follow the existing feed-recovery behavior.


Trading in this operational mode additionally requires authenticated zero spot
maker/taker fees. The current published policy is zero trading fees
(https://support.bitfinex.com/hc/en-us/articles/213919589-What-fees-does-Bitfinex-charge).
The authenticated summary endpoint documents account rates
(https://docs.bitfinex.com/reference/rest-auth-summary). The bot validates the three
maker rates and crypto-to-fiat taker rate at startup and every 30 seconds, outside
the order path. Every submission rejects absent, future, or older-than-60-second
confirmation. Nonzero or malformed rates revoke permission. This is a supported
fee-policy constraint, not an invented maximum fee: the exchange must honor its
reported rates. Nonzero-fee trading is unsupported until a fee-inclusive inventory
reservation design is implemented; historical actual fees remain fully accounted.

## Obnova potvrzení objednávek za běhu

ACK `ACTIVE` s nulovým dosud vyplněným množstvím není konečný výsledek.
Exekutor ověří konečný stav objednávky a kompletní součet jednotlivých plnění;
částečně naplněná zrušená IOC objednávka zachová skutečné plnění. Identita CID,
ID objednávky, směr, původní množství a přesný součet se musejí shodovat.
Při nejistotě se další objednávky zablokují a obnova se opakuje za běhu.
Odblokování vyžaduje kompletní účetnictví, shodu peněženky, žádné aktivní
objednávky a atomický zápis obnovených pozic. Dvousekundové zpoždění ingestu
se neobchází; obnova počká na další synchronizaci.

`positions.json` navíc uchovává přiřazení výstupů k pozicím, skutečně požadované
množství výstupu, čas a množinu dokončených CID. Dokončené výstupy se nesmějí
vymazat: brání opětovnému vytvoření již prodaného inventáře při restartu.
Dashboard i Telegram uvádějí `execution_block_reason` a při blokaci stav Halted.
Zbytek pod minimem burzy zůstává evidovaný; neposílá se neplatná objednávka.
Neidentifikovaný inventář se neprodává přes uměle vytvořenou pořizovací cenu.

Před nasazením zálohovat konzistentní stav při zastavené službě. Starší binárka
nemusí přijmout nová metadata positions.json. Po nových plněních nevracet starou
účetní databázi ani starý stav pozic; tím by se ztratila historie. Případnou
obnovu starší verze provést až po nezávislé rekonciliaci aktuálních plnění.
