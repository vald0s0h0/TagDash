#!/usr/bin/env python3
"""Seed 200 random closed trades into TradeTally (demo/mock account).

Targets (all configurable below):
  - base risk per trade = $100  -> R-multiples fall out cleanly (TradeTally
    computes r_value = pnl / (|entry-stop| * qty), which we pin to $100).
  - profit factor (gross win $ / gross loss $) = 1.70 (winners are scaled to hit it).
  - dates only in April 2026, US regular session hours.
  - account_identifier = "démo mock"
  - longs & shorts, random setups / symbols / prices.
  - emotion tags: rien (=> no tag), fomo, revenge, ennui, panique, forçage.

Commissions/fees are 0 so net P&L == R*100 exactly and the metrics stay clean.
Run with --dry to print the distribution without posting; --limit N to cap.
"""

import argparse
import json
import random
import sys
import urllib.request
import urllib.error
from datetime import datetime, timedelta, timezone

BASE_URL = "https://trade.fabrelexos.synology.me"
TOKEN    = "tt_live_ky0LOz0RE-7xKZJPTVnOf4kxtOpm5pcz"
ACCOUNT  = "démo mock"

N_TRADES      = 200
BASE_RISK     = 100.0          # $ risked per trade (R unit)
PROFIT_FACTOR = 1.70           # target gross-win / gross-loss
WIN_RATE      = 0.42           # fraction of winners
SEED          = 20260423

SETUPS = ["Micro Pullback", "Backside Parabolic", "Panic Mean Reversion", "Perfect Pullback"]
# "rien" => empty tag list; the others map to a single emotion tag.
TAG_CHOICES = ["rien", "fomo", "revenge", "ennui", "panique", "forçage"]

SYMBOLS = [
    "AAPL", "TSLA", "NVDA", "AMD", "SOFI", "PLTR", "RIVN", "LCID", "NIO", "MARA",
    "RIOT", "CVNA", "GME", "AMC", "BBBY", "MULN", "FFIE", "SNDL", "TLRY", "HOOD",
    "COIN", "AFRM", "UPST", "CHPT", "PLUG", "FCEL", "BLNK", "WKHS", "GOEV", "RIDE",
    "SPCE", "DKNG", "BYND", "PTON", "CLOV", "WISH", "SAVA", "DWAC", "PHUN", "BBIG",
    "ATER", "PROG", "GNUS", "XELA", "INPX", "CEI", "CTRM", "SHIP", "TOPS", "ENVX",
    "IONQ", "RKLB", "ASTR", "SOUN", "BBAI", "AI", "SMCI", "ARM", "DELL", "MSTR",
]

# ── R-multiple distribution ───────────────────────────────────────────────────

def build_r_multiples(rng: random.Random, n: int) -> list[float]:
    """One signed R-multiple per trade; winners scaled so PF == PROFIT_FACTOR."""
    rs: list[float] = []
    for _ in range(n):
        if rng.random() < WIN_RATE:
            # Winner: skewed toward small/medium, occasional runner.
            r = rng.expovariate(1 / 1.4) + 0.2          # mean ~1.6, long tail
            r = min(r, 6.0)
            rs.append(round(r, 3))
        else:
            # Loser: mostly a full stop, some partial / slight slippage past it.
            r = -rng.uniform(0.4, 1.15)
            rs.append(round(r, 3))

    gross_win  = sum(r for r in rs if r > 0)
    gross_loss = -sum(r for r in rs if r < 0)
    if gross_win <= 0 or gross_loss <= 0:
        return rs
    scale = (PROFIT_FACTOR * gross_loss) / gross_win
    return [round(r * scale, 4) if r > 0 else r for r in rs]

# ── April 2026 session timestamps ─────────────────────────────────────────────

def april_business_days(year: int = 2026) -> list[datetime]:
    days = []
    d = datetime(year, 4, 1, tzinfo=timezone.utc)
    while d.month == 4:
        if d.weekday() < 5:  # Mon-Fri
            days.append(d)
        d += timedelta(days=1)
    return days

def random_session_times(rng: random.Random, day: datetime) -> tuple[datetime, datetime]:
    """Entry/exit within 09:30-16:00 ET (EDT = UTC-4 in April) -> 13:30-20:00 UTC."""
    open_utc  = day.replace(hour=13, minute=30, second=0, microsecond=0)
    # Entry somewhere in the first ~6h of the session.
    entry_off = rng.randint(0, 5 * 3600 + 1800)
    entry = open_utc + timedelta(seconds=entry_off)
    # Hold from 30s up to ~90 min, but never past the close.
    hold = rng.randint(30, 90 * 60)
    close_utc = open_utc + timedelta(hours=6, minutes=30)
    exit_ = min(entry + timedelta(seconds=hold), close_utc - timedelta(seconds=5))
    return entry, exit_

