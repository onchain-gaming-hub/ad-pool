---
name: ad-pool-integration
description: How to connect a frontend to blockchain-games-hub ad-pool — show the right ad in a game (read GameAdConfig / AdSlotAuction / DefaultAd, verify the image hash, fall back to text or the platform default ad), let advertisers bid with SOL from an external wallet (open_auction / bid), submit a creative to the moderator Worker (signed message + multipart), and configure a game's ad slots as its owner. Use when building or reviewing any FE, SDK or script that reads or writes ad-pool accounts or calls the ad-pool Worker.
---

# ad-pool — Frontend Integration Guide

> Source: `blockchain-games-hub/ad-pool/SKILL.md` · maintained by the blockchain-games-hub maintainers · version 2026-10-11 (program IDL 0.1.0 · Worker API v1)
> Deeper references: design `specs/designs/01-program-design-v1.md` · Worker API `specs/designs/04-worker-api-v1.md` · devnet `specs/deploy/devnet-runbook.md` · working examples `scripts/devnet/smoke.ts` (bids that landed on devnet) · `worker/tests/e2e.mjs` (a Worker client written from doc 04)
> **devnet only** for now · `<WORKER_URL>` = origin of the moderator Worker and `<AUTH_DOMAIN>` = that Worker's `AUTH_DOMAIN` value (ask the platform maintainer)

## 0. Overview in 30 seconds

- Each game sells **ad slots** (fixed W×H) per **epoch** (unit: days) · auctions are in **SOL**, one per (game, epoch, slot)
- The winner submits **image + text + click_url** to the **Worker (moderator)** → the Worker checks them (file, AI moderation, URL) and **writes the result on chain itself** · the FE does **not** need to trust the Worker — the chain is the source of truth
- Game FEs **read accounts directly** (no CPI, no backend) · the image is pinned by `image_sha256` on chain
- Money: the winner pays in full (no refund even if the ad never passes moderation) · after the epoch starts, 99% → game owner · 1% → platform Treasury (`ad_fee_bps` snapshotted at the first bid)
- Advertisers use **External Wallets only** (Phantom, Solflare …) and pay their own gas + rent — **no ad-pool instruction goes through the relayer**

| | devnet |
|---|---|
| ad-pool program | `6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk` |
| platform-core program | `AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G` |
| IDL | `ad-pool/idls/ad_pool.json` (+ `platform_core.json` for `DefaultAd`) |
| Test game | `noop_game` `8pqRixeRTuSBUNit1d73oPwX7kneMtcKBKBCgKSvxod4` (slot 0 = 728×90, slot 1 = 300×250 · 1-day epochs) |
| Worker (`<WORKER_URL>`) | `POST /v1/creative` · `POST /v1/default-ad` · `GET /ads/<sha>.<ext>` |

## 1. Who uses what

| User | Does | Sections |
|---|---|---|
| **Game FE** (every game) | Shows ads in its slots — read-only | §3, §4 |
| **Advertiser FE** | Browses slots/prices · bids · submits a creative · resubmits an image | §3, §5, §6 |
| **Game owner** (`GameEntry.owner` in platform-core) | Creates/updates the ad slot config | §7 |
| Platform Admin | Default ads (`DefaultAd`) via the Worker | §8 |
| Nobody (automatic) | settle / close — Worker crank every 10 minutes | §9 |

## 2. PDAs (TypeScript · `@solana/web3.js` 1.x)

```ts
import { PublicKey } from '@solana/web3.js';
export const AD_POOL = new PublicKey('6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk');
export const PLATFORM_CORE = new PublicKey('AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G');
const u16 = (n: number) => { const b = Buffer.alloc(2); b.writeUInt16LE(n); return b; };
const u64 = (n: bigint) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(n); return b; };
const pda = (prog: PublicKey, ...s: Buffer[]) => PublicKey.findProgramAddressSync(s, prog)[0];

export const adConfig  = (game: PublicKey) => pda(AD_POOL, Buffer.from('ad_config'), game.toBuffer());
export const adSlot    = (game: PublicKey, epoch: bigint, slot: number) =>
  pda(AD_POOL, Buffer.from('ad_slot'), game.toBuffer(), u64(epoch), Buffer.from([slot]));
// platform-core
export const gameEntry = (game: PublicKey) => pda(PLATFORM_CORE, Buffer.from('game'), game.toBuffer());
export const pcConfig  = () => pda(PLATFORM_CORE, Buffer.from('config'));
export const defaultAd = (w: number, h: number) => pda(PLATFORM_CORE, Buffer.from('default_ad'), u16(w), u16(h));
```
- `game` = **the game's program ID** (not its name · not the owner's wallet)
- `epoch` is **u64 little-endian** · `slot` is **u8** — a wrong type silently yields a different address

