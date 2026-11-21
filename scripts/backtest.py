#!/usr/bin/env python3
"""Offline backtester for Pump.fun bonding-curve strategies.

Input: CSV files written by `trade_recorder.rs` (logs/trades/*.csv)
  C,recv_ms,slot,mint,creator,0,0,0,0,0
  T,recv_ms,slot,mint,user,is_buy,sol_lamports,token_amount,vsol,vtok

Simulation is deliberately conservative:
  * entry fill = curve state at decision_time + ENTRY_LAT_MS (real latency),
  * exit  fill = curve state at trigger_time  + EXIT_LAT_MS (gap risk is real),
  * exact x*y=k math with our own price impact, FEE each side + priority fees,
  * exits evaluated on every on-chain trade (continuous), not 1s ticks.

Usage:
  python3 backtest.py load   <csv_dir>                   # summary of data
  python3 backtest.py search <csv_dir> [n_samples] [seed] # random search, train/test split
  python3 backtest.py eval   <csv_dir> '<json params>'    # evaluate one param set
"""
import glob
import json
import math
import os
import random
import statistics
import sys
from collections import defaultdict

LAMPORTS = 1_000_000_000
INIT_VSOL = 30 * LAMPORTS
K0 = 30_000_000_000 * 1_073_000_000_000_000
MAYHEM_BOT = "BwWK17cbHxwWBKZkUYvzxLcNQ1YVyaFezduWbtm2de6s"
FEE = 0.0125          # conservative: pump.fun protocol+creator fee per side
PRIO_FEE_SOL = 0.0002 # per side (priority + jito tip), paper approximation
SIZE_SOL = 0.5
ENTRY_LAT_MS = 800
EXIT_LAT_MS = 800


class Token:
    __slots__ = ("mint", "born", "creator", "t", "buy", "sol", "user", "vsol", "vtok")

    def __init__(self, mint, born, creator):
        self.mint, self.born, self.creator = mint, born, creator
        self.t, self.buy, self.sol, self.user, self.vsol, self.vtok = [], [], [], [], [], []


def load(csv_dir):
    toks = {}
    files = sorted(glob.glob(os.path.join(csv_dir, "*.csv")))
    for fn in files:
        with open(fn, "r", encoding="utf-8", errors="ignore") as f:
            for line in f:
                p = line.rstrip("\n").split(",")
                if len(p) != 10:
                    continue
                try:
                    if p[0] == "C":
                        if p[3] not in toks:
                            toks[p[3]] = Token(p[3], int(p[1]), p[4])
                    elif p[0] == "T":
                        tk = toks.get(p[3])
                        if tk is None:
                            continue
                        tk.t.append(int(p[1]))
                        tk.buy.append(p[5] == "1")
                        tk.sol.append(int(p[6]))
                        tk.user.append(p[4])
                        tk.vsol.append(int(p[8]))
                        tk.vtok.append(int(p[9]))
                except ValueError:
                    continue
    out = []
    for tk in toks.values():
        if not tk.t:
            continue
        # strictly standard 30-SOL curve (k = 30e9 * 1.073e15 invariant, no Mayhem virtual-reserve mutation)
        pre = tk.vsol[0] - (tk.sol[0] if tk.buy[0] else -tk.sol[0])
        if abs(pre - INIT_VSOL) > 0.05 * LAMPORTS:
            continue
        if any(u == MAYHEM_BOT or abs(vs * vt - K0) / K0 > 0.005 for u, vs, vt in zip(tk.user, tk.vsol, tk.vtok)):
            continue
        out.append(tk)
    out.sort(key=lambda x: x.born)
    return out, files


def buy_tokens(vsol, vtok, sol_lamports):
    net = sol_lamports * (1 - FEE)
    k = vsol * vtok
    return vtok - k / (vsol + net)


def sell_sol(vsol, vtok, tokens):
    k = vsol * vtok
    out = vsol - k / (vtok + tokens)
    return out * (1 - FEE)


def state_at(tk, idx_from, t):
    """Index of last trade with time <= t, starting search at idx_from."""
    i = idx_from
    n = len(tk.t)
    while i + 1 < n and tk.t[i + 1] <= t:
        i += 1
    return i


