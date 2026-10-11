---
name: ad-pool-integration
description: How to connect a frontend to blockchain-games-hub ad-pool — show the right ad in a game (read GameAdConfig / AdSlotAuction / DefaultAd, verify the image hash, fall back to text or the platform default ad), let advertisers bid with SOL from an external wallet (open_auction / bid), submit a creative to the moderator Worker (signed message + multipart), and configure a game's ad slots as its owner. Use when building or reviewing any FE, SDK or script that reads or writes ad-pool accounts or calls the ad-pool Worker.
---

# ad-pool — คู่มือต่อ Frontend

> ต้นฉบับ: `blockchain-games-hub/ad-pool/SKILL.md` · ผู้ดูแล: maintainer · ฉบับ 2026-10-11 (program IDL 0.1.0 · Worker API v1)
> อ้างอิงลึก: design `specs/designs/01-program-design-v1.md` · Worker API `specs/designs/04-worker-api-v1.md` · devnet `specs/deploy/devnet-runbook.md` · ตัวอย่างที่รันจริง `scripts/devnet/smoke.ts` (bid บน devnet) · `worker/tests/e2e.mjs` (client ของ Worker เขียนจาก doc 04)
> **devnet เท่านั้น** ตอนนี้ · Worker **ยังไม่ deploy** (W6 รออนุมัติ) → URL ของ Worker / `AUTH_DOMAIN` ในคู่มือนี้เป็นค่าตัวอย่าง

## 0. ภาพรวม 30 วินาที

- แต่ละเกมขาย **ช่องโฆษณา** (W×H ตายตัว) เป็นรอบ **epoch** (หน่วยวัน) · ประมูลด้วย **SOL** ต่อ (เกม, epoch, ช่อง)
- คนชนะส่ง **ภาพ + ข้อความ + click_url** ให้ **Worker (moderator)** → Worker ตรวจ (ไฟล์, AI moderation, URL) แล้ว **เขียนผลลง chain เอง** · FE **ไม่ต้อง** เชื่อ Worker — chain คือความจริง
- FE ของเกม **อ่าน account ตรงๆ** (ไม่มี CPI, ไม่ต้องมี backend) · ภาพ pin ด้วย `image_sha256` บน chain
- เงิน: ผู้ชนะจ่ายเต็ม (ไม่คืนแม้โฆษณาไม่ผ่าน — D4) · หลัง epoch เริ่ม 99% → เจ้าของเกม · 1% → Treasury ของ platform (`ad_fee_bps` snapshot ตอน bid แรก)
- ผู้ลงโฆษณาใช้ **External Wallet เท่านั้น** (Phantom, Solflare …) จ่าย gas + rent เอง — **ไม่มี ix ไหนผ่าน relayer** (A16)

| | devnet |
|---|---|
| ad-pool program | `6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk` |
| platform-core program | `AJnPSYsn7J2muHfKztRtpZz5kBP94dWkNZLr9aMhu18G` |
| IDL | `ad-pool/idls/ad_pool.json` (+ `platform_core.json` สำหรับ `DefaultAd`) |
| เกมทดสอบ | `noop_game` `8pqRixeRTuSBUNit1d73oPwX7kneMtcKBKBCgKSvxod4` (ช่อง 0 = 728×90, ช่อง 1 = 300×250 · epoch 1 วัน) |
| Worker | ยังไม่ deploy · `POST /v1/creative`, `GET /ads/<sha>.<ext>` |

## 1. ใครใช้ส่วนไหน

| ผู้ใช้ | ทำอะไร | หัวข้อ |
|---|---|---|
| **FE ของเกม** (ทุกเกม) | แสดงโฆษณาในช่อง — อ่านอย่างเดียว | §3, §4 |
| **FE ผู้ลงโฆษณา** | ดูช่อง/ราคา · bid · ส่ง creative · ส่งภาพใหม่ | §3, §5, §6 |
| **เจ้าของเกม** (`GameEntry.owner` ใน platform-core) | สร้าง/แก้ config ช่องโฆษณา | §7 |
| Admin ของ platform | โฆษณาพื้นฐาน (`DefaultAd`) ผ่าน Worker | §8 |
| ไม่มีใคร (อัตโนมัติ) | settle / close — Worker crank ทุก 10 นาที | §9 |