## 3. Accounts the FE reads (fixed offsets — decodable without Anchor)

> For every account: **check that the account's `owner` is the right program, its size, and its discriminator before trusting the data** (otherwise anyone can pass a look-alike fake account) · discriminators are in the IDL (`accounts[].discriminator`)

**`GameAdConfig`** — `[b"ad_config", game]` · owner = ad-pool · **199 B**

| offset | field | type | |
|---:|---|---|---|
| 10 | `slot_count` | u8 | valid slots are 0..slot_count |
| 11 | `game_program` | Pubkey | |
| 43 | `genesis_ts` | i64 | 00:00 UTC (multiple of 86,400) · immutable |
| 51 | `epoch_duration_secs` | i64 | multiple of 1 day · ≤ 30 days · immutable |
| 59 | `auction_close_lead_secs` | i64 | ≥ 7 days · bidding closes this long before the epoch starts |
| 67 | `min_bid` | u64 | lamports · first bid |
| 75 | `min_increment_bps` | u16 | |
| 77 | `bid_horizon_epochs` | u16 | how many epochs ahead bidding is open |
| 79 | `slots[8]` | 5 B per slot: `width` u16 · `height` u16 · `active` bool | |

**`AdSlotAuction`** — `[b"ad_slot", game, epoch u64 LE, slot u8]` · owner = ad-pool · **779 B** · **no account = nobody has bid yet** (or the epoch is over and the account was already closed by `close_auction` — history is in the `AuctionClosed` event)

| offset | field | type | |
|---:|---|---|---|
| 10 | `creative_status` | u8 | **0 Empty · 1 TextOnly · 2 Approved** |
| 11 | `settled` | bool | |
| 13 | `game_program` | Pubkey | used to derive `gameEntry(...)` for `bid` |
| 45 | `epoch` | u64 | |
| 53 / 55 | `width` / `height` | u16 | snapshot of the slot |
| 57 | `close_ts` | i64 | bidding closes |
| 65 | `epoch_start_ts` | i64 | |
| 73 | `epoch_end_ts` | i64 | |
| 81 | `fee_bps` | u16 | snapshot of `ad_fee_bps` at the first bid |
| 83 | `min_increment_bps` | u16 | snapshot |
| 85 | `rent_payer` | Pubkey | first bidder (gets the rent back on close) |
| 117 | `highest_bidder` | Pubkey | leader / winner |
| 149 | `highest_bid` | u64 | lamports |
| 157 | `bid_count` | u32 | |
| 161 | `reject_count` | u16 | number of rejects / revokes |
| 163 | `last_reject_flags` | u32 | latest reasons (bits in §6.5) |
| 167 | `approved_at` | i64 | 0 = never approved (revoke resets it to 0) |
| 215 | `creative` | `image_url` string · `image_sha256` [32] · `text` string · `click_url` string | string = u32 LE length + UTF-8 bytes |

**`DefaultAd`** (platform-core) — `[b"default_ad", w u16 LE, h u16 LE]` · owner = platform-core · **651 B** · `enabled` @14 (bool) · `creative` @55 (same layout) · no account / `enabled = false` / undecodable → **show nothing**

```ts
function readCreative(d: Buffer, off: number) {
  const str = () => { const n = d.readUInt32LE(off); const s = d.subarray(off + 4, off + 4 + n).toString('utf8'); off += 4 + n; return s; };
  const image_url = str(); const image_sha256 = d.subarray(off, off + 32); off += 32;
  const text = str(); const click_url = str();
  return { image_url, image_sha256, text, click_url };
}
```

## 4. Showing ads in a game (game FE)

### 4.1 Current epoch + phase (compute it yourself — status does not change by itself over time)
```ts
const now = Math.floor(Date.now() / 1000);              // the client clock is fine for *display*
const epoch = now < genesis ? null : BigInt(Math.floor((now - genesis) / duration));
// per auction: now < close_ts → Bidding · < epoch_start → CreativeWindow · < epoch_end → Live · else Ended
```
- ⚠️ The program's **decisions** (`BiddingClosed`, the creative window) use the validator clock · near a boundary (`close_ts` etc.) allow ±60 s, or read the time from the chain (`getBlockTime`) before letting the user act

