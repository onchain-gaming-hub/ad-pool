# ad-pool

**Per-game ad slots on Solana, auctioned in SOL and moderated before they reach the screen.**

Part of [blockchain-games-hub](https://github.com/solana-games-hub): fully on-chain games that share a platform layer (game registry, roles, treasury), lobbies, reward pools — and this ad marketplace.

- Every game sells **ad slots** of a fixed size (W×H), one auction per **epoch** (days) per slot.
- Anyone with an external wallet can bid in **SOL**. A higher bid refunds the previous leader **in the same transaction**.
- The winner submits an **image + short text + click URL**. A **moderator Worker** checks the file, runs AI moderation on the image and the text, and checks the URL against reputation data — then writes the verdict **on chain**. Approved images are pinned by **sha256**, so they can never be swapped.
- When the epoch starts, the winning bid is paid out: **99% to the game owner, 1% to the platform treasury**.
- Game frontends read the accounts directly — **no CPI from games, no backend required**. If nothing has passed moderation, the game shows the text only, or the platform's default ad.

## How it works

```
advertiser wallet ──open_auction / bid (SOL)──▶ ad-pool program ◀── reads ── game frontend
                                                     ▲                         │
winner ── signed creative (image, text, URL) ──▶ moderator Worker              │ image (sha256-checked)
                                                     │  file checks · AI moderation · URL reputation
                                                     ├─ approve / reject on chain (Moderator role)
                                                     ├─ approved image → R2 ──── GET /ads/<sha256> ──┘
                                                     └─ cron: settle → payout · close → rent refund
```

| Component | Where | Stack |
|---|---|---|
| On-chain program | [`programs/ad_pool`](programs/ad_pool) | Anchor 1.2.1 · Rust |
| Moderator Worker | [`worker/`](worker) | Cloudflare Workers (Rust, `worker` 0.8.7) · Durable Objects · R2 · OpenAI `omni-moderation-latest` · Google Web Risk |
| Platform dependency | platform-core (separate repo) | game registry (`GameEntry`), RBAC roles, fee config, treasury, default ads |

## Integrate

**Building a frontend, SDK or script against ad-pool? Start with [`SKILL.md`](SKILL.md)** — showing ads in a game (with fallbacks and safety rules), bidding, submitting a creative to the Worker, and configuring a game's slots, with PDAs, account offsets, instruction layouts and error codes.

| | devnet |
|---|---|
| ad-pool program | `6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk` |
| platform-core program | `AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G` |
| IDL | [`idls/ad_pool.json`](idls/ad_pool.json) (v0.1.0) · [`idls/platform_core.json`](idls/platform_core.json) |

Mainnet: not deployed.

## Repository layout

```
programs/ad_pool/     Anchor program (12 instructions) + LiteSVM tests
worker/               moderator Worker: src/ · tests/ (golden, intake, e2e.mjs) · examples/ · wrangler.toml
idls/                 ad_pool.json (exported) · platform_core.json (copy of the neighbor's IDL)
scripts/              test.sh · export-idl.sh · devnet/ (smoke.ts, a no-op test game)
docs/                 worker-api.md — the Worker's HTTP contract
SKILL.md              frontend integration guide (English)
```

## Build & test

Toolchain: Rust (pinned 1.99.0 via `rust-toolchain.toml`) · Anchor CLI 1.2.1 · Solana/Agave CLI 4.3 · Node.js ≥ 18 (for the Worker's wrangler and e2e).

```bash
# program: build + LiteSVM tests
./scripts/test.sh

# Worker: unit + golden + intake tests, lint (native and wasm32)
cd worker
cargo test
cargo clippy --all-targets && cargo clippy --target wasm32-unknown-unknown --all-targets

# Worker end-to-end: the real Worker on `wrangler dev` against an in-process mock of
# Solana RPC, OpenAI and Web Risk (throwaway keys, no secrets needed)
npm install
npm run e2e
```

To run the Worker against devnet yourself, copy `worker/.dev.vars.example` to `worker/.dev.vars` (gitignored) and fill in the secrets: a keyed devnet RPC URL, an OpenAI API key, a Web Risk API key, a moderator keypair that holds platform-core's Moderator role, and a separate crank keypair.

## Security notes

- **The chain is the source of truth.** Only a key holding the platform-core **Moderator** role can approve, reject or revoke a creative; frontends never need to trust the Worker's HTTP responses.
- **Fail closed.** If any check cannot run (moderation API down, URL reputation unavailable, RPC errors), the Worker approves nothing.
- **Wallet-signed submissions.** Creatives are authenticated with an ed25519 message signature from the winning wallet (never a transaction signature), bound to the Worker's domain, cluster, slot, content hash, a nonce and an expiry.
- **Known limitations.**
  - Image moderation cannot detect hate or illicit content in images.
  - The signed message shows a content hash rather than the content itself.
  - A click URL's destination can change after approval. The Worker re-checks live ads every ~10 minutes and can revoke them, and frontends must still warn users before leaving the game.

## Status

- **Program:** feature-complete for v1 and deployed on **devnet**. 116 tests pass.
- **Moderator Worker:** feature-complete. Unit, golden and intake tests pass, plus a 53-case end-to-end suite.
- **This is devnet software.** It has not been audited.
