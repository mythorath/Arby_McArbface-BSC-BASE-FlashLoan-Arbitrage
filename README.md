# arbv2 — Cross-DEX Flash-Loan Arbitrage Bot (BSC + Base)

A from-scratch Rust rewrite of a multi-protocol DEX arbitrage scanner. Single-binary,
in-process AMM math, mempool backrunning, and tiered MEV bundle submission across
free direct-to-builder venues + a paid Chainstack Warp fallback.

> **Status:** Live, end-to-end pipeline validated on BSC mainnet (force-fired tx
> hash on chain showing full execution path — ~340k gas, contract reverted on
> `InsufficientProfit` because the spread closed before our bundle landed, which
> is the *correct* competitive behavior). On a continuous soak it scans 19 BSC
> pools / 282 closed-loop paths and 28 Base pools / 456 paths every block,
> typically completing a full scan in under 50 ms after state refresh.
>
> **Profitability:** Not yet positive. The system is fast and correct, but on
> these two chains we are competing against actors with co-located builders and
> deeper path coverage. See [the open issues](#known-limitations--open-problems)
> for what would likely be needed to actually start landing winners. **PRs and
> ideas welcome.**

---

## Why this exists

This is the second iteration of a personal flash-loan-arb experiment. The first
was a Python scanner that, over three days of live mainnet scanning, never
captured a single profitable trade — every spread closed before the
`eth_call` round-trip finished. The diagnosis was unambiguous: we were too slow.

The rewrite was a top-to-bottom redesign for latency:

- **Rust + alloy-rs v1** for the hot path (no Python GIL, no per-path RPC).
- **In-process AMM math** — every supported protocol's swap formula is ported
  exactly from the on-chain contract into Rust, so we evaluate hundreds of
  candidate paths per block without a single network round-trip.
- **`StateReader.sol`** — one batched `eth_call` per block fetches every
  pool's state slot in a single round-trip, instead of N sequential calls.
- **Tiered submission** — free direct-to-builder bundles (48Club, BlockRazor,
  JetBldr, NodeReal, Blink) for normal flow, with Chainstack Warp Trader nodes
  reserved for high-EV trades that can absorb the per-tx fee.
- **Liquidity-aware sizing + ternary search** over flash amount, capped at a
  configurable percentage of the smallest reserve in the path so we never
  blow out the spread we're trying to capture.
- **Per-path circuit breaker** — paths that revert N times in a row are
  suppressed for K blocks so we don't hemorrhage builder-rejection fees on the
  same broken combo.
- **Mempool backrunning** — pending V2/V3 swaps are decoded, matched against an
  index of paths that touch the same token pair, and re-evaluated immediately
  for backrun opportunities.

Even with all of the above, on BSC and Base in 2026 the easy money is gone.
This repo is being open-sourced because it's a clean, working reference
implementation that someone with better infra (co-located builder, more chains,
proprietary path discovery) might be able to turn profitable — and because
extra eyes might find bugs we missed.

---

## Architecture

```
                  ┌─────────────────────────────────────────────┐
                  │                arb-runner                    │  per-chain main loop
                  └──┬─────────────┬─────────────┬───────────┬──┘
                     │             │             │           │
              ┌──────▼─────┐ ┌────▼──────┐ ┌────▼─────┐ ┌──▼────────┐
              │  arb-state │ │ arb-paths │ │ arb-sim  │ │ arb-submit │
              │            │ │           │ │          │ │            │
              │ batched    │ │ closed-   │ │ in-proc  │ │ tiered MEV │
              │ refresher  │ │ loop      │ │ AMM math │ │ + presign  │
              │ via        │ │ enumerator│ │ + ternary│ │ + circuit  │
              │ StateReader│ │ (2/3-hop) │ │ search   │ │  breaker   │
              └────────────┘ └───────────┘ └──────────┘ └────────────┘
                     ▲             ▲             ▲           ▲
                     │             │             │           │
              ┌──────┴─────────────┴─────────────┴───────────┴───┐
              │                  arb-rpc / arb-core              │
              │   alloy v1 client w/ nonce cache  +  AMM kernels │
              └────────────────────┬─────────────────────────────┘
                                   │
                          ┌────────▼────────┐
                          │  arb-mempool    │
                          │  WSS pending tx │
                          │  decoder for    │
                          │  backrunning    │
                          └─────────────────┘
```

Eight Rust crates, ~10k LOC of original code:

| crate | purpose |
|---|---|
| `arb-core`     | AMM math kernels — Uniswap V2, V3, V4 (CL), PancakeSwap StableSwap (Curve-style), Wombat, DODO V2, Algebra/Thena Fusion, Aerodrome V2 (volatile + stable, Solidly-style), Aerodrome Slipstream |
| `arb-state`    | In-memory pool state store + batched on-chain refresh through the `StateReader` helper contract |
| `arb-paths`    | Closed-loop 2-hop and 3-hop path enumeration (`USDT → WBNB → USDT`, etc.) |
| `arb-sim`      | Full-path simulation against current state + ternary-search optimizer over flash amount + multi-stage profit gate |
| `arb-submit`   | Bundle building, pre-signed calldata pool, and submission across 5+ MEV venues |
| `arb-mempool`  | Pending-tx WSS subscription + selector-based decoder for V2/V3 swap calldata |
| `arb-rpc`      | alloy v1 wrapper with latency tracking, nonce cache, and split read/submit endpoints |
| `arb-runner`   | Per-chain main binary — config loading, scanner loop, Prometheus metrics, status JSON |

### Smart contracts (`contracts/`)

A standard Foundry project. Three contracts:

- **`BscFlashArb.sol`** — production BSC contract. Borrows via Uniswap V4
  PoolManager and routes a single closed-loop swap across any combination of
  the 7 supported BSC protocols. All paths execute atomically in `unlockCallback`.
  Owner-only with token allowlist + reentrancy guards.
- **`BaseFlashArb.sol`** — Base port. Adds Aerodrome V2 + Slipstream support.
- **`StateReader.sol`** — pure `view` helper. Takes a list of pool addresses
  partitioned by protocol type and returns all of their state in a single
  `eth_call` response. Hardened against fee-tier ABI variations (PancakeSwap V3
  uses `uint32` for `feeProtocol`, Uniswap V3 uses `uint8`, etc.) by using
  low-level `staticcall + abi.decode`.

### Operations

- **`dashboard.py`** — terminal dashboard that tails `status/*.json` and the
  log files for a live readout of scan rate, candidates, submits, lands,
  suppressed paths, hot paths, and color-coded events. Pure Python stdlib.
- **`ops/systemd/`** — sample systemd unit files for running both scanners as
  long-lived services with auto-restart.
- **`ops/prometheus/`** — sample scrape config. The runner exposes metrics on
  `:9100` (BSC) / `:9101` (Base): scan latency histograms, candidates/min,
  submits, lands, reverts, suppressed paths, builder-sim rejects, Warp spend,
  backrun candidates, etc.

---

## Quick start

### Requirements

- Rust 1.84+ (`curl https://sh.rustup.rs -sSf | sh`)
- Foundry (`curl -L https://foundry.paradigm.xyz | bash`)
- A BSC + Base RPC + WSS endpoint (Chainstack Growth tier is what this was
  developed against, but anything with reasonable rate limits works)
- A small amount of native gas token in the operating wallet on each chain
  (~$10 of BNB, ~$10 of ETH on Base — flash loans cover all trading capital)

### Setup

```bash
git clone https://github.com/YOUR_USERNAME/arbv2.git
cd arbv2

# 1. Configure
cp .env.example .env
$EDITOR .env   # fill in PRIVATE_KEY, RPC URLs, etc.

# 2. Build the Rust binary
cargo build --release

# 3. Build & deploy contracts (or use the addresses already in .env.example)
cd contracts
forge install
forge build
forge script script/DeployBsc.s.sol --rpc-url $BSC_RPC_URL --broadcast
forge script script/DeployBase.s.sol --rpc-url $BASE_RPC_URL --broadcast
forge script script/DeployBscStateReader.s.sol --rpc-url $BSC_RPC_URL --broadcast
forge script script/DeployBaseStateReader.s.sol --rpc-url $BASE_RPC_URL --broadcast
cd ..

# 4. Run the scanners
./target/release/arb-runner config/bsc.toml
./target/release/arb-runner config/base.toml

# 5. Watch the dashboard in another tmux pane
python3 dashboard.py
```

### Force-fire smoke test

To validate the full pipeline (scan → optimize → build → sign → submit → land →
receipt → revert reason), the runner has a `--force-fire` mode that ignores all
profit gates and submits the next candidate it finds, no matter how small:

```bash
./target/release/arb-runner config/bsc.toml --force-fire
```

This will spend a few cents of gas on a deliberately-losing trade so you can
confirm tx hashes appear on chain with the expected calldata structure.

---

## Configuration

Per-chain TOML files in `config/`. Each one defines:

- `[chain]` — RPC/WSS URLs (via env-var substitution `${BSC_RPC_URL}`),
  contract addresses, block time, scan budget.
- `[wallet]` — which env var holds the private key.
- `[scanner]` — flash tokens, default flash amounts, optimization iterations,
  dry-run flag.
- `[scanner.flash_amounts]` — starting flash amount per token.
- `[scanner.flash_bounds.<TOKEN>]` — min/max bounds for the ternary search.
- `[gate]` — multi-stage profit gate: `min_profit_bps`, `min_profit_usd`,
  `safety_margin_bps`, `stable_pool_extra_margin_bps`.
- `[submission]` — venue config, `warp_threshold_usd`, builder URLs.
- `[circuit_breaker]` — consecutive-revert threshold, suppression window.
- `[tokens]` — token symbol → address.
- `[token_usd_prices]` — used for $-denominated profit gating.
- `[[pools]]` — repeated table; one entry per pool (address, protocol type,
  token0, token1, fee_bps, optional metadata).

Pools are validated at startup: the binary fetches each pool's reserves and
verifies they're nonzero before adding it to the path enumerator, so a bad
config entry just drops a single pool instead of crashing the whole scanner.

---

## Supported protocols

| Protocol family | Implementation | Chains |
|---|---|---|
| Uniswap V2 (and forks: PCS V2, BiSwap, ApeSwap, …) | exact constant-product, custom fee_bps | BSC, Base |
| Uniswap V3 (and forks: PCS V3, Sushi V3) | exact tick-math, sqrt-price reconstruction | BSC, Base |
| Uniswap V4 | PoolManager-based via the deployed contract | BSC, Base |
| PancakeSwap StableSwap (Curve-style) | Newton iteration on the invariant | BSC |
| Wombat | logarithmic invariant w/ haircut | BSC |
| DODO V2 (PMM) | proactive market maker | BSC |
| Algebra / Thena Fusion (V3-style with dynamic fees) | tick-math + on-chain fee read | BSC |
| Aerodrome V2 (volatile + stable, Solidly-style) | exact `_k()` + Newton invariant solver | Base |
| Aerodrome Slipstream (V3-style w/ tick spacing) | tick-math | Base |

Adding a new protocol is a fairly small change: implement the swap math in
`arb-core/src/<protocol>.rs`, add a variant to the `Protocol` enum, wire it
into the StateReader bucket in `arb-state/src/refresher.rs`, and add a swap
encoding branch in the contract.

---

## Known limitations & open problems

This is a working, instrumented system that is **not currently profitable**.
Here are the things I think would actually move the needle, in rough order of
expected impact:

1. **Co-located builder relationships.** On BSC, the dominant flow is going
   through 48Club + Puissant. Submitting via their public endpoint means we're
   strictly behind anyone with a private relationship.
2. **Path coverage.** ~280 paths on BSC and ~450 on Base is enough to catch the
   obvious WBNB/USDT/USDC triangles, but wider DEX-listed assets (memecoins,
   newly-launched tokens) likely have larger and longer-lived spreads that we
   never see because we don't index those pools.
3. **Cross-DEX-tier strategies.** Stable-stable spreads between PancakeSwap
   StableSwap and Wombat are usually tiny; the long-tail of vol/stable
   misalignments is where the real edges live.
4. **Sub-block latency.** We currently react block-by-block. Base's Flashblocks
   (200ms pre-confirmations) and BSC's mempool stream are wired up but
   under-exploited — only V2/V3 selectors are decoded right now.
5. **Better mempool decoding.** The decoder only handles a handful of selectors.
   Universal Router, 1inch aggregators, and Aerodrome's router are common in
   the mempool and currently invisible to the backrun engine.
6. **Capital-aware Warp gating.** `submission.warp_threshold_usd` is a constant;
   it should adapt to recent spend and recent landed-trade EV.

If any of these sound interesting, [see CONTRIBUTING.md](CONTRIBUTING.md).

---

## Project layout

```
arbv2/
├── Cargo.toml              workspace root
├── crates/
│   ├── arb-core/           AMM math kernels
│   ├── arb-state/          on-chain state refresher
│   ├── arb-paths/          closed-loop path enumeration
│   ├── arb-sim/            simulation + ternary optimizer + profit gate
│   ├── arb-submit/         MEV bundle submission + presign pool
│   ├── arb-mempool/        pending tx decoder
│   ├── arb-rpc/            alloy wrapper + nonce cache
│   └── arb-runner/         main binary, per-chain scanner loop
├── contracts/              Foundry project
│   ├── src/
│   │   ├── BscFlashArb.sol
│   │   ├── BaseFlashArb.sol
│   │   └── StateReader.sol
│   └── script/             deploy scripts
├── config/
│   ├── bsc.toml
│   └── base.toml
├── ops/
│   ├── systemd/            unit files (path-templated, edit before use)
│   └── prometheus/         scrape config
└── dashboard.py            live terminal dashboard
```

---

## License

MIT — see [LICENSE](LICENSE). Use it, fork it, sell it, learn from it.

## Tip jar

If this repo helped you (or you just want to encourage more open-source MEV
work), tips are appreciated. The wallet that operates the contracts on chain is:

```
0xe2Bf8Febd9Ad5703F3E703BdC7B277F7cB0F253f
```

Any EVM chain. ETH/BNB/stables/whatever. Thank you.

## Security

If you find a vulnerability — especially anything that could be used to drain
the deployed contracts or leak the operating wallet's keys via some indirect
path — please follow the responsible disclosure process in
[SECURITY.md](SECURITY.md) before opening a public issue.
