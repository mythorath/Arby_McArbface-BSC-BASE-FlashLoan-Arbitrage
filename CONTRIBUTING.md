# Contributing

PRs and issues are welcome. This is a hobby project, so the bar is "obviously
helpful and obviously not malicious" rather than any formal process.

## Quick orientation

- Read the [README](README.md) and skim `arb-runner/src/runner.rs` — that's the
  scanner main loop and references almost every other crate.
- Pool math kernels live in `arb-core/src/<protocol>.rs`. Each one has unit
  tests that pin the output against on-chain reference values.
- Submission venues live in `arb-submit/src/<venue>.rs`. They all implement
  the `Submitter` trait.

## Things that would obviously help

The **Known limitations** section of the README is the canonical to-do list.
In rough order of impact:

1. Wider path coverage — more pools, more chains. New pools are mostly a
   `config/<chain>.toml` change plus a quick liquidity validation.
2. New protocol support — implement the swap math in `arb-core`, add a
   `Protocol` enum variant, wire it into `StateReader.sol`.
3. Better mempool decoding — Universal Router, 1inch, aggregator routers.
4. New chains — Arbitrum, Polygon, Optimism, Linea, Sonic. Most of the work is
   getting `StateReader.sol` deployed and finding a free local builder.
5. Adaptive Warp gating — replace the constant `submission.warp_threshold_usd`
   with something that tracks recent EV.
6. Smarter circuit breaker — currently consecutive-revert-count-based; could
   use a sliding-window revert rate or differentiate "spread closed" reverts
   from "broken path" reverts.

## Things to avoid

- **Don't commit anything that touches `.env`, the operating wallet, or any
  RPC URL with an embedded API key.** The `.gitignore` is comprehensive but
  please double-check `git status` before committing.
- **Don't add a new MEV venue without a free tier.** Paid relays are fine in
  principle but they belong behind the existing `warp_threshold_usd` gate, not
  in the always-on path.
- **Don't change contract storage layout without a redeploy plan.** The
  deployed contracts in this repo's `.env.example` are live. If you change
  storage you need a new deployment.

## Style

- Rust: `cargo fmt && cargo clippy`. CI is not yet set up; run them locally.
- Solidity: `forge fmt`. Keep gas usage in mind — the swap loop in
  `BscFlashArb.executeV4Arbitrage` is on the hot path.

## Testing

- `cargo test --workspace` for the Rust side.
- `forge test` inside `contracts/` for the Solidity side.
- `cargo run --release --bin validate-pools -- config/<chain>.toml` to compare
  in-process AMM math against on-chain `getAmountOut`/`quoteExactInputSingle`
  for every pool in a config. Anything with a non-trivial delta is a bug in
  the kernel, in the config, or in the `StateReader` decoder.

## Disclosure of bugs that affect funds

Please follow [SECURITY.md](SECURITY.md) for anything that could be used to
drain the deployed contracts or the operating wallet — open a security
advisory or email the most recent commit author privately, not a public issue.