## 2. PDA (TypeScript · `@solana/web3.js` 1.x)

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
- `game` = **program ID ของเกม** (ไม่ใช่ชื่อ · ไม่ใช่ wallet เจ้าของ)
- `epoch` เป็น **u64 little-endian** · `slot` เป็น **u8** — ผิดชนิด = ได้ address อื่นเงียบๆ

## 3. Account ที่ FE อ่าน (offset ตายตัว — decode เองได้โดยไม่ต้องใช้ Anchor)

> ทุก account: **ตรวจ `owner` ของ account = program ที่ถูก + ตรวจขนาด + discriminator ก่อนเชื่อ data** (ไม่งั้นใครก็ส่ง account ปลอมที่หน้าตาเหมือนกันมาได้) · discriminator อยู่ใน IDL (`accounts[].discriminator`)

**`GameAdConfig`** — `[b"ad_config", game]` · owner = ad-pool · **199 B**

| offset | field | type | |
|---:|---|---|---|
| 10 | `slot_count` | u8 | ใช้ได้ 0..slot_count |
| 11 | `game_program` | Pubkey | |
| 43 | `genesis_ts` | i64 | 00:00 UTC (หาร 86,400 ลงตัว) · เปลี่ยนไม่ได้ |
| 51 | `epoch_duration_secs` | i64 | ทวีคูณของ 1 วัน · ≤ 30 วัน · เปลี่ยนไม่ได้ |
| 59 | `auction_close_lead_secs` | i64 | ≥ 7 วัน · ปิดประมูลก่อน epoch เริ่มเท่านี้ |
| 67 | `min_bid` | u64 | lamports · bid แรก |
| 75 | `min_increment_bps` | u16 | |
| 77 | `bid_horizon_epochs` | u16 | เปิดให้ bid ล่วงหน้าได้ไกลสุดกี่ epoch |
| 79 | `slots[8]` | 5 B ต่อช่อง: `width` u16 · `height` u16 · `active` bool | |

**`AdSlotAuction`** — `[b"ad_slot", game, epoch u64 LE, slot u8]` · owner = ad-pool · **779 B** · **ไม่มี account = ยังไม่มีใคร bid**

| offset | field | type | |
|---:|---|---|---|
| 10 | `creative_status` | u8 | **0 Empty · 1 TextOnly · 2 Approved** |
| 11 | `settled` | bool | |
| 53 / 55 | `width` / `height` | u16 | snapshot ของช่อง |
| 57 | `close_ts` | i64 | ปิดประมูล |
| 65 | `epoch_start_ts` | i64 | |
| 73 | `epoch_end_ts` | i64 | |
| 83 | `min_increment_bps` | u16 | snapshot |
| 85 | `rent_payer` | Pubkey | bidder คนแรก (ได้ rent คืนตอนปิด) |
| 117 | `highest_bidder` | Pubkey | ผู้นำ / ผู้ชนะ |
| 149 | `highest_bid` | u64 | lamports |
| 157 | `bid_count` | u32 | |
| 161 | `reject_count` | u16 | ถูก reject / revoke กี่ครั้ง |
| 163 | `last_reject_flags` | u32 | เหตุผลล่าสุด (bit ดู §6.5) |
| 215 | `creative` | `image_url` string · `image_sha256` [32] · `text` string · `click_url` string | string = u32 LE length + UTF-8 bytes |

**`DefaultAd`** (platform-core) — `[b"default_ad", w u16 LE, h u16 LE]` · owner = platform-core · **651 B** · `enabled` @14 (bool) · `creative` @55 (layout เดียวกัน) · ไม่มี account / `enabled = false` / decode ไม่ได้ → **ไม่แสดงอะไร**

```ts
function readCreative(d: Buffer, off: number) {
  const str = () => { const n = d.readUInt32LE(off); const s = d.subarray(off + 4, off + 4 + n).toString('utf8'); off += 4 + n; return s; };
  const image_url = str(); const image_sha256 = d.subarray(off, off + 32); off += 32;
  const text = str(); const click_url = str();
  return { image_url, image_sha256, text, click_url };
}
```

