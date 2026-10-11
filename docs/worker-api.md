# Worker API v1 — ad-pool moderator Worker

The HTTP contract of the moderator Worker: what an advertiser's frontend (external wallet) sends, what the Worker checks, and what it answers. Every endpoint is **devnet only** for now.

Code: `worker/src/{auth,image,submission,gate,moderation,urlcheck,webrisk,decision,tx,ads,defaultad}.rs` · tests: `worker/tests/{intake,golden}.rs`, unit tests, and `worker/tests/e2e.mjs` (a client written from this document, so drift between this file and the Worker fails a test). For the frontend side (PDAs, account offsets, bidding, display rules) see [`SKILL.md`](../SKILL.md).

## CORS

Browser pages on another origin can call the Worker only if their origin is listed in the Worker's `CORS_ALLOW_ORIGINS` var (comma-separated exact origins, e.g. `https://app.example,http://localhost:5173`; no wildcard). A listed origin gets `Access-Control-Allow-Origin: <its origin>` + `Vary: Origin` on every response, and `OPTIONS` preflights answer 204 with the allowed methods (`GET, POST, OPTIONS`) and headers. Unset or empty → no CORS headers.

## `POST /v1/creative` — the winning bidder submits a creative

`multipart/form-data`. A `Content-Length` header is required (missing → 411; larger than `MAX_IMAGE_BYTES` + 64 KiB → 413).

| field | type | notes |
|---|---|---|
| `game` | text | game program ID (base58) |
| `epoch` | text | u64, decimal |
| `slot` | text | u8, decimal |
| `signer` | text | pubkey of the signing wallet; must be the auction's `highest_bidder` |
| `nonce` | text | 32 lowercase hex chars, fresh every time (cannot be reused) |
| `expires` | text | unix seconds, > now and ≤ now + 600 |
| `signature` | text | ed25519 signature (**base58** — the result of `signMessage`) over the message below. **Do not send the message itself**: the Worker rebuilds it from its config, the submitted fields and the hash it computes, then verifies the signature over those bytes (this avoids multipart turning `\n` into `\r\n`). |
| `image` | file | PNG / JPEG / WebP · **W×H must exactly match the slot** · ≤ `MAX_IMAGE_BYTES` (default 1 MiB) · **no animation** (APNG / animated WebP) · type is taken from magic bytes, not the content-type · the structure must be complete (PNG through IEND, JPEG ending at EOI, WebP with a matching RIFF size) with **no trailing bytes** · EXIF/XMP are allowed only if Orientation = 1 (or absent) and there is **no GPS data** (in EXIF or XMP) — otherwise strip before sending · progressive JPEG is fine · the WebP canvas must equal the image |
| `text` | text | **full**: required · 1–120 **bytes** of UTF-8 · no control characters and no invisible / direction-changing characters (zero-width, bidi overrides, …) |
| `click_url` | text | **full**: required · lowercase `https://` followed by something · ≤ 200 bytes · no whitespace/control characters · **layer 1** rules (below) · the value written on chain is the WHATWG-normalized form; the signature covers the string as submitted |

Submission kinds:
- **full** (`text` + `click_url` present) — allowed when the auction status is `Empty`.
- **image-only** (neither field) — allowed when the status is `TextOnly` (text and link already approved; only a new image can be sent).

### Message to sign

The frontend builds the same bytes to call `signMessage`: UTF-8, lines separated by `\n`, no trailing newline.

```text
ad-pool creative submission
domain: <the Worker's AUTH_DOMAIN>
cluster: devnet
program: 6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk
game: <game_program>
epoch: <epoch>
slot: <slot>
payload: <lowercase sha256 hex of the payload>
nonce: <32 lowercase hex chars, fresh every time>
expires: <unix seconds, > now and ≤ now + 600>
```

- Numbers are canonical decimal (no leading `0`). Keys appear in exactly this order. `domain` is the Worker's `AUTH_DOMAIN`; the frontend must use the same value.
- ⚠️ The domain in the message only means as much as the wallet shows the user: a phishing page can ask for a signature over a message that names the real domain. A stronger option for the future is wallet-standard SIWS `signIn`, where the wallet checks the domain itself.
- **payload** = `sha256("ad-pool/v1" ‖ sha256(image) ‖ u32le(len(text)) ‖ text ‖ u32le(len(click_url)) ‖ click_url)`. For image-only, `text` and `click_url` are empty.
- Sign a **message** only, never a transaction.

### `click_url` layer 1 — rules (applied to the submitted URL **and every redirect hop**)

