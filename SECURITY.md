# Security Policy

## Reporting a Vulnerability

This project's smart contracts hold no idle capital — every flash loan is
borrowed and repaid in the same transaction, and the operating wallet only
holds enough native gas token to pay for submissions (~$10 per chain).

That said, there are still ways things could go wrong:

- A bug in the swap-encoding logic that lets an attacker craft calldata that
  drains tokens that happen to be approved on the contract during a flash loan.
- A bug in the `unlockCallback` flow that lets a non-PoolManager caller
  trigger swaps.
- A bug in `StateReader.sol` that returns malformed data and causes the runner
  to submit a guaranteed-loss tx.
- A bug in the Rust mempool decoder that lets a crafted tx broadcast crash
  the scanner or trigger an infinite loop.
- A leak of the operating wallet's private key via logs, error messages, or
  an environment-variable handling bug.

If you find any of these (or anything else that you think the maintainers
would want to know about *before* the world does), please:

1. **Do not open a public GitHub issue.**
2. Email the address listed in `git log --format='%ae' -1` for the most recent
   commit on `main`. (If GitHub Sponsors / FUNDING.yml lists a contact, that
   works too.)
3. If neither of those works, open a GitHub Security Advisory on this repo
   (Security tab → Advisories → "Report a vulnerability") which keeps the
   discussion private until a fix is published.

A response within a few days is the realistic expectation — this is a
hobby/research project, not a funded protocol with a 24/7 security team.

## Scope

In scope:

- Source code in this repo (Rust crates, Solidity contracts).
- The deployed contracts at the addresses listed in `.env.example`.
- Configuration that, if exploited, could cause the runner to leak secrets.

Out of scope:

- Vulnerabilities in upstream dependencies (alloy, foundry, openzeppelin,
  etc.). Please report those to the upstream projects.
- Generic MEV strategy critiques ("you're going to get sandwiched"). Those
  are perfectly fine as public issues / discussions.
- Bugs in DEX protocols this code interacts with. Report those to the DEX team.

## Reward

There is no formal bug bounty. If a report leads to a real fix, the
contributor will be credited (with permission) in the release notes, and the
tip jar in the README is at your disposal.