## 4. แสดงโฆษณาในเกม (FE ของเกม)

### 4.1 epoch ปัจจุบัน + phase (คำนวณเอง — สถานะไม่เปลี่ยนเองตามเวลา)
```ts
const now = Math.floor(Date.now() / 1000);              // ใช้นาฬิกา client ได้ (หน่วยวัน — drift ไม่มีผล)
const epoch = now < genesis ? null : BigInt(Math.floor((now - genesis) / duration));
// ของ auction: now < close_ts → Bidding · < epoch_start → CreativeWindow · < epoch_end → Live · ไม่งั้น Ended
```

### 4.2 ลำดับการเลือกสิ่งที่แสดง (ต่อช่อง)
1. อ่าน `GameAdConfig` + `AdSlotAuction(epoch ปัจจุบัน, slot)` + `DefaultAd(w, h)` ใน **`getMultipleAccounts` ครั้งเดียว**
2. auction มีอยู่ (owner/ขนาด/discriminator ถูก) **และ** `epoch_start_ts ≤ now < epoch_end_ts`:
   - `Approved` (2) → **ภาพ** + text + click_url — **ดาวน์โหลดภาพแล้ว `sha256(bytes) == image_sha256` ก่อนแสดง** · ไม่ตรง → ใช้แบบ TextOnly แทน
   - `TextOnly` (1) → **ข้อความ** + click_url แทนภาพ (A13 — ภาพยังไม่ผ่าน)
   - `Empty` (0) → ข้อ 3
3. `DefaultAd` ของขนาดนี้ `enabled` → แสดงแบบเดียวกับ `Approved` (platform-core บังคับแค่ `image_sha256 ≠ 0` — ถ้า `image_url` ว่างหรือ hash ไม่ตรง ให้แสดงข้อความแทน)
4. ไม่มีอะไรเลย → ไม่แสดงอะไร (หรือ placeholder ของเกมเอง)

### 4.3 กติกาความปลอดภัยตอน render (บังคับ)
- **`text` = plain text เท่านั้น** (`textContent` ไม่ใช่ `innerHTML`) · เป็น UTF-8 ≤ 120 **byte** (ไทย ≈ 40 ตัวอักษร)
- **ภาพ**: ตรวจ origin ของ `image_url` ว่าเป็น Worker ของ platform (allowlist) **และ** ตรวจ sha256 · on-chain ตรวจแค่ความยาว · ไม่ต้องเชื่อ content-type
- **click_url (ชั้น 3 ของ A14)**: Worker ตรวจตอน approve และตรวจซ้ำทุก ~10 นาทีแล้ว แต่ปลายทางเปลี่ยนได้ → **แสดงคำเตือนก่อนออกจากเกมเสมอ** ("กำลังออกไปที่ `<host>`") · เปิดด้วย `rel="noopener noreferrer"` · **ห้ามใส่ attribution ของ Google** ในคำเตือน (เงื่อนไข Web Risk)
- **ไม่ต้องตรวจซ้ำว่าใครชนะ / จ่ายแล้วหรือยัง** — status บน chain เขียนได้แค่ Worker (role Moderator) เท่านั้น

### 4.4 Cache
- `Approved` เป็นของถาวรใน epoch (ล็อกด้วย sha256 — Q4) · แต่ Worker **revoke** ได้ (กลับเป็น `Empty`) → poll ใหม่ทุก 1–5 นาที หรือ `onAccountChange`
- ภาพ: `GET /ads/<sha256>.<ext>` เป็น immutable (cache ได้ตลอด · มี ETag)

## 5. ประมูล (FE ผู้ลงโฆษณา · External Wallet)

### 5.1 bid แรก vs bid ต่อ
- ยังไม่มี `AdSlotAuction` → **`open_auction`** (สร้าง account · bidder จ่าย rent ≈ 0.0046 SOL — ได้คืนตอนปิด)
- มีแล้ว → **`bid`** · ถ้าสองคน `open_auction` พร้อมกัน คนหลัง fail (account มีแล้ว) → อ่านใหม่แล้วใช้ `bid`