`https` only · no `user:pass@` · no port (other than 443) · the host must not be an IP address (including forms like `https://3232235521/` or `0x7f.1`) · no `xn--` labels (IDN/punycode, to block homographs — so internationalized domains such as `.ไทย` cannot be used in v1) · at least 2 labels · not an internal name (`localhost`, `*.local`, `*.internal`, `*.arpa`, `*.test`, …) · not a link shortener (`bit.ly`, `t.co`, `tinyurl.com`, … and their subdomains) · **must not point back at the Worker itself** (the host of `PUBLIC_BASE_URL` / `AUTH_DOMAIN` — checked while walking the URL → on-chain reject `SelfHost`) · normalized form ≤ 200 bytes.

- The submitted URL breaks a rule → **400** `{"reason":{"ClickUrl":"Shortener"}}` (no transaction — the sender can fix it).
- A redirect hop breaks a rule → **on-chain reject** (flag `URL_LAYER1`). The Worker follows redirects itself (`redirect: manual`), ≤ 5 hops, 5 s timeout per hop.

### `click_url` layer 2 — Google Web Risk Lookup

`uris:search` with threat types MALWARE / SOCIAL_ENGINEERING / UNWANTED_SOFTWARE. Checked **before** each hop is fetched, so a host known to be dangerous is never contacted. The API key travels in the `x-goog-api-key` header, never in the URL. A match → on-chain reject (flag `URL_LAYER2`). If the check cannot run (no key, billing disabled, timeout) → **nothing is approved** (503).

Layer 3 is the frontend's job: warn the user before leaving the game (see `SKILL.md`).

### Order of checks

1. Fields, image (type, structure, file size, no animation, EXIF), text, click_url (+ layer 1) → **400**
2. Nonce + expiry + signature over the message the Worker rebuilt → **401**
3. RPC cluster check (genesis hash) → read `AdSlotAuction` (owner = ad-pool, size 779 B) → missing → **404**
4. Inside `[close_ts, epoch_end_ts)`, else **409** `NotInCreativeWindow` (checked before the signer — it is public data) · `signer == highest_bidder`, else **403** · status matches the submission kind, else **409** · image W×H matches the slot, else **400**
5. Per-auction gate (Durable Object): a nonce cannot be reused → **409** `Replay` · another submission for this auction is in progress → **409** `InFlight` (the nonce is **not** consumed — resend the same request after `retry_after_secs`) · ≤ `MAX_SUBMISSIONS_PER_HOUR` (default 10) per auction → **429** · gate down/timeout → **503**
6. Holding the per-auction lock the whole time (always released; expires on its own after 180 s):
   1. load the moderator key (must match `MODERATOR_ADDRESS`);
   2. in parallel: OpenAI `omni-moderation-latest` on the image and on the text (**two separate calls**), and URL layers 1–2 on every hop;
   3. decide (table below);
   4. if the image is approved: write it to R2 as `ads/<sha256>.<ext>` **before** sending the transaction;
   5. re-read the auction and repeat check 4;
   6. if the lock has been held for more than 90 s → **503 `deadline`**, no transaction is sent;
   7. sign the transaction with the moderator key (sole signer and fee payer, preflight on), send, and poll for `confirmed` 8 times, 2 s apart (≤ ~16 s).

### Decision table (`decision.rs`)

| kind | image | text | URL | result |
|---|---|---|---|---|
| full | ✅ | ✅ | ✅ | `approve_creative` → **Approved** |
| full | ❌ | ✅ | ✅ | `approve_creative` text-only + `image_reject_flags` → **TextOnly** (a new image can be sent as image-only) |
| full | any | ❌ | any | `reject_creative(flags)` |
| full | any | any | ❌ | `reject_creative(flags)` |
| full | — some check could not run and none of the others decided a reject — | | | **503**, nothing goes on chain |
| image-only | ✅ / ❌ / could not run | — | — | `approve_image` / `reject_creative` / **503** |

Flag bits: `0` file · `1` W×H · `2` image failed moderation · `3` text failed moderation · `4` URL layer 1 (redirect) · `5` URL layer 2 (Web Risk). Every reject carries flags ≠ 0.

- The split: anything decidable from the request alone → **400, no transaction**. Anything decided by an outside source (moderation, a redirect destination, Web Risk) → **recorded on chain**, for transparency — the winner is not refunded if no creative passes.
- Moderation v1 uses OpenAI's `flagged` as is.

### Responses

