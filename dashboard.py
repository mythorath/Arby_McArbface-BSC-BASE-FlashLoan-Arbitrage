#!/usr/bin/env python3
"""
Live arbitrage dashboard.  Run on a dedicated terminal / SSH session:

    cd ~/Humming/arbv2 && python3 dashboard.py

Press Ctrl-C to quit.
"""

import json, os, sys, time, re
from collections import deque
from datetime import datetime, timezone

STATUS_DIR = "status"
LOG_DIR    = "logs"
CHAINS     = ["bsc", "base"]
REFRESH_S  = 2
MAX_EVENTS = 24          # recent event rows to show
NATIVE_PRICES = {"BSC": 600.0, "Base": 2500.0}
NATIVE_SYMBOL = {"BSC": "BNB", "Base": "ETH"}

# ANSI helpers
RESET  = "\033[0m"
BOLD   = "\033[1m"
DIM    = "\033[2m"
GREEN  = "\033[32m"
RED    = "\033[31m"
YELLOW = "\033[33m"
CYAN   = "\033[36m"
WHITE  = "\033[37m"
BG_RED = "\033[41m"
CLEAR  = "\033[2J\033[H"

EVENT_RE = re.compile(
    r"(\d{4}-\d{2}-\d{2}T[\d:.]+Z)\s+\w+\s+\S+\s+\S+\s+(.*)"
)

INTERESTING = re.compile(
    r"Optimized path|Submitted|landed|Backrun|circuit-breaker|minute summary"
)


def load_status(chain: str) -> dict | None:
    path = os.path.join(STATUS_DIR, f"{chain}.json")
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def tail_events(chain: str, n: int = 50) -> list[str]:
    path = os.path.join(LOG_DIR, f"{chain}.log")
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            read_bytes = min(size, 32_000)
            f.seek(size - read_bytes)
            raw = f.read().decode("utf-8", errors="replace")
    except FileNotFoundError:
        return []
    lines = raw.splitlines()
    return [l for l in lines if INTERESTING.search(l)][-n:]


def fmt_usd(val) -> str:
    if isinstance(val, str):
        val = float(val)
    if val >= 1.0:
        return f"${val:,.2f}"
    if val >= 0.01:
        return f"${val:.4f}"
    return f"${val:.6f}"


def fmt_dur(seconds: int) -> str:
    h, m = divmod(seconds, 3600)
    m, s = divmod(m, 60)
    if h:
        return f"{h}h{m:02d}m"
    return f"{m}m{s:02d}s"


def colorize_event(line: str, chain_tag: str) -> str:
    ts_match = re.match(r"(\d{4}-\d{2}-\d{2}T(\d{2}:\d{2}:\d{2})\.\d+Z)", line)
    short_ts = ts_match.group(2) if ts_match else "??:??:??"

    tag = f"{DIM}{chain_tag}{RESET}"

    if "landed" in line and 'status="success"' in line:
        gas_m = re.search(r"gas_used=(\d+)", line)
        gas = gas_m.group(1) if gas_m else "?"
        return f"  {GREEN}{BOLD}++ SUCCESS{RESET}  {tag} {short_ts}  gas={gas}"

    if "landed" in line and 'status="revert"' in line:
        gas_m = re.search(r"gas_used=(\d+)", line)
        gas = gas_m.group(1) if gas_m else "?"
        return f"  {RED}-- REVERT {RESET}  {tag} {short_ts}  gas={gas}"

    if "Submitted" in line:
        venue_m = re.search(r'venue="([^"]+)"', line)
        hash_m = re.search(r'hash=Some\("(0x[a-f0-9]{8})', line)
        venue = venue_m.group(1) if venue_m else "?"
        tx_short = hash_m.group(1) if hash_m else ""
        return f"  {CYAN}-> SUBMIT {RESET}  {tag} {short_ts}  {venue} {DIM}{tx_short}..{RESET}"

    if "Optimized path" in line:
        pid_m = re.search(r"path_id=(\d+)", line)
        bps_m = re.search(r"profit_bps=(\d+)", line)
        usd_m = re.search(r'effective_usd="([^"]+)"', line)
        pid = pid_m.group(1) if pid_m else "?"
        bps = bps_m.group(1) if bps_m else "?"
        usd = usd_m.group(1) if usd_m else "?"
        return f"  {YELLOW}** FOUND  {RESET}  {tag} {short_ts}  path={pid} {bps}bps ${usd}"

    if "circuit-breaker" in line:
        pid_m = re.search(r"path_id=(\d+)", line)
        pid = pid_m.group(1) if pid_m else "?"
        return f"  {RED}{BOLD}!! BREAK  {RESET}  {tag} {short_ts}  path={pid} suppressed"

    if "Backrun" in line:
        return f"  {GREEN}~~ BACKRUN{RESET}  {tag} {short_ts}  {line.split('Backrun')[1][:40]}"

    if "minute summary" in line:
        return None

    return None