### 5.2 ขั้นต่ำ
- bid แรก: `amount ≥ max(GameAdConfig.min_bid, rent(779 B))` (devnet: rent = 4,607,560 lamports)
- bid ต่อ: `amount ≥ highest_bid + max(1, ceil(highest_bid × min_increment_bps / 10,000))` (ใช้ `min_increment_bps` ของ **auction**)
- ผู้นำเพิ่มเงินตัวเองได้ (`prev_bidder` = ตัวเอง · โอนแค่ส่วนต่าง)

### 5.3 เวลา
- เปิดได้เมื่อ `now < close_ts` **และ** `epoch_start ≤ now + bid_horizon_epochs × duration` · `close_ts = epoch_start − auction_close_lead_secs` (≥ 7 วัน)
- เกมต้อง `Active` ใน platform-core ทุก bid (Q2) · ช่องต้อง `active`

### 5.4 Instruction (ลำดับ account ตรงตาม IDL · ตัวอย่างจริง `scripts/devnet/smoke.ts`)

| ix | discriminator | data | accounts (signer · writable) |
|---|---|---|---|
| `open_auction` | `[48,60,204,12,175,130,173,33]` | game Pubkey · epoch u64 · slot_index u8 · amount u64 | bidder (s,w) · `gameEntry(game)` · `pcConfig()` · `adConfig(game)` · `adSlot(game,epoch,slot)` (w) · SystemProgram |
| `bid` | `[199,56,85,38,146,243,37,158]` | amount u64 | bidder (s,w) · auction (w) · `gameEntry(auction.game_program)` · **prev_bidder = `auction.highest_bidder`** (w) · SystemProgram |

- **คืนเงินคนเก่าอยู่ใน tx เดียวกัน (A3)** — ต้องแนบ `prev_bidder` ตรงกับที่อ่านมา · ถ้ามีคนแซงก่อน tx ลง → error **`LeaderChanged` (6017)** → **อ่าน auction ใหม่ แล้วสร้าง tx ใหม่** (ไม่ใช่ retry ตัวเดิม)
- ใช้ Anchor TS ก็ได้: **`@anchor-lang/core` 1.2.1** + `idls/ad_pool.json` (⚠️ ห้ามใช้ `@coral-xyz/anchor` — ค้างที่ 0.32 ใช้กับ Anchor 1.x ไม่ได้)

## 6. ส่ง creative ให้ Worker (ผู้ชนะ · หลังปิดประมูล)

### 6.1 เมื่อไร
- ได้ใน `[close_ts, epoch_end_ts)` (รวมช่วงที่ epoch กำลังแสดง — ผ่านแล้วขึ้นจอทันที)
- **full** (ภาพ + text + click_url) เมื่อ status = `Empty` · **image-only** (ส่งแค่ภาพใหม่) เมื่อ status = `TextOnly` · `Approved` แล้ว = ล็อก เปลี่ยนไม่ได้

### 6.2 ข้อความที่ให้ wallet เซ็น (`signMessage` · UTF-8 · `\n` · **ไม่มี newline ท้าย**)
```text
ad-pool creative submission
domain: <AUTH_DOMAIN ของ Worker>
cluster: devnet
program: 6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk
game: <game program>
epoch: <epoch>
slot: <slot>
payload: <sha256 hex ตัวเล็ก>
nonce: <32 hex ตัวเล็ก — สุ่มใหม่ทุกครั้ง>
expires: <unix seconds · > now และ ≤ now + 600>
```
```ts
// payload = sha256("ad-pool/v1" ‖ sha256(image) ‖ u32le(len text) ‖ text ‖ u32le(len click_url) ‖ click_url)
// image-only: text = "" และ click_url = "" · len คือจำนวน BYTE (UTF-8) ไม่ใช่จำนวนตัวอักษร
const sig = await wallet.signMessage(new TextEncoder().encode(message));   // → base58 ของ 64 byte
```
- เลขฐานสิบรูปแบบมาตรฐาน (ไม่มี 0 นำหน้า) · **เซ็น message เท่านั้น ห้ามเซ็น transaction** · Worker สร้างข้อความเองจากค่าที่ส่ง — ถ้าเนื้อหาต่างไปแม้ 1 byte = 401