# ── Trade builder ─────────────────────────────────────────────────────────────

def build_trade(rng: random.Random, r_mult: float) -> dict:
    symbol = rng.choice(SYMBOLS)
    side   = rng.choice(["long", "short"])
    setup  = rng.choice(SETUPS)
    tag    = rng.choice(TAG_CHOICES)
    tags   = [] if tag == "rien" else [tag]

    entry = round(rng.uniform(1.5, 80.0), 2)
    qty   = rng.randint(40, 1200)
    d     = BASE_RISK / qty                       # per-share stop distance, risk == $100 exactly

    if side == "long":
        stop = entry - d
        exit_ = entry + r_mult * d
        first_action, last_action = "buy", "sell"
    else:
        stop = entry + d
        exit_ = entry - r_mult * d
        first_action, last_action = "sell", "buy"

    stop  = round(stop, 4)
    exit_ = round(exit_, 4)

    day = rng.choice(april_business_days())
    entry_t, exit_t = random_session_times(rng, day)
    iso = lambda t: t.isoformat().replace("+00:00", "Z")

    pnl = round(r_mult * BASE_RISK, 2)
    note = (f"[seed] {setup} · risque {BASE_RISK:.0f}$ · R={r_mult:+.2f} "
            f"(PnL {pnl:+.0f}$) · émotion: {tag}")

    return {
        "symbol":            symbol,
        "side":              side,
        "entryTime":         iso(entry_t),
        "exitTime":          iso(exit_t),
        "entryPrice":        entry,
        "exitPrice":         exit_,
        "quantity":          qty,
        "commission":        0,
        "fees":              0,
        "exitCommission":    0,
        "stopLoss":          stop,
        "takeProfit":        None,
        "notes":             note,
        "setup":             setup,
        "strategy":          setup,
        "broker":            "TagDash",
        "account_identifier": ACCOUNT,
        "instrumentType":    "stock",
        "tags":              tags,
        "executions": [
            {"action": first_action, "price": entry, "quantity": qty,
             "datetime": iso(entry_t), "commission": 0, "fees": 0},
            {"action": last_action, "price": exit_, "quantity": qty,
             "datetime": iso(exit_t), "commission": 0, "fees": 0},
        ],
    }

# ── HTTP ──────────────────────────────────────────────────────────────────────

def post_trade(payload: dict) -> tuple[bool, str]:
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"{BASE_URL}/api/v1/trades", data=data, method="POST",
        headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = resp.read().decode("utf-8", "replace")
            return True, body[:200]
    except urllib.error.HTTPError as e:
        return False, f"HTTP {e.code}: {e.read().decode('utf-8', 'replace')[:300]}"
    except Exception as e:  # noqa: BLE001
        return False, str(e)

# ── Main ────────────────────────────────────────────────────────────────────

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry", action="store_true", help="don't post, just show stats")
    ap.add_argument("--limit", type=int, default=N_TRADES)
    args = ap.parse_args()

    rng = random.Random(SEED)
    n = args.limit
    r_mults = build_r_multiples(rng, n)

    gp = sum(r for r in r_mults if r > 0) * BASE_RISK
    gl = -sum(r for r in r_mults if r < 0) * BASE_RISK
    wins = sum(1 for r in r_mults if r > 0)
    net = sum(r_mults) * BASE_RISK
    print(f"Trades: {n}  | gagnants: {wins} ({wins/n*100:.0f}%)  perdants: {n-wins}")
    print(f"Gross profit: {gp:,.0f}$  | Gross loss: {gl:,.0f}$  | Net: {net:,.0f}$")
    print(f"Profit factor: {gp/gl:.3f}  | risque/trade: {BASE_RISK:.0f}$  "
          f"| R moyen: {sum(r_mults)/n:+.2f}")

    trades = [build_trade(rng, r) for r in r_mults]
    if args.dry:
        print("\n--- exemple de payload ---")
        print(json.dumps(trades[0], indent=2, ensure_ascii=False))
        return 0

    ok = 0
    for i, t in enumerate(trades, 1):
        success, msg = post_trade(t)
        if success:
            ok += 1
        else:
            print(f"  [{i:3}/{n}] ECHEC {t['symbol']}: {msg}")
        if i % 25 == 0:
            print(f"  ... {i}/{n} envoyés ({ok} ok)")
    print(f"\nTerminé: {ok}/{n} trades créés sur TradeTally.")
    return 0 if ok == n else 1

if __name__ == "__main__":
    sys.exit(main())
