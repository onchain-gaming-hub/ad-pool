// ad-pool — devnet P5 setup + smoke (runbook §3). Idempotent: every step checks chain state first.
// Usage: bun run smoke.ts <admin-keypair.json> <advertiser-keypair.json> <moderator-pubkey> <game-program>
//   env RPC_URL (default public devnet) · CLOSE_IN_SECS (default 1800) · DRY=1 (print plan only)
// Steps: pc register_game (admin = owner) → pc grant_role(Moderator) → init_ad_config →
//        open_auction (admin bids first) → bid (advertiser outbids → admin refunded in the same tx, A3).
// The test config picks `auction_close_lead_secs` so the smoke epoch closes CLOSE_IN_SECS from now:
// the Worker can then submit a creative today instead of waiting ≥ 1 day (D5 lead stays ≥ 7 days).
// Refuses to run on anything but devnet (genesis-hash check). Instructions are built from the IDLs.
import {
  Connection, Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction,
  TransactionInstruction, sendAndConfirmTransaction,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";

const DEVNET_GENESIS = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
const DAY = 86_400;

const load = (p: string) => JSON.parse(readFileSync(new URL(p, import.meta.url), "utf8"));
const ap = load("../../idls/ad_pool.json");
const pc = load("../../../platform-core/idls/platform_core.json");
const AP = new PublicKey(ap.address);
const PC = new PublicKey(pc.address);

const [adminPath, advPath, moderatorArg, gameArg] = process.argv.slice(2);
if (!gameArg) throw new Error("usage: bun run smoke.ts <admin.json> <advertiser.json> <moderator> <game_program>");
const kp = (p: string) => Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(p, "utf8"))));
const admin = kp(adminPath);
const adv = kp(advPath);
const moderator = new PublicKey(moderatorArg);
const game = new PublicKey(gameArg);
const dry = process.env.DRY === "1";
const closeIn = Number(process.env.CLOSE_IN_SECS ?? 1800);

const conn = new Connection(process.env.RPC_URL ?? "https://api.devnet.solana.com", "confirmed");
if ((await conn.getGenesisHash()) !== DEVNET_GENESIS) throw new Error("refusing: RPC is not devnet");

const pda = (prog: PublicKey, ...s: (Buffer | Uint8Array)[]) => PublicKey.findProgramAddressSync(s, prog)[0];
const b = (s: string) => Buffer.from(s);
const u8 = (n: number) => Buffer.from([n]);
const u16 = (n: number) => { const x = Buffer.alloc(2); x.writeUInt16LE(n); return x; };
const u32 = (n: number) => { const x = Buffer.alloc(4); x.writeUInt32LE(n); return x; };
const u64 = (n: bigint | number) => { const x = Buffer.alloc(8); x.writeBigUInt64LE(BigInt(n)); return x; };
const i64 = (n: bigint | number) => { const x = Buffer.alloc(8); x.writeBigInt64LE(BigInt(n)); return x; };

const MODERATOR_ROLE = pc.types.find((t: any) => t.name === "Role").type.variants
  .findIndex((v: any) => v.name === "Moderator");
const ADMIN_ROLE = 0;
const pcConfig = pda(PC, b("config"));
const adminRole = pda(PC, b("role"), u8(ADMIN_ROLE), admin.publicKey.toBuffer());
const modRole = pda(PC, b("role"), u8(MODERATOR_ROLE), moderator.toBuffer());
const gameEntry = pda(PC, b("game"), game.toBuffer());
const adConfig = pda(AP, b("ad_config"), game.toBuffer());

function ix(idl: any, prog: PublicKey, name: string, accts: Record<string, PublicKey>, args: Buffer[]) {
  const d = idl.instructions.find((i: any) => i.name === name);
  const keys = d.accounts.map((a: any) => {
    if (!accts[a.name]) throw new Error(`${name}: account ${a.name} not provided — IDL changed?`);
    return { pubkey: accts[a.name], isSigner: !!a.signer, isWritable: !!a.writable };
  });
  return new TransactionInstruction({ programId: prog, keys, data: Buffer.concat([Buffer.from(d.discriminator), ...args]) });
}
async function send(label: string, i: TransactionInstruction, signers: Keypair[]) {
  if (dry) { console.log(`DRY ${label}`); return; }
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(i), signers, { commitment: "confirmed" });
  const tx = await conn.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
  console.log(`✅ ${label}: ${sig} · cu ${tx?.meta?.computeUnitsConsumed}`);
}
const exists = async (k: PublicKey) => !!(await conn.getAccountInfo(k));

console.log({ admin: admin.publicKey.toBase58(), advertiser: adv.publicKey.toBase58(),
  moderator: moderator.toBase58(), game: game.toBase58(), moderatorRole: MODERATOR_ROLE });

// 1. platform-core: register the test game (owner = admin, co-signs)
if (await exists(gameEntry)) console.log("game_entry exists — skip register_game");
else await send("pc register_game", ix(pc, PC, "register_game", {
  signer: admin.publicKey, auth_role: adminRole, owner: admin.publicKey, game_program: game,
  game_entry: gameEntry, system_program: SystemProgram.programId,
}, [u8(ADMIN_ROLE)]), [admin]);