def simulate(tk, P):
    """Return pnl_sol or None if no entry."""
    n = len(tk.t)
    born = tk.born
    buyers = set()
    n_buys = 0
    n_sells = 0
    creator_sold = False
    peak_real = 0.0
    win = []  # (t, signed sol) for inflow window
    entry_i = None
    for i in range(n):
        t = tk.t[i]
        age = (t - born) / 1000.0
        if age > P["max_age"]:
            break
        signed = tk.sol[i] if tk.buy[i] else -tk.sol[i]
        if tk.buy[i]:
            n_buys += 1
            buyers.add(tk.user[i])
        else:
            n_sells += 1
            if tk.user[i] == tk.creator:
                creator_sold = True
        win.append((t, signed))
        while win and win[0][0] < t - P["inflow_win"] * 1000:
            win.pop(0)
        real = (tk.vsol[i] - INIT_VSOL) / LAMPORTS
        peak_real = max(peak_real, real)
        if age < P["min_age"]:
            continue
        if P["no_creator_sell"] and creator_sold:
            continue
        if not (P["min_sol"] <= real <= P["max_sol"]):
            continue
        if len(buyers) < P["min_buyers"]:
            continue
        inflow = sum(s for _, s in win) / LAMPORTS
        if inflow < P["min_inflow"]:
            continue
        if peak_real > 0 and (peak_real - real) / peak_real > P["max_dd"]:
            continue
        if n_buys > 0 and n_sells / n_buys > P["max_sell_ratio"]:
            continue
        entry_i = i
        break
    if entry_i is None:
        return None

    t_dec = tk.t[entry_i]
    ei = state_at(tk, entry_i, t_dec + ENTRY_LAT_MS)
    vs, vt = tk.vsol[ei], tk.vtok[ei]
    size = SIZE_SOL * LAMPORTS
    tokens = buy_tokens(vs, vt, size)
    if tokens <= 0:
        return None
    entry_px = size / tokens  # effective lamports per token incl fee/impact
    remaining = 1.0
    realized = 0.0
    high = 0.0
    tp1_done = False
    t_entry = tk.t[ei]

    def value_ratio(j):
        # mark-to-market of 1 token-unit fraction via full-sell math of remaining tokens
        v = sell_sol(tk.vsol[j], tk.vtok[j], tokens * remaining)
        return v / (size * remaining) if remaining > 0 else 0.0

    j = ei
    exit_reason = "end"
    while True:
        j += 1
        if j >= n:
            j = n - 1
            exit_reason = "end"
            break
        t = tk.t[j]
        r = value_ratio(j)  # >1 profit
        high = max(high, r)
        held = (t - t_entry) / 1000.0
        if not tp1_done and P["tp1"] > 0 and r >= 1 + P["tp1"]:
            # fill tranche after latency
            fj = state_at(tk, j, t + EXIT_LAT_MS)
            part = tokens * remaining * P["tp1_pct"]
            realized += sell_sol(tk.vsol[fj], tk.vtok[fj], part)
            remaining *= (1 - P["tp1_pct"])
            tp1_done = True
            j = fj
            if remaining <= 1e-9:
                exit_reason = "tp1"
                break
            continue
        if r <= 1 - P["sl"]:
            exit_reason = "sl"
            break
        if high >= 1 + P["trail_act"] and r <= high - P["trail_dist"]:
            exit_reason = "trail"
            break
        if held >= P["max_hold"]:
            exit_reason = "time"
            break
        if held >= P["stall_secs"] and high < 1 + P["stall_min"]:
            exit_reason = "stall"
            break
    fj = state_at(tk, j, tk.t[j] + EXIT_LAT_MS)
    if remaining > 0:
        realized += sell_sol(tk.vsol[fj], tk.vtok[fj], tokens * remaining)
    pnl = realized / LAMPORTS - SIZE_SOL - 2 * PRIO_FEE_SOL
    return pnl, exit_reason