### 4.2 What to show (per slot)
1. Read `GameAdConfig` + `AdSlotAuction(current epoch, slot)` + `DefaultAd(w, h)` in **one `getMultipleAccounts` call**
2. The auction exists (owner/size/discriminator valid) **and** `epoch_start_ts ≤ now < epoch_end_ts`:
   - `Approved` (2) → **image** + text + click_url — **download the image and check `sha256(bytes) == image_sha256` before showing it** · mismatch → treat as TextOnly
   - `TextOnly` (1) → **text** + click_url instead of the image (the image has not passed yet)
   - `Empty` (0) → step 3
3. `DefaultAd` for this size is `enabled` → show it like `Approved` · platform-core only enforces `image_sha256 ≠ 0` + lengths + `click_url` starting with `https://` → if `image_url` is empty or the hash does not match, show the text instead · **if `text` is empty too → step 4** · show a link only when `click_url` has a host after `https://`
4. Nothing → show nothing (or the game's own placeholder)

### 4.3 Rendering safety rules (mandatory)
- **`text` is plain text only** (`textContent`, not `innerHTML`) · UTF-8 ≤ 120 **bytes** (Thai ≈ 40 characters)
- **Image**: check that the origin of `image_url` is the platform's Worker (allowlist) **and** check the sha256 · on chain only the length is checked · do not trust the content-type
- **click_url (the FE is the third check layer)**: the Worker checks it at approval and re-checks it every ~10 minutes, but the destination can still change → **always show a warning before leaving the game** ("You are leaving to `<host>`") · open with `rel="noopener noreferrer"` · **do not include Google attribution** in the warning (Web Risk terms)
- **No need to re-verify who won / whether they paid** — the on-chain status can only be written by the Worker (Moderator role)

### 4.4 Caching
- `Approved` is permanent for the epoch (locked by sha256) · but the Worker can **revoke** it (back to `Empty`) → re-poll every 1–5 minutes, or use `onAccountChange`
- Images: `GET /ads/<sha256>.<ext>` is immutable (cache forever · has an ETag)

## 5. Bidding (advertiser FE · External Wallet)

### 5.1 First bid vs later bids
- No `AdSlotAuction` yet → **`open_auction`** (creates the account · the bidder pays rent ≈ 0.0046 SOL — refunded on close)
- Already exists → **`bid`** · if two people `open_auction` at the same time, the second fails (account already exists) → re-read and use `bid`

### 5.2 Minimums
- First bid: `amount ≥ max(GameAdConfig.min_bid, min_bid_floor)` · `min_bid_floor = max(rent(779 B), ceil(rent(0) × 10,000 / 8,000))` — currently equal to rent(779) = 4,607,560 lamports on devnet
- Later bids: `amount ≥ highest_bid + max(1, ceil(highest_bid × min_increment_bps / 10,000))` (using the **auction's** `min_increment_bps`)
- The leader may raise their own bid (`prev_bidder` = themselves · only the difference is transferred)

### 5.3 Timing
- Open while `now < close_ts` **and** `epoch_start ≤ now + bid_horizon_epochs × duration` · `close_ts = epoch_start − auction_close_lead_secs` (≥ 7 days)
- The game must be `Active` in platform-core for every bid · the slot must be `active`

### 5.4 Instructions (account order matches the IDL · working example `scripts/devnet/smoke.ts`)

| ix | discriminator | data | accounts (signer · writable) |
|---|---|---|---|
| `open_auction` | `[48,60,204,12,175,130,173,33]` | game Pubkey · epoch u64 · slot_index u8 · amount u64 | bidder (s,w) · `gameEntry(game)` · `pcConfig()` · `adConfig(game)` · `adSlot(game,epoch,slot)` (w) · SystemProgram |
| `bid` | `[199,56,85,38,146,243,37,158]` | amount u64 | bidder (s,w) · auction (w) · `gameEntry(auction.game_program)` · **prev_bidder = `auction.highest_bidder`** (w) · SystemProgram |

- **The previous leader is refunded in the same tx** — `prev_bidder` must match what you read · if someone outbids you before your tx lands → error **`LeaderChanged` (6017)** → **re-read the auction and build a new tx** (do not retry the old one)
- Anchor TS works too: **`@anchor-lang/core` 1.2.1** + `idls/ad_pool.json` (⚠️ do not use `@coral-xyz/anchor` — it is stuck at 0.32 and does not work with Anchor 1.x)

## 6. Submitting a creative to the Worker (winner · after bidding closes)

### 6.1 When
- Within `[close_ts, epoch_end_ts)` (including while the epoch is live — once approved it shows immediately)
- **full** (image + text + click_url) when status = `Empty` · **image-only** (a new image only) when status = `TextOnly` · `Approved` = locked, cannot change

### 6.2 Message for the wallet to sign (`signMessage` · UTF-8 · `\n` · **no trailing newline**)
```text
ad-pool creative submission
domain: <AUTH_DOMAIN of the Worker>
cluster: devnet
program: 6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk
game: <game program>
epoch: <epoch>
slot: <slot>
payload: <lowercase sha256 hex>
nonce: <32 lowercase hex chars — new random value every time>
expires: <unix seconds · > now and ≤ now + 600>
```
```ts
// payload = sha256("ad-pool/v1" ‖ sha256(image) ‖ u32le(len text) ‖ text ‖ u32le(len click_url) ‖ click_url)
// image-only: text = "" and click_url = "" · len is the number of BYTES (UTF-8), not characters
import bs58 from 'bs58';
const raw = await wallet.signMessage(new TextEncoder().encode(message));  // Uint8Array(64) (wallet-adapter)
const signature = bs58.encode(raw);   // ← send the `signature` field as **base58** (not hex / bytes / base64)
```
- Phantom's direct provider returns `{ signature }` → `bs58.encode(result.signature)` · the wrong encoding = 401 `BadSignatureEncoding`
- Canonical decimals (no leading zeros) · **sign a message only, never a transaction** · the Worker rebuilds the message from the submitted values — if it differs by even 1 byte = 401

### 6.3 `POST /v1/creative` (`multipart/form-data` · `Content-Length` required)
fields: `game` · `epoch` · `slot` · `signer` (the winner's pubkey) · `nonce` · `expires` · `signature` · `image` (file) · `text` · `click_url` (image-only: omit text/click_url)

- **Image**: PNG / JPEG / WebP (detected from magic bytes) · **W×H must match the slot exactly** · ≤ 1 MiB · not animated · no trailing bytes · EXIF allowed if it does not rotate the image and has **no GPS** (strip it before sending)
- **text**: 1–120 bytes · no control / invisible / bidi-override characters
- **click_url**: `https://` · no IP / port / user:pass / punycode / shortener / internal names · ≤ 200 bytes · what goes on chain is the normalized form (e.g. lowercase host + trailing `/`)

### 6.4 Responses (summary — the full contract is `specs/designs/04-worker-api-v1.md` · if they disagree, doc 04 wins)
| HTTP | Meaning | What the FE does |
|---|---|---|
| 200 `approved` / `text_only` / `image_approved` | On chain (has `signature`) | Show the result · `text_only` = the image failed (see `reasons`) → let the user send a new image as image-only |
| 422 `rejected` | Failed · reasons recorded on chain (`flags`, `reasons`) | Fix and resubmit (status unchanged) |
| 202 `confirmed:false` | Tx sent but not yet confirmed | Re-read the auction · **do not resubmit within 180 s** (the slot's lock is still held → 409 `InFlight`) |
| 400 / 401 | Bad input (incl. `multipart_required`) / signature failed | Fix on the client |
| 411 `length_required` · 413 `too_large` | No `Content-Length` / body over 1 MiB + 64 KiB | Send `Content-Length` · shrink the image |
| 404 `no_auction` | Nobody has bid on this slot (or it was already closed) | Check game/epoch/slot |
| 403 `NotWinner` · 409 `NotInCreativeWindow` / `WrongKind` / `AlreadyApproved` | Wrong signer / wrong time / wrong kind | |
| 409 `InFlight` | Another submission for this slot is in progress | **Resend the same request** after `retry_after_secs` (the nonce was not consumed) |
| 409 `Replay` · 429 `RateLimited` | Nonce reused · more than 10 per hour per slot | Re-sign / wait |
| 503 (e.g. `moderation_unavailable`, `moderator_unavailable`, `rpc_*`, `storage_unavailable`, `deadline`, `gate_unavailable`) | A check could not run (fail closed) — nothing went on chain | **Re-sign with a new nonce** and try again |
| 502 `tx_unknown` | Tx sent but the outcome is unknown (may have landed · has `signature`) | **Do not re-sign right away** — check the `signature` / re-read the auction first (lock held for 180 s) |
| 502 `tx_rejected` / `tx_failed` (with `program_error`) | The program refused (e.g. 6012 `SlotInactive`) | Look the code up in §10 · **retrying will not help** |
| 502 other (`rpc_*`, `bad_auction_account`) | RPC / account anomaly | Try again later · report it if it repeats |

### 6.5 Bits of `flags` / `last_reject_flags`
`1` file · `2` W×H · `4` image failed moderation · `8` text failed · `16` click_url layer 1 · `32` click_url layer 2 (Web Risk) · `64` too long

## 7. Game owner: configuring ad slots

Must be signed by **`GameEntry.owner`**, with the game `Active` (at init) · instructions are in the IDL (`init_ad_config`, `update_ad_config`, `add_slot`, `set_slot_active`)

| field | Rule (enforced by the program) |
|---|---|
| `genesis_ts` | ≥ 0 · **multiple of 86,400** (00:00 UTC = 07:00 ICT) · ≤ now + 90 days · **cannot be changed later** |
| `epoch_duration_secs` | multiple of 86,400 · ≤ 30 days · **cannot be changed later** |
| `auction_close_lead_secs` | **≥ 7 × 86,400** (does not have to be a whole number of days) |
| `min_bid` | ≥ rent(779 B) |
| `min_increment_bps` | 1..=10,000 |
| `bid_horizon_epochs` | `× duration ≤ 90 days` and `≥ lead + 1 epoch` |
| `slots` | 1..=8 slots · W, H > 0 · **add-only — cannot be removed or resized** · to stop selling a slot = `set_slot_active(false)` |

- `update_ad_config` (every field is an Option) applies to **new auctions** only — auctions already open have snapshotted their values
- A wrong `genesis_ts` / duration makes that config permanently unusable (there is no re-init) → double-check before signing
- Working example: `scripts/devnet/smoke.ts` (`init_ad_config`)

## 8. Default ads (platform Admin)
- `POST /v1/default-ad` on the Worker · the sender must be a wallet with platform-core `RoleMember(Admin)` · goes through every check in §6 (all must pass — no text-only)
- The signed message starts with `ad-pool default ad submission` and has `program: <platform-core>`, `width`, `height` instead of game/epoch/slot (details in doc 04)
- Enable/disable (`set_default_ad_enabled`) → the Admin signs it directly against platform-core from their wallet (the Worker has no endpoint for this)

## 9. settle / close (nothing to do)
- The Worker crank (every 10 minutes) sends `settle` after `epoch_start_ts` (money → game owner 99% / Treasury 1%) and `close_auction` after `epoch_end_ts` (rent → `rent_payer`) · both are **permissionless** — anyone may call them (funds always go to the right place)

## 10. Error codes FEs hit most (all are in the IDL `errors`)

| code | name | What to do |
|---:|---|---|
| 6000 | `GameNotActive` | Game is suspended — no bids |
| 6008 / 6009 | `MinBidBelowFloor` / `BidBelowFloor` | Below the account's rent |
| 6011 / 6012 | `InvalidSlot` / `SlotInactive` | No such slot / slot not for sale |
| 6014 | `BiddingClosed` | Past `close_ts` |
| 6015 | `BeyondHorizon` | Epoch is further ahead than bidding allows |
| 6016 | `BidTooLow` | Below min_bid or the minimum increment |
| 6017 | `LeaderChanged` | Outbid → re-read + build a new tx |
| 6001 / 6002–6006 / 6010 / 6013 | config | See §7 |

## 11. Checklist before going live
- [ ] Check owner + size + discriminator of every account before reading it
- [ ] `epoch` is u64 LE · `slot` is u8 in seeds
- [ ] Images: origin allowlist + `sha256 == image_sha256` · text as plain text · a warning before following click_url
- [ ] Full fallback: Approved → TextOnly → DefaultAd(enabled) → empty
- [ ] Bids: pass `prev_bidder` = the `highest_bidder` you just read · handle `LeaderChanged`
- [ ] Creatives: the signed message matches the template byte for byte · a new nonce every time · send `Content-Length`
- [ ] External Wallet only · never send ad-pool instructions through the relayer
- [ ] ⚠️ The wallet only shows a hash in the signed message → the FE should show the text / click_url / image clearly before the user signs (a phishing page could trick a winner into signing someone else's creative · a SIWS-based flow is under consideration)