### 6.3 `POST /v1/creative` (`multipart/form-data` · ต้องมี `Content-Length`)
fields: `game` · `epoch` · `slot` · `signer` (pubkey ของผู้ชนะ) · `nonce` · `expires` · `signature` · `image` (file) · `text` · `click_url` (image-only: ไม่ส่ง text/click_url)

- **ภาพ**: PNG / JPEG / WebP (ดูจาก magic bytes) · **W×H ตรงช่องเป๊ะ** · ≤ 1 MiB · ไม่เคลื่อนไหว · ไม่มี byte ต่อท้าย · EXIF ได้ถ้าไม่หมุนภาพและ**ไม่มี GPS** (strip ก่อนส่ง)
- **text**: 1–120 byte · ไม่มี control / ตัวอักษรล่องหน / bidi override
- **click_url**: `https://` · ไม่มี IP / port / user:pass / punycode / shortener / ชื่อภายใน · ≤ 200 byte · ที่ขึ้น chain = รูป normalize (เช่น host ตัวเล็ก + `/` ท้าย)

### 6.4 คำตอบ
| HTTP | ความหมาย | FE ทำอะไร |
|---|---|---|
| 200 `approved` / `text_only` / `image_approved` | ขึ้น chain แล้ว (มี `signature`) | แสดงผล · `text_only` = ภาพไม่ผ่าน (ดู `reasons`) → ให้ส่งภาพใหม่แบบ image-only |
| 422 `rejected` | ไม่ผ่าน · บันทึกเหตุผลบน chain (`flags`, `reasons`) | แก้แล้วส่งใหม่ (สถานะไม่เปลี่ยน) |
| 202 `confirmed:false` | ส่ง tx แล้วแต่ยังไม่ confirm | อ่าน auction ซ้ำ |
| 400 / 401 | ข้อมูลผิด / ลายเซ็นไม่ผ่าน | แก้ที่ client |
| 403 `NotWinner` · 409 `NotInCreativeWindow` / `WrongKind` / `AlreadyApproved` | ผิดสิทธิ์ / ผิดเวลา / ผิดชนิด | |
| 409 `InFlight` | มี submission ของช่องนี้กำลังทำ | **ส่งคำขอเดิมซ้ำได้** หลัง `retry_after_secs` (nonce ยังไม่ถูกใช้) |
| 409 `Replay` · 429 `RateLimited` | nonce ซ้ำ · เกิน 10 ครั้ง/ชม./ช่อง | เซ็นใหม่ / รอ |
| 503 / 502 | ตรวจไม่ได้ชั่วคราว (fail closed) / RPC | **เซ็นใหม่ด้วย nonce ใหม่** แล้วลองอีกครั้ง |

### 6.5 bit ของ `flags` / `last_reject_flags`
`1` ไฟล์ · `2` W×H · `4` ภาพไม่ผ่าน moderation · `8` ข้อความไม่ผ่าน · `16` click_url ชั้น 1 · `32` click_url ชั้น 2 (Web Risk) · `64` ยาวเกิน

## 7. เจ้าของเกม: ตั้งค่าช่องโฆษณา

ต้องเซ็นโดย **`GameEntry.owner`** และเกม `Active` (ตอน init) · ix อยู่ใน IDL (`init_ad_config`, `update_ad_config`, `add_slot`, `set_slot_active`)

| field | กติกา (program บังคับ) |
|---|---|
| `genesis_ts` | ≥ 0 · **หาร 86,400 ลงตัว** (00:00 UTC = 07:00 ICT) · ≤ now + 90 วัน · **แก้ไม่ได้อีก** |
| `epoch_duration_secs` | ทวีคูณของ 86,400 · ≤ 30 วัน · **แก้ไม่ได้อีก** |
| `auction_close_lead_secs` | **≥ 7 × 86,400** (ไม่จำเป็นต้องลงตัวที่วัน) |
| `min_bid` | ≥ rent(779 B) |
| `min_increment_bps` | 1..=10,000 |
| `bid_horizon_epochs` | `× duration ≤ 90 วัน` และ `≥ lead + 1 epoch` |
| `slots` | 1..=8 ช่อง · W, H > 0 · **เพิ่มได้อย่างเดียว ลบ/แก้ขนาดไม่ได้** · เลิกขาย = `set_slot_active(false)` |

