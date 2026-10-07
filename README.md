# ad-pool

ส่วนหนึ่งของ **blockchain-games-hub** — พื้นที่โฆษณาของแต่ละเกมบน Solana

- **Anchor program**: กำหนดช่องโฆษณา (W×H) ต่อเกม · ประมูลด้วย **SOL** ต่อ (epoch, slot) · คืนเงินคนแพ้ใน tx เดียว · settle → เจ้าของเกม (หัก 5% เข้า Treasury ของ platform-core)
- **Cloudflare Worker (moderator)**: ตรวจผู้ชนะด้วยลายเซ็น wallet · ตรวจ W×H · AI moderation ภาพ + ข้อความ · ตรวจ `click_url` · เก็บ **R2** (key = sha256) · approve/reject บน chain
- FE ของเกมอ่าน account ตรงๆ — เกมไม่ต้อง CPI

| | |
|---|---|
| Specs | [`specs/init.md`](specs/init.md) (maintainer) · [`specs/spec-v1.md`](specs/spec-v1.md) |
| Decisions | [`../DECISIONS.md`](../DECISIONS.md) (A1–A14) |
| Owner | `maintainer` (`<internal>`) |

สถานะ: spec draft — ยังไม่มี code · devnet เท่านั้น
