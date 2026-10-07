# Unified strategy configuration / Jednotná konfigurace strategie

strategy.toml is the editable source of the migrated operator ceilings, entry capacity and inventory settings. Percent-suffixed fields use percentage points; VPIN uses a fraction. History-driven tuning is removed from production. Configured limits are fixed until a validated restart. History, secrets, balances, orders and host settings remain outside this file.

strategy.toml je editovatelný zdroj migrovaných stropů operátora, počtu obchodů a nastavení inventáře. Pole s příponou pct používají procentní body; VPIN používá podíl. Ladění podle historie je z produkce odstraněno. Nakonfigurované limity jsou pevné do ověřeného restartu. Historie, tajemství, zůstatky, objednávky a nastavení hostu zůstávají mimo soubor.

Every edit is desired policy until an authorized coherent restart loads it. The whole applied policy is retained with PENDING_RESTART instead of a partial hot reload. Invalid files are rejected; old files without required new fields cannot silently fall back. Deploy compatible binary and configuration together only after verification.

Každá úprava je požadovanou politikou do schváleného soudržného restartu. Celá použitá politika zůstává zachována s PENDING_RESTART místo částečného přenačtení. Neplatné soubory se odmítnou; staré soubory bez nových povinných polí nemohou potichu použít výchozí hodnoty. Kompatibilní binární soubor a konfiguraci nasadit společně až po ověření.

Preserved values: ten slots including pending BUYs, maximum 10% entry notional, disabled stop loss, daily 1%, weekly 1.165%, aggregate 60%, single-trade risk 5%, five losses, VPIN 0.30, baseline ceiling 10%, regime inventory 10/20/35%. A 10% entry notional is distinct from the single-trade risk metric. BUY profit trailing is enabled after a rise of25 USD, starts its floor10 USD above entry and only moves upwards. New SELL-decline trailing is not armed. Existing recovered exit state remains intact. Historical calibration cannot change limits or the sizing baseline. Other configured protective gates remain. This candidate is not deployed.

Zachované hodnoty: deset míst včetně čekajících BUY, nejvýše 10 % kapitálu na nákup, vypnutý stop loss, denní 1 %, týdenní 1,165 %, celková expozice 60 %, riziko obchodu 5 %, pět ztrát, VPIN 0,30, strop baseline 10 % a režimový inventář 10/20/35 %. Hodnota nákupu 10 % se liší od metriky rizika obchodu. BUY trailing je aktivní po růstu o25 USD, začíná hranicí10 USD nad vstupem a posouvá se pouze nahoru. Nový SELL trailing při poklesu se neaktivuje. Obnovený existující stav výstupů zůstává zachován. Historická kalibrace nesmí měnit limity ani základ velikosti pozice. Ostatní nakonfigurované ochranné brány zůstávají. Kandidát není nasazen.

## BTC and USD funding / Doplňování BTC a USD

Verified sizing capital includes USD plus BTC valued using a coherent fresh quote; available USD still constrains execution. Deposits are external capital flows, never strategy profit, closed trades or bot entries. Deposited BTC needs separate origin and cost-basis records before any strategy sale. Reserves and pending orders remain accounted.

Ověřený kapitál pro sizing zahrnuje USD a BTC oceněné soudržným čerstvým kurzem; dostupné USD stále omezuje provedení nákupu. Vklady jsou externí toky kapitálu, nikoli zisk strategie, uzavřené obchody či vstupy bota. Vložené BTC potřebuje samostatnou evidenci původu a pořizovací ceny před prodejem strategií. Rezervy a čekající objednávky se nadále započítávají.

Funding integration is NOT IMPLEMENTED. Existing wallet-delta reconciliation may halt on unattributed BTC changes and ignores USD-only funding. The raw-equity guard has no verified flow adjustment, so a deposit can mask existing losses. Do not remove protections or infer deposits from balance deltas.

Integrace vkladů NENÍ IMPLEMENTOVÁNA. Dosavadní synchronizace může zastavit práci při nesmířené změně BTC a ignoruje samostatné USD vklady. Ochrana prosté equity nemá ověřenou úpravu o toky kapitálu, takže vklad může skrýt existující ztráty. Neodstraňovat ochrany a neodhadovat vklady z rozdílů zůstatků.

Required funding follow-up: authoritative unique transfer IDs and cursors, timestamps and BTC/USD valuation at each transfer, durable duplicate-safe processing, flow-neutral unitized performance/drawdown, external holdings attribution and restart/replay/partial-failure tests. Historical records must survive. The live trading key must not be used concurrently.

Nutné pokračování pro vklady: autoritativní jedinečná ID převodů a kurzory, čas a ocenění BTC/USD při každém převodu, trvalé zpracování bez duplicit, výkonnost a pokles nezávislé na vkladech pomocí jednotek kapitálu, původ externích BTC a testy restartu, opakování a částečných selhání. Historické záznamy zachovat. Živý obchodní klíč nepoužívat souběžně.