// 2. platform-core: Moderator role for the Worker key
if (await exists(modRole)) console.log("moderator RoleMember exists — skip grant_role");
else await send("pc grant_role(Moderator)", ix(pc, PC, "grant_role", {
  admin: admin.publicKey, admin_role: adminRole, config: pcConfig, role_member: modRole,
  system_program: SystemProgram.programId,
}, [u8(MODERATOR_ROLE), moderator.toBuffer()]), [admin]);

// 3. ad-pool: GameAdConfig — 1-day epochs, genesis = today 00:00 UTC
const now = Math.floor(Date.now() / 1000);
const SMOKE_EPOCH = 8;
let genesis: number, lead: number;
const cfgInfo = await conn.getAccountInfo(adConfig);
if (cfgInfo) {
  // GameAdConfig: disc 8 · version · bump · slot_count · game 32 → genesis_ts @43, epoch_duration @51, lead @59
  genesis = Number(cfgInfo.data.readBigInt64LE(43));
  lead = Number(cfgInfo.data.readBigInt64LE(59));
  console.log(`ad_config exists — genesis ${genesis} lead ${lead}`);
} else {
  genesis = Math.floor(now / DAY) * DAY;
  const start = genesis + SMOKE_EPOCH * DAY;
  lead = start - (now + closeIn);
  if (lead < 7 * DAY) throw new Error(`lead ${lead} < 7 days`);
  const minBid = 0.01 * LAMPORTS_PER_SOL;
  await send("init_ad_config", ix(ap, AP, "init_ad_config", {
    game_owner: admin.publicKey, game_entry: gameEntry, ad_config: adConfig,
    system_program: SystemProgram.programId,
  }, [game.toBuffer(), i64(genesis), i64(DAY), i64(lead), u64(minBid), u16(1000), u16(30),
      u32(2), u16(728), u16(90), u16(300), u16(250)]), [admin]);
}

// 4. open_auction (admin = first bidder, pays rent) → 5. advertiser outbids
const auction = pda(AP, b("ad_slot"), game.toBuffer(), u64(SMOKE_EPOCH), u8(0));
let a = await conn.getAccountInfo(auction);
if (!a) {
  await send(`open_auction epoch ${SMOKE_EPOCH} slot 0`, ix(ap, AP, "open_auction", {
    bidder: admin.publicKey, game_entry: gameEntry, config: pcConfig, ad_config: adConfig,
    auction, system_program: SystemProgram.programId,
  }, [game.toBuffer(), u64(SMOKE_EPOCH), u8(0), u64(0.01 * LAMPORTS_PER_SOL)]), [admin]);
  a = await conn.getAccountInfo(auction);
}
if (a && !dry) {
  const highest = new PublicKey(a.data.subarray(117, 149));
  if (!highest.equals(adv.publicKey)) {
    const bal = await conn.getBalance(highest);
    await send("bid (advertiser outbids, refund in same tx)", ix(ap, AP, "bid", {
      bidder: adv.publicKey, auction, game_entry: gameEntry, prev_bidder: highest,
      system_program: SystemProgram.programId,
    }, [u64(0.02 * LAMPORTS_PER_SOL)]), [adv]);
    console.log(`prev bidder refund: +${(await conn.getBalance(highest)) - bal} lamports`);
  } else console.log("advertiser already highest — skip bid");
  a = (await conn.getAccountInfo(auction))!;
}

// --- verify ------------------------------------------------------------------------
if (!dry) {
  const d = a!.data;
  const disc = (n: string) => Buffer.from(ap.accounts.find((x: any) => x.name === n).discriminator);
  const st = {
    auction: auction.toBase58(), owner: a!.owner.toBase58(), size: d.length,
    status: d[10], settled: d[11], epoch: d.readBigUInt64LE(45), w: d.readUInt16LE(53), h: d.readUInt16LE(55),
    close_ts: Number(d.readBigInt64LE(57)), start: Number(d.readBigInt64LE(65)), end: Number(d.readBigInt64LE(73)),
    fee_bps: d.readUInt16LE(81), highest_bidder: new PublicKey(d.subarray(117, 149)).toBase58(),
    highest_bid: d.readBigUInt64LE(149), bid_count: d.readUInt32LE(157),
  };
  console.log(st, `close in ${st.close_ts - Math.floor(Date.now() / 1000)} s`);
  const checks: [string, boolean][] = [
    ["owner = ad-pool", a!.owner.equals(AP)],
    ["size 779", d.length === 779],
    ["discriminator AdSlotAuction", d.subarray(0, 8).equals(disc("AdSlotAuction"))],
    ["highest = advertiser", st.highest_bidder === adv.publicKey.toBase58()],
    ["bid_count 2", st.bid_count === 2],
    ["status Empty", st.status === 0],
    ["window", st.close_ts === st.start - lead && st.end === st.start + DAY],
  ];
  for (const [n, ok] of checks) console.log(ok ? "✅" : "❌", n);
  if (checks.some(([, ok]) => !ok)) process.exit(1);
}
