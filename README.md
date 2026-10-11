# ad-pool

ส่วนหนึ่งของ **blockchain-games-hub** — พื้นที่โฆษณาของแต่ละเกมบน Solana

- **Anchor program**: กำหนดช่องโฆษณา (W×H) ต่อเกม · ประมูลด้วย **SOL** ต่อ (epoch, slot) · คืนเงินคนแพ้ใน tx เดียว · settle → เจ้าของเกม (หัก 1% เข้า Treasury ของ platform-core)
- **Cloudflare Worker (moderator)**: ตรวจผู้ชนะด้วยลายเซ็น wallet · ตรวจ W×H · AI moderation ภาพ + ข้อความ · ตรวจ `click_url` · เก็บ **R2** (key = sha256) · approve/reject บน chain
- FE ของเกมอ่าน account ตรงๆ — เกมไม่ต้อง CPI

| | |
|---|---|
| Specs | [`specs/init.md`](specs/init.md) (maintainer — ต้นฉบับ) · [`specs/spec-v1.md`](specs/spec-v1.md) (ภาพรวม) |
| **Design** | [`specs/designs/01-program-design-v1.md`](specs/designs/01-program-design-v1.md) — accounts, state machine, instructions (ผ่าน review · อนุมัติ 2026-10-09) |
| **Plan / tracking** | [`specs/designs/02-implementation-plan.md`](specs/designs/02-implementation-plan.md) — phase P0–P5 (program) · W0–W6 (Worker) · F1 (FE) |
| **ต่อ FE** | [`SKILL.md`](SKILL.md) — แสดงโฆษณาในเกม · bid · ส่ง creative · ตั้งค่าช่องโฆษณา (สำหรับทีม FE / SDK) |
| Worker API | [`specs/designs/04-worker-api-v1.md`](specs/designs/04-worker-api-v1.md) — `POST /v1/creative` |
| **Devnet** | program `6T2vwUm5EPBAUQaZNcPwEkgK4rHc9AjoET6DaTq3LEqk` · slot 509493617 · upgrade authority `HRstpEJN…MsTvy` — [`specs/deploy/devnet-runbook.md`](specs/deploy/devnet-runbook.md) §4 |
| Research | [`specs/research/`](specs/research/) — A9 moderation · A14 URL reputation |
| Decisions | [`../DECISIONS.md`](../DECISIONS.md) (A1–A14, D3–D5, D11 · rev 8) |
| Integration | platform-core [`03-consumer-integration-v1.md`](../platform-core/specs/designs/03-consumer-integration-v1.md) · relayer `solana-poc/relayer/skills/gasless-relayer-integration/SKILL.md` |
| Owner | `maintainer` (`<internal>`) |

สถานะ (2026-10-10): design v1 อนุมัติแล้ว (External Wallet เท่านั้น) · program P0–P4 ✅ · **deploy devnet ✅ (P5)** · Worker W0–W1 ✅ · W2 กำลังทำ · devnet เท่านั้น

เวลา spec ขัดกัน ยึดตามลำดับนี้: `DECISIONS.md` → `designs/01-program-design-v1.md` → `spec-v1.md` (⭐ rev 3) → `spec-v1.md` เนื้อหาเดิม

spec เวอร์ชันเก่าเก็บใน `specs/archive/`
