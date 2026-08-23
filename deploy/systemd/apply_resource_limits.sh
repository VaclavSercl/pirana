#!/usr/bin/env bash
set -euo pipefail

# CASLAV v5.1 — ochrana proti OOM na Pi bez swapu.
# Pirana (obchoduje, peak 31 MB) dostava velkou rezervu a NEJVYSSI ochranu
# pred OOM killerem. Pomocne sluzby se omezuji, aby nemohly vyhladovet system
# a nechat OOM killer sahnout po obchodujicim procesu s otevrenou pozici.

mk() {
  local unit="$1" high="$2" max="$3" oomadj="$4" extra="${5:-}"
  sudo mkdir -p "/etc/systemd/system/${unit}.d"
  sudo tee "/etc/systemd/system/${unit}.d/10-resources.conf" >/dev/null <<EOF
# CASLAV v5.1 :: resource limity (Pi 4, 7,6 GB RAM, SWAP 0 B)
[Service]
MemoryHigh=${high}
MemoryMax=${max}
OOMScoreAdjust=${oomadj}
${extra}
EOF
  echo "  [OK] ${unit}  high=${high} max=${max} oom_adj=${oomadj}"
}

echo "=== NASTAVUJI RESOURCE LIMITY ==="
# Obchodujici proces: peak 31 MB -> 256/384 MB je 8-12x rezerva.
# OOMScoreAdjust=-900 => OOM killer po nem sahne az uplne nakonec.
mk pirana.service                  256M 384M -900 "Restart=always
RestartSec=5"

# Exporter: metriky, mala zatez.
mk pirana-exporter.service         64M  128M  -100

# Telegram bot: ovlada obchodovani, ale neni v horke ceste.
mk pirana-telegram-bot.service     128M 256M  -200

# Oneshot reporty: bezi kratce, mohou byt obetovany prvni.
for u in pirana-daily-check pirana-monthly-report pirana-monthly-proposal pirana-yearly-report; do
  mk "${u}.service" 384M 512M 500 "CPUWeight=50"
done

echo
echo "=== DAEMON-RELOAD ==="
sudo systemctl daemon-reload
echo "RELOAD_EXIT=$?"

echo
echo "=== OVERENI (bez restartu sluzeb) ==="
for u in pirana.service pirana-exporter.service pirana-telegram-bot.service pirana-daily-check.service; do
  h=$(systemctl show -p MemoryHigh --value "$u")
  m=$(systemctl show -p MemoryMax --value "$u")
  o=$(systemctl show -p OOMScoreAdjust --value "$u")
  printf "  %-34s high=%-10s max=%-10s oom_adj=%s\n" "$u" "$h" "$m" "$o"
done
