#!/usr/bin/env bash
# Managed by Steam Frame 6 GHz Tool. Do not edit in place.
set -euo pipefail
export PATH=/usr/bin:/bin

reg_is_us() {
  iw reg get | awk '
    /^global$/ { section = "global" }
    /^phy#0([[:space:]]|$)/ { section = "phy0" }
    /^phy#[^0]/ { section = "other" }
    /^[[:space:]]*country US:/ {
      if (section == "global") global = 1
      if (section == "phy0") phy0 = 1
    }
    END { exit !(global && phy0) }
  '
}

attempts=0
window_start=0
last_attempt=0
warned=0
repair() {
  if reg_is_us; then return 0; fi
  now=$(date +%s)
  if (( now - window_start >= 3600 )); then
    attempts=0
    window_start=$now
    warned=0
  fi
  if (( attempts >= 3 )); then
    if (( warned == 0 )); then
      echo 'Region is not US; stopped after three attempts in one hour.' >&2
      warned=1
    fi
    return 0
  fi
  if (( now - last_attempt < 5 )); then return 0; fi
  attempts=$((attempts + 1))
  last_attempt=$now
  echo 'Regulatory state changed; requesting US.'
  if ! iw reg set US; then
    echo 'Could not request US regulatory state.' >&2
    return 0
  fi
  sleep 1
  if ! reg_is_us; then
    echo 'US was requested but global and phy#0 were not both confirmed.' >&2
  fi
}

# A cold-plugged phy can appear after systemd starts this service.
for ((i = 0; i < 30; i++)); do
  if iw phy phy0 info >/dev/null 2>&1; then break; fi
  sleep 1
done
# Open the event stream before the initial check to avoid missing startup changes.
{
  repair
  echo 'Monitoring regulatory changes.'
  # The event text is only a wake-up signal; never parse its unstable format.
  while IFS= read -r _; do repair; done
} < <(stdbuf -oL iw event -T)