- `update_ad_config` (ทุก field เป็น Option) มีผลกับ **auction ใหม่** เท่านั้น — auction ที่เปิดแล้ว snapshot ค่าไว้
- ตั้ง `genesis_ts` / duration ผิด = config นั้นใช้ไม่ได้ถาวร (ไม่มี re-init) → ตรวจก่อนเซ็น
- ตัวอย่างจริง: `scripts/devnet/smoke.ts` (`init_ad_config`)

## 8. โฆษณาพื้นฐาน (Admin ของ platform)
- `POST /v1/default-ad` บน Worker · ผู้ส่ง = wallet ที่มี `RoleMember(Admin)` ของ platform-core เท่านั้น · ผ่านการตรวจแบบเดียวกับ §6 ทุกขั้น (ต้องผ่านครบ — ไม่มี text-only)
- ข้อความเซ็นขึ้นต้นด้วย `ad-pool default ad submission` และมี `program: <platform-core>`, `width`, `height` แทน game/epoch/slot (รายละเอียด doc 04)
- เปิด/ปิด (`set_default_ad_enabled`) → Admin เซ็นเองจาก wallet ตรงกับ platform-core (Worker ไม่มี endpoint นี้)

## 9. settle / close (ไม่ต้องทำอะไร)
- Worker crank (ทุก 10 นาที) ส่ง `settle` หลัง `epoch_start_ts` (เงิน → เจ้าของเกม 99% / Treasury 1%) และ `close_auction` หลัง `epoch_end_ts` (rent → `rent_payer`) · ทั้งคู่ **permissionless** — ใครกดแทนก็ได้ (เงินไปที่ถูกต้องเสมอ)

## 10. Error code ที่ FE เจอบ่อย (ทั้งหมดใน IDL `errors`)

| code | name | ทำอะไร |
|---:|---|---|
| 6000 | `GameNotActive` | เกมถูกพัก — ห้าม bid |
| 6008 / 6009 | `MinBidBelowFloor` / `BidBelowFloor` | ต่ำกว่าค่า rent ของ account |
| 6011 / 6012 | `InvalidSlot` / `SlotInactive` | ช่องไม่มี / ปิดขาย |
| 6014 | `BiddingClosed` | เลย `close_ts` แล้ว |
| 6015 | `BeyondHorizon` | epoch ไกลเกินกว่าที่เปิดให้ bid |
| 6016 | `BidTooLow` | ต่ำกว่า min_bid หรือ increment ขั้นต่ำ |
| 6017 | `LeaderChanged` | มีคนแซง → อ่านใหม่ + สร้าง tx ใหม่ |
| 6001 / 6002–6006 / 6010 / 6013 | config | ดู §7 |

## 11. Checklist ก่อนขึ้นจริง
- [ ] ตรวจ owner + ขนาด + discriminator ของทุก account ก่อนอ่าน
- [ ] `epoch` เป็น u64 LE · `slot` เป็น u8 ใน seed
- [ ] ภาพ: origin allowlist + `sha256 == image_sha256` · text เป็น plain text · click_url มีคำเตือนก่อนออก
- [ ] fallback ครบ: Approved → TextOnly → DefaultAd(enabled) → ว่าง
- [ ] bid: แนบ `prev_bidder` = `highest_bidder` ที่เพิ่งอ่าน · จัดการ `LeaderChanged`
- [ ] creative: ข้อความเซ็นตรง template ทุก byte · nonce ใหม่ทุกครั้ง · ส่ง `Content-Length`
- [ ] ใช้ External Wallet · ไม่ส่ง ix ของ ad-pool ผ่าน relayer
- [ ] ⚠️ ข้อความที่ wallet เซ็นแสดงแค่ hash → FE ควรแสดง text / click_url / ภาพให้ผู้ใช้เห็นชัดก่อนกดเซ็น (หน้าเว็บปลอมอาจหลอกให้เซ็น creative ของคนอื่น · แนวทาง SIWS กำลังพิจารณา)