SPACE = {
    "min_age": [0, 2, 5, 10, 20, 40],
    "max_age": [30, 60, 120, 300],
    "min_sol": [0.5, 2, 4, 6, 8, 12, 16, 25],
    "span_sol": [3, 6, 10, 20, 40],
    "min_buyers": [1, 3, 5, 8, 12, 20],
    "inflow_win": [3, 5, 10, 20],
    "min_inflow": [-100, 0, 0.5, 1, 2, 4],
    "max_dd": [0.05, 0.1, 0.2, 0.35, 1.0],
    "max_sell_ratio": [0.2, 0.4, 0.7, 100],
    "no_creator_sell": [True, False],
    "tp1": [0.08, 0.12, 0.2, 0.35, 0.6, 1.0],
    "tp1_pct": [0.5, 0.65, 0.8, 1.0],
    "sl": [0.05, 0.08, 0.12, 0.2, 0.3],
    "trail_act": [0.04, 0.08, 0.15, 0.3],
    "trail_dist": [0.03, 0.06, 0.1, 0.2],
    "max_hold": [15, 30, 60, 120, 300],
    "stall_secs": [4, 8, 15, 30, 1000],
    "stall_min": [0.0, 0.03, 0.06],
}


def sample(rng):
    P = {k: rng.choice(v) for k, v in SPACE.items()}
    P["max_sol"] = P["min_sol"] + P.pop("span_sol")
    if P["max_age"] <= P["min_age"]:
        P["max_age"] = P["min_age"] + 30
    return P


def evaluate(toks, P):
    pnls, reasons = [], defaultdict(int)
    for tk in toks:
        r = simulate(tk, P)
        if r is None:
            continue
        pnls.append(r[0])
        reasons[r[1]] += 1
    if not pnls:
        return {"n": 0, "pnl": 0.0, "wr": 0.0, "mean": 0.0, "med": 0.0, "worst": 0.0, "reasons": {}}
    return {
        "n": len(pnls),
        "pnl": round(sum(pnls), 4),
        "wr": round(sum(p > 0 for p in pnls) / len(pnls), 3),
        "mean": round(sum(pnls) / len(pnls), 5),
        "med": round(statistics.median(pnls), 5),
        "worst": round(min(pnls), 4),
        "reasons": dict(reasons),
    }


def split(toks, parts=3):
    n = len(toks)
    return [toks[i * n // parts:(i + 1) * n // parts] for i in range(parts)]


def main():
    cmd, d = sys.argv[1], sys.argv[2]
    toks, files = load(d)
    span_h = (toks[-1].born - toks[0].born) / 3.6e6 if toks else 0
    print(f"files={len(files)} tokens_with_trades={len(toks)} span_h={span_h:.1f} "
          f"trades={sum(len(t.t) for t in toks)}", flush=True)
    if cmd == "load":
        reach = defaultdict(int)
        for tk in toks:
            mx = (max(tk.vsol) - INIT_VSOL) / LAMPORTS
            for th in (1, 5, 10, 20, 40, 60, 85):
                if mx >= th:
                    reach[th] += 1
        print("tokens reaching real SOL >=:", dict(reach))
        return
    if cmd == "eval":
        P = json.loads(sys.argv[3])
        A, B, C = split(toks)
        for name, part in (("A", A), ("B", B), ("C", C), ("ALL", toks)):
            print(name, evaluate(part, P))
        return
    if cmd == "search":
        nsamp = int(sys.argv[3]) if len(sys.argv) > 3 else 400
        rng = random.Random(int(sys.argv[4]) if len(sys.argv) > 4 else 1)
        A, B, C = split(toks)
        # prefilter: only tokens that ever reach 0.5 SOL real
        res = []
        for k in range(nsamp):
            P = sample(rng)
            ra = evaluate(A, P)
            if ra["n"] < 15 or ra["pnl"] <= 0:
                continue
            rb = evaluate(B, P)
            if rb["n"] < 15 or rb["pnl"] <= 0:
                continue
            rc = evaluate(C, P)  # held-out, never used for selection
            res.append((min(ra["mean"], rb["mean"]), P, ra, rb, rc))
            print(f"[{k}] A={ra['pnl']}/{ra['n']} B={rb['pnl']}/{rb['n']} C(test)={rc['pnl']}/{rc['n']} wr={rc['wr']}",
                  flush=True)
        res.sort(key=lambda x: -x[0])
        print("\n=== TOP (selected on A+B, C is out-of-sample) ===")
        for sc, P, ra, rb, rc in res[:15]:
            print(json.dumps(P))
            print("   A", ra)
            print("   B", rb)
            print("   C", rc)
        return


if __name__ == "__main__":
    main()