def render_chain_header(d: dict, chain: str) -> list[str]:
    m = d["metrics"]
    name = d["chain"]
    price = NATIVE_PRICES.get(name, 1.0)
    sym = NATIVE_SYMBOL.get(name, "?")
    uptime = fmt_dur(d["uptime_seconds"])

    try:
        bal_wei = int(d["wallet_balance_native"])
    except (ValueError, TypeError):
        bal_wei = 0
    bal = bal_wei / 1e18
    bal_usd = bal * price

    ok = m.get("landed_success", 0)
    rev = m.get("landed_revert", 0)
    drop = m.get("dropped", 0)
    subs = m.get("submitted_total", 0)
    cands = m.get("candidates_total", 0)
    scans = m.get("scans_total", 0)
    supp = m.get("paths_suppressed", 0)
    warp = m.get("warp_spend_usd", "0.00")
    br_c = m.get("backrun_candidates", 0)
    br_s = m.get("backrun_submitted", 0)

    ok_color = GREEN + BOLD if ok > 0 else WHITE
    rev_color = RED if rev > 0 else DIM

    lines = []
    lines.append(f"  {BOLD}{name}{RESET}  blk {d['block']}  up {uptime}  "
                 f"wallet {bal:.4f} {sym} ({fmt_usd(bal_usd)})")
    lines.append(
        f"    scans {scans:,}  candidates {cands:,}  submitted {subs}  "
        f"{ok_color}ok {ok}{RESET}  {rev_color}rev {rev}{RESET}  drop {drop}  "
        f"supp {supp}  warp {fmt_usd(float(warp))}"
    )
    if br_c > 0 or br_s > 0:
        lines.append(f"    backrun: {br_c} candidates, {br_s} submitted")

    top = d.get("top_active_paths", [])
    if top:
        parts = []
        for p in top[:5]:
            pid = p["path_id"]
            s, r, o = p["submits"], p["reverts"], p["successes"]
            c = GREEN if o > 0 else (RED if r > 0 and o == 0 else WHITE)
            parts.append(f"{c}#{pid}{RESET}({s}/{o}/{r})")
        lines.append(f"    hot paths (sub/ok/rev): {' '.join(parts)}")

    return lines


def main():
    events = deque(maxlen=MAX_EVENTS)

    log_positions = {}
    for chain in CHAINS:
        log_path = os.path.join(LOG_DIR, f"{chain}.log")
        try:
            log_positions[chain] = os.path.getsize(log_path)
        except FileNotFoundError:
            log_positions[chain] = 0
        for line in tail_events(chain, MAX_EVENTS // 2):
            tag = chain.upper()[:3]
            colored = colorize_event(line, tag)
            if colored:
                events.append(colored)

    while True:
        # Read new log lines since last position
        for chain in CHAINS:
            log_path = os.path.join(LOG_DIR, f"{chain}.log")
            try:
                size = os.path.getsize(log_path)
            except FileNotFoundError:
                continue
            if size < log_positions[chain]:
                log_positions[chain] = 0
            if size > log_positions[chain]:
                with open(log_path, "rb") as f:
                    f.seek(log_positions[chain])
                    new_data = f.read(size - log_positions[chain])
                log_positions[chain] = size
                for line in new_data.decode("utf-8", errors="replace").splitlines():
                    if INTERESTING.search(line):
                        tag = chain.upper()[:3]
                        colored = colorize_event(line, tag)
                        if colored:
                            events.append(colored)

        out = []
        out.append(CLEAR)
        now = datetime.now(timezone.utc).strftime("%H:%M:%S UTC")
        out.append(f"{BOLD}  ARBITRAGE DASHBOARD{RESET}  {DIM}{now}{RESET}")
        out.append(f"  {'─' * 72}")

        any_data = False
        for chain in CHAINS:
            d = load_status(chain)
            if d:
                any_data = True
                out.extend(render_chain_header(d, chain))
                out.append("")

        if not any_data:
            out.append(f"  {DIM}No status data yet — waiting for runners to start...{RESET}")
            out.append("")

        out.append(f"  {'─' * 72}")
        out.append(f"  {BOLD}LIVE EVENTS{RESET}  {DIM}(optimized / submitted / landed){RESET}")
        out.append("")

        if events:
            for ev in events:
                out.append(ev)
        else:
            out.append(f"  {DIM}No events yet...{RESET}")

        out.append("")
        out.append(f"  {DIM}Refresh {REFRESH_S}s | Ctrl-C to quit{RESET}")

        sys.stdout.write("\n".join(out) + "\n")
        sys.stdout.flush()
        time.sleep(REFRESH_S)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print(f"\n{RESET}Dashboard stopped.")