- **200** `{"status":"approved"|"text_only"|"image_approved","signature":"…","auction":"…","image_sha256":"…","image_url"?:"…","image_reject_flags"?:n,"reasons"?:[…]}` — transaction `confirmed`.
- **422** `{"status":"rejected","flags":n,"reasons":["text:violence",…],"signature":"…"}` — `reject_creative` confirmed. The auction status does not change, so the winner can submit again.
- **202** same body as above plus `"confirmed":false` — sent but not yet confirmed (check the `signature` or read the auction). The auction's lock is **kept until it expires (180 s)** so the transaction is not sent twice.
- **503** `{"error":"moderation_unavailable","unavailable":["web_risk:billing_disabled",…]}` · `storage_unavailable` (R2 down / 10 s timeout) · `moderator_unavailable` · `rpc_rate_limited` · `rpc_unavailable` (node-side error) · `deadline` — nothing went on chain. Sign again with a new nonce and retry.
- **502** `tx_rejected` (preflight failed; `program_error` = the program's error code, e.g. 6012) · `tx_failed` · `tx_unknown` (may have landed — includes `signature`; also covers JSON-RPC -32603; the lock is kept).
- Errors: `{"error":"rejected","reason":{…}}` (400/401/403/409; 502 if the on-chain auction does not match the derived address — a chain-side anomaly) · `{"error":"gate","reason":"Replay"}` (409) · `{"error":"gate","reason":{"RateLimited":{"retry_after_secs":n}}}` (429) · `gate_unavailable` (503 — retry with a new nonce).
- ⚠️ The nonce is consumed once the request passes the gate. If a later step fails transiently, sign again with a new nonce. RPC errors are `{"error":"rpc_unavailable"|"rpc_timeout"|…}` (502) without upstream details, because the RPC URL contains an API key.

## `POST /v1/default-ad` — the platform's default ad

Only a wallet holding platform-core's **`RoleMember(Admin)`** may submit (the Moderator role does not count). `multipart/form-data`, like `/v1/creative`.

| field | notes |
|---|---|
| `width` · `height` | u16, canonical decimal, > 0 — the size of the `DefaultAd` |
| `signer` | the Admin's pubkey |
| `nonce` · `expires` · `signature` | as for creatives (signing the message below) |
| `image` · `text` · `click_url` | **all three required** · same rules as a full creative · image W×H must equal `width`×`height` exactly |

Message the Admin signs (the first line **differs** from a creative's, so a signature cannot be reused across kinds):

```text
ad-pool default ad submission
domain: <AUTH_DOMAIN>
cluster: devnet
program: AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G
width: <u16>
height: <u16>
payload: <sha256 hex — same formula as for creatives>
nonce: <32 hex>
expires: <unix seconds>
```

Order: local checks + signature (400/401) → cluster → `RoleMember(Admin, signer)` owned by platform-core, else **403 `NotAdmin`** → gate per size (`default_ad:<w>x<h>`; Replay / InFlight / RateLimited as for creatives) → OpenAI image + text and URL layers 1–2 → decide: **everything must pass** (there is no text-only result — platform-core requires a non-zero image hash) → R2 → no `DefaultAd` of that size yet → `create_default_ad`, otherwise `update_default_ad` (both with `auth_role_kind = Moderator`; the Worker key pays the fee and rent, ≈ 0.004 SOL). There is no endpoint for `set_default_ad_enabled` — the Admin signs that from their own wallet.

- **200** `{"status":"created"|"updated","signature","default_ad","width","height","image_url","image_sha256"}` · **422** `{"status":"rejected","flags","reasons"}` (**no transaction** — platform-core keeps no record of rejected default ads) · **202** `confirmed:false` · 503/502 as for creatives.

## Other endpoints

| | |
|---|---|
| `GET /ads/<sha256>.<png\|jpg\|webp>` | Approved image from R2 · `content-type` from the type the Worker detected itself · `X-Content-Type-Options: nosniff` · `Cache-Control: public, max-age=31536000, immutable` · `ETag` = sha256 · CSP `default-src 'none'; sandbox` · the Worker re-checks the bytes' sha256 before every response · frontends must also check the hash against `image_sha256` on chain |
| `GET /health` | config + program IDs (no RPC call) |
| `GET /v1/health/rpc` | RPC reachable and on devnet (may be restricted on a production deployment) |
| `GET /v1/auction/:game/:epoch/:slot` | the decoded `AdSlotAuction` + phase (may be rate-limited or cached on a production deployment) |
