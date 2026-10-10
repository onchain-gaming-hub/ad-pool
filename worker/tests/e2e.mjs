// W5 e2e: the real Worker on `wrangler dev` against an in-process mock of Solana RPC, OpenAI
// moderations and Web Risk. No dependencies (Node ≥ 18). Never touches devnet or any real API:
// wrangler runs from a temp dir (no .dev.vars), every secret is a throwaway, and upstream URLs
// point at 127.0.0.1 (the only override config.rs accepts).
//   cd worker && node tests/e2e.mjs
// Real network use: the Worker fetches `https://example.com/` while walking a click_url in
// the tests that pass the URL checks (offline, those fail as 503 click_url_unreachable).
// Not covered here (unit tests only): multi-hop redirect walks (no reachable redirector —
// layer 1 refuses IP / localhost hosts) and RPC-URL redaction.
// The mock RPC applies moderator txs to its accounts (status byte) and answers
// getSignatureStatuses per signature, so re-reads and lock behavior see real state changes.
//
// The client side here is written from specs/designs/04-worker-api-v1.md (message template,
// payload hash, multipart fields), not from the Worker's code — so doc drift fails a test.
import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const WORKER = path.resolve(HERE, '..');
const IDL = JSON.parse(fs.readFileSync(path.join(WORKER, '../idls/ad_pool.json'), 'utf8'));
const PC_IDL = JSON.parse(fs.readFileSync(path.join(WORKER, '../idls/platform_core.json'), 'utf8'));
const MOCK_PORT = 8899;
const PORT = 8790;
const MOCK = `http://127.0.0.1:${MOCK_PORT}`;
const W = `http://127.0.0.1:${PORT}`;
const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const MAINNET = '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
const DOMAIN = 'ad-pool.devnet.local';
const OPENAI_KEY = 'test-openai-key';
const WEBRISK_KEY = 'test-webrisk-key';

// ------------------------------------------------------------------ base58 / keys
const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function b58(buf) {
  let n = BigInt('0x' + (Buffer.from(buf).toString('hex') || '0')), out = '';
  while (n > 0n) { out = B58[Number(n % 58n)] + out; n /= 58n; }
  for (const b of buf) { if (b !== 0) break; out = '1' + out; }
  return out;
}
function unb58(s) {
  let n = 0n;
  for (const c of s) n = n * 58n + BigInt(B58.indexOf(c));
  let hex = n.toString(16); if (hex.length % 2) hex = '0' + hex;
  const body = n === 0n ? Buffer.alloc(0) : Buffer.from(hex, 'hex');
  return Buffer.concat([Buffer.alloc(s.match(/^1*/)[0].length), body]);
}
function keypair() {
  const { privateKey, publicKey } = crypto.generateKeyPairSync('ed25519');
  const jwk = privateKey.export({ format: 'jwk' });
  const seed = Buffer.from(jwk.d, 'base64url'), pub = Buffer.from(jwk.x, 'base64url');
  return { privateKey, publicKey, pub: b58(pub), secret58: b58(Buffer.concat([seed, pub])) };
}
const sign = (kp, msg) => b58(crypto.sign(null, Buffer.from(msg, 'utf8'), kp.privateKey));
const pubKeyObj = (raw) => crypto.createPublicKey({
  key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]), format: 'der', type: 'spki',
});
const sha256 = (b) => crypto.createHash('sha256').update(b).digest();
const disc = (idl, name) => Buffer.from(idl.instructions.find((i) => i.name === name).discriminator);

// ------------------------------------------------------------------ mock upstream
const state = {};
const stats = {};
function resetMock() {
  Object.assign(state, {
    genesis: DEVNET, accounts: {}, send: 'ok', sendDelayMs: 0, status: 'confirmed',
    openai: 'ok', flagText: [], flagImageSha: [], webrisk: 'ok', threats: [], flipOnModeration: null, sigs: new Set(),
  });
  Object.assign(stats, { sent: [], moderations: [], webrisk: [], gpa: 0, rpc: {} });
}
resetMock();
const readBody = (req) => new Promise((ok) => { const c = []; req.on('data', (d) => c.push(d)); req.on('end', () => ok(Buffer.concat(c))); });
const json = (res, status, obj) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(obj)); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const CATS = ['harassment', 'harassment/threatening', 'hate', 'hate/threatening', 'illicit', 'illicit/violent',
  'self-harm', 'self-harm/intent', 'self-harm/instructions', 'sexual', 'sexual/minors', 'violence', 'violence/graphic'];
function moderationResult(flagged) {
  const categories = Object.fromEntries(CATS.map((c) => [c, flagged && c === 'violence']));
  const category_scores = Object.fromEntries(CATS.map((c) => [c, flagged && c === 'violence' ? 0.97 : 0.0001]));
  return { id: 'modr-e2e', model: 'omni-moderation-latest', results: [{ flagged, categories, category_scores, category_applied_input_types: {} }] };
}

const mock = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  const body = await readBody(req);
  if (url.pathname === '/v1/moderations') {
    if (req.headers.authorization !== `Bearer ${OPENAI_KEY}`) return json(res, 401, {});
    const b = JSON.parse(body);
    const isImage = Array.isArray(b.input);
    let flagged = false, kind = 'text';
    if (isImage) {
      kind = 'image';
      const dataUrl = b.input[0].image_url.url;
      const bytes = Buffer.from(dataUrl.split(',')[1], 'base64');
      flagged = state.flagImageSha.includes(sha256(bytes).toString('hex'));
      stats.moderations.push({ kind, model: b.model, mime: dataUrl.slice(5, dataUrl.indexOf(';')) });
    } else {
      flagged = state.flagText.some((t) => b.input.includes(t));
      stats.moderations.push({ kind, model: b.model });
    }
    // E11: the auction changes on chain while moderation runs
    if (state.flipOnModeration) { setStatus(state.flipOnModeration, 2); state.flipOnModeration = null; }
    if (state.openai === '500') return json(res, 500, { error: 'mock' });
    return json(res, 200, moderationResult(flagged));
  }
  if (url.pathname === '/v1/uris:search') {
    if (req.headers['x-goog-api-key'] !== WEBRISK_KEY || url.searchParams.has('key')) return json(res, 400, { error: { details: [{ reason: 'API_KEY_INVALID' }] } });
    const uri = url.searchParams.get('uri');
    stats.webrisk.push(uri);
    if (state.webrisk === 'billing') return json(res, 403, { error: { code: 403, details: [{ reason: 'BILLING_DISABLED' }] } });
    if (state.threats.includes(uri)) {
      return json(res, 200, { threat: { threatTypes: ['MALWARE'], expireTime: new Date(Date.now() + 300_000).toISOString() } });
    }
    return json(res, 200, {});
  }
  if (req.method !== 'POST' || url.pathname !== '/') return json(res, 404, {});
  const rpc = JSON.parse(body);
  stats.rpc[rpc.method] = (stats.rpc[rpc.method] || 0) + 1;
  const ok = (result) => json(res, 200, { jsonrpc: '2.0', id: rpc.id, result });
  const info = (a) => a && { data: [a.data, 'base64'], executable: false, lamports: a.lamports, owner: a.owner, rentEpoch: 0, space: Buffer.from(a.data, 'base64').length };
  switch (rpc.method) {
    case 'getGenesisHash': return ok(state.genesis);
    case 'getAccountInfo': return ok({ context: { slot: 1 }, value: info(state.accounts[rpc.params[0]]) ?? null });
    case 'getProgramAccounts': {
      stats.gpa++;
      const [program, opts] = rpc.params;
      const size = opts.filters.find((f) => f.dataSize)?.dataSize;
      const mc = opts.filters.find((f) => f.memcmp)?.memcmp;
      const list = Object.entries(state.accounts).filter(([, a]) => {
        const d = Buffer.from(a.data, 'base64');
        return a.owner === program && d.length === size && d.subarray(mc.offset, mc.offset + 8).equals(unb58(mc.bytes));
      }).map(([pubkey, a]) => ({ pubkey, account: info(a) }));
      return ok(list);
    }
    case 'getLatestBlockhash': return ok({ context: { slot: 1 }, value: { blockhash: b58(crypto.randomBytes(32)), lastValidBlockHeight: 100 } });
    case 'sendTransaction': {
      if (state.sendDelayMs) await sleep(state.sendDelayMs);
      const raw = Buffer.from(rpc.params[0], 'base64');
      if (state.send === '429') return json(res, 429, {});
      if (state.send === 'unhealthy') return json(res, 200, { jsonrpc: '2.0', id: rpc.id, error: { code: -32005, message: 'Node is unhealthy' } });
      if (state.send === 'preflight6012') return json(res, 200, { jsonrpc: '2.0', id: rpc.id, error: { code: -32002, message: 'Transaction simulation failed: custom program error: 0x177c', data: { err: { InstructionError: [0, { Custom: 6012 }] } } } });
      const tx = decodeTx(raw);
      stats.sent.push(tx);
      state.sigs.add(tx.sig);
      applyTx(tx);
      return ok(tx.sig);
    }
    case 'getSignatureStatuses': {
      const known = state.sigs.has(rpc.params[0][0]);
      const v = (state.status === 'pending' || !known) ? null
        : state.status === 'failed' ? { slot: 1, err: { InstructionError: [0, { Custom: 6013 }] }, confirmationStatus: 'confirmed' }
          : { slot: 1, confirmations: 1, err: null, confirmationStatus: 'confirmed' };
      return ok({ context: { slot: 1 }, value: [v] });
    }
    default: return json(res, 200, { jsonrpc: '2.0', id: rpc.id, error: { code: -32601, message: 'mock: ' + rpc.method } });
  }
});

// AdSlotAuction.creative_status @10 (0 Empty · 1 TextOnly · 2 Approved)
function setStatus(addr, v) {
  const a = state.accounts[addr]; if (!a) return;
  const d = Buffer.from(a.data, 'base64'); d[10] = v; a.data = d.toString('base64');
}
function applyTx(tx) {
  for (const ix of tx.ixs) {
    if (ix.program !== IDL.address) continue;
    const d8 = ix.data.subarray(0, 8);
    if (d8.equals(disc(IDL, 'approve_creative'))) setStatus(ix.accounts[2], ix.data.readUInt32LE(8) === 0 ? 1 : 2);
    else if (d8.equals(disc(IDL, 'approve_image'))) setStatus(ix.accounts[2], 2);
    else if (d8.equals(disc(IDL, 'revoke_creative'))) setStatus(ix.accounts[2], 0);
  }
}

// legacy tx: [sigs][header 3][keys][blockhash][ixs] — verify every signature
function cu16(b, o) { let v = 0, s = 0, i = 0; for (;;) { const x = b[o + i++]; v |= (x & 0x7f) << s; if (!(x & 0x80)) break; s += 7; } return [v, o + i]; }
function decodeTx(raw) {
  let [n, o] = cu16(raw, 0);
  const sigs = []; for (let i = 0; i < n; i++) { sigs.push(raw.subarray(o, o + 64)); o += 64; }
  const msg = raw.subarray(o);
  const header = [msg[0], msg[1], msg[2]]; let p = 3;
  let k; [k, p] = cu16(msg, p);
  const keys = []; for (let i = 0; i < k; i++) { keys.push(b58(msg.subarray(p, p + 32))); p += 32; }
  p += 32; // blockhash
  let ni; [ni, p] = cu16(msg, p);
  const ixs = [];
  for (let i = 0; i < ni; i++) {
    const program = keys[msg[p++]];
    let na; [na, p] = cu16(msg, p);
    const accounts = [...msg.subarray(p, p + na)].map((x) => keys[x]); p += na;
    let nd; [nd, p] = cu16(msg, p);
    ixs.push({ program, accounts, data: Buffer.from(msg.subarray(p, p + nd)) }); p += nd;
  }
  const verified = sigs.every((s, i) => crypto.verify(null, msg, pubKeyObj(unb58(keys[i]).subarray(0, 32)), s));
  return { sig: b58(sigs[0]), numSigners: header[0], signers: keys.slice(0, header[0]), ixs, verified, size: raw.length };
}

// ------------------------------------------------------------------ client (from doc 04)
function payload(img, text, click) {
  const t = Buffer.from(text, 'utf8'), c = Buffer.from(click, 'utf8');
  const lt = Buffer.alloc(4); lt.writeUInt32LE(t.length); const lc = Buffer.alloc(4); lc.writeUInt32LE(c.length);
  return sha256(Buffer.concat([Buffer.from('ad-pool/v1'), sha256(img), lt, t, lc, c])).toString('hex');
}
const nonce = () => crypto.randomBytes(16).toString('hex');
const nowS = () => Math.floor(Date.now() / 1000);
function creativeMessage({ program, game, epoch, slot, pl, n, expires }) {
  return `ad-pool creative submission\ndomain: ${DOMAIN}\ncluster: devnet\nprogram: ${program}\ngame: ${game}\nepoch: ${epoch}\nslot: ${slot}\npayload: ${pl}\nnonce: ${n}\nexpires: ${expires}`;
}
function defaultAdMessage({ program, width, height, pl, n, expires }) {
  return `ad-pool default ad submission\ndomain: ${DOMAIN}\ncluster: devnet\nprogram: ${program}\nwidth: ${width}\nheight: ${height}\npayload: ${pl}\nnonce: ${n}\nexpires: ${expires}`;
}
function multipart(fields, image) {
  const boundary = '----e2e' + crypto.randomBytes(8).toString('hex');
  const parts = [];
  for (const [k, v] of Object.entries(fields)) {
    if (v === undefined) continue;
    parts.push(Buffer.from(`--${boundary}\r\ncontent-disposition: form-data; name="${k}"\r\n\r\n${v}\r\n`));
  }
  parts.push(Buffer.from(`--${boundary}\r\ncontent-disposition: form-data; name="image"; filename="ad"\r\ncontent-type: application/octet-stream\r\n\r\n`), image, Buffer.from(`\r\n--${boundary}--\r\n`));
  return { body: Buffer.concat(parts), type: `multipart/form-data; boundary=${boundary}` };
}
async function post(pathname, { body, type }) {
  const r = await fetch(W + pathname, { method: 'POST', body, headers: { 'content-type': type, 'content-length': String(body.length) } });
  const text = await r.text(); let j = null; try { j = JSON.parse(text); } catch {}
  return { status: r.status, json: j, text };
}
function creativeRequest(F, kp, auctionName, img, text, click, opts = {}) {
  const [, epoch] = F.auctions[auctionName];
  const n = opts.nonce ?? nonce(), expires = nowS() + 300;
  const pl = payload(img, text ?? '', click ?? '');
  const m = creativeMessage({ program: F.ad_pool, game: F.game, epoch, slot: 0, pl, n, expires });
  return multipart({ game: F.game, epoch, slot: 0, signer: kp.pub, nonce: n, expires, signature: sign(kp, m), text, click_url: click }, img);
}
function defaultAdRequest(F, kp, width, height, img, text, click) {
  const n = nonce(), expires = nowS() + 300, pl = payload(img, text, click);
  const m = defaultAdMessage({ program: F.platform_core, width, height, pl, n, expires });
  return multipart({ width, height, signer: kp.pub, nonce: n, expires, signature: sign(kp, m), text, click_url: click }, img);
}

// ------------------------------------------------------------------ runner
const results = [];
const check = (id, ok, detail = '') => { results.push({ id, ok }); console.log(`${ok ? 'PASS' : 'FAIL'} ${id}${ok ? '' : ' — ' + detail}`); };
const last = () => stats.sent[stats.sent.length - 1];

async function main() {
  console.log('… building Worker (worker-build --release) + fixtures');
  execFileSync('worker-build', ['--release'], { cwd: WORKER, stdio: 'ignore' });
  execFileSync('cargo', ['build', '-q', '--example', 'e2e_fixtures'], { cwd: WORKER, stdio: 'inherit' });

  const adv = keypair(), admin = keypair(), moderator = keypair(), crank = keypair(), stranger = keypair(), owner = keypair();
  const F = JSON.parse(execFileSync(path.join(WORKER, 'target/debug/examples/e2e_fixtures'),
    [String(nowS()), adv.pub, admin.pub, moderator.pub, owner.pub]).toString());
  state.accounts = F.accounts;

  await new Promise((r) => mock.listen(MOCK_PORT, '127.0.0.1', r));
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'ad-pool-e2e-'));
  // wrangler.toml copy: absolute main, no [build] (already built), nothing else changes
  const toml = fs.readFileSync(path.join(WORKER, 'wrangler.toml'), 'utf8')
    .replace('main = "build/index.js"', `main = "${path.join(WORKER, 'build/index.js')}"`)
    .replace(/\[build\]\ncommand = "worker-build --release"\n/, '');
  fs.writeFileSync(path.join(tmp, 'wrangler.toml'), toml);
  const vars = {
    RPC_URL_DEVNET: MOCK, OPENAI_MODERATIONS_URL: `${MOCK}/v1/moderations`, WEB_RISK_SEARCH_URL: `${MOCK}/v1/uris:search`,
    OPENAI_API_KEY: OPENAI_KEY, WEB_RISK_API_KEY: WEBRISK_KEY,
    MODERATOR_KEYPAIR: moderator.secret58, MODERATOR_ADDRESS: moderator.pub,
    CRANK_KEYPAIR: crank.secret58, CRANK_ADDRESS: crank.pub,
    MAX_SUBMISSIONS_PER_HOUR: '3', RPC_TIMEOUT_MS: '2000', PUBLIC_BASE_URL: 'https://ads.adpool-e2e.com',
    SUBREQUEST_BUDGET: '900',
  };
  const logPath = path.join(tmp, 'wrangler.log');
  const log = fs.openSync(logPath, 'w');
  const wr = spawn(path.join(WORKER, 'node_modules/.bin/wrangler'),
    ['dev', '--port', String(PORT), '--ip', '127.0.0.1', '--persist-to', path.join(tmp, 'state'), '--test-scheduled',
      ...Object.entries(vars).flatMap(([k, v]) => ['--var', `${k}:${v}`])],
    { cwd: tmp, stdio: ['ignore', log, log], detached: true });
  try {
    for (let i = 0; ; i++) {
      if (/Ready on/.test(fs.readFileSync(logPath, 'utf8'))) break;
      if (i > 300) throw new Error('wrangler did not start:\n' + fs.readFileSync(logPath, 'utf8'));
      await sleep(200);
    }
    await run(F, { adv, admin, moderator, crank, stranger, owner });
    const L = fs.readFileSync(logPath, 'utf8');
    check('E-LOG no secret in wrangler log', ![OPENAI_KEY, WEBRISK_KEY, moderator.secret58, crank.secret58].some((s) => L.includes(s)));
    check('E-LOG stale settle reported', /STALE/.test(L));
  } finally {
    try { process.kill(-wr.pid); } catch {}
    mock.close();
  }
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  process.exit(failed.length ? 1 : 0);
}

async function run(F, K) {
  const png = fs.readFileSync(path.join(HERE, 'fixtures/b728x90.png'));
  const jpg = fs.readFileSync(path.join(HERE, 'fixtures/b728x90.jpg'));
  const m300 = fs.readFileSync(path.join(HERE, 'fixtures/m300x250.png'));
  const AP = IDL.address, PC = PC_IDL.address;
  const CLEAN = 'https://example.com/';
  const BAD = 'https://bad.e2e-test.example.org/';
  const BAD2 = 'https://bad2.e2e-test.example.org/';
  state.threats = [BAD, BAD2, 'https://malware.e2e-test.example.org/'];

  // E0 — first request of the isolate: wrong cluster must be refused (guard caches success only)
  state.genesis = MAINNET;
  let r = await fetch(W + '/v1/health/rpc'); let j = await r.json();
  check('E0 wrong cluster → 502 rpc_wrong_cluster', r.status === 502 && j.error === 'rpc_wrong_cluster', JSON.stringify(j));
  state.genesis = DEVNET;
  r = await fetch(W + '/v1/health/rpc');
  check('E0b devnet → ok', r.status === 200);

  // E1 — full approve
  let n0 = stats.sent.length;
  let res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty1', png, 'ร้านกาแฟเปิดใหม่', 'https://Example.com/'));
  let tx = last();
  check('E1 full → 200 approved', res.status === 200 && res.json?.status === 'approved', res.text);
  check('E1 tx = approve_creative, signed by moderator only, sig valid',
    stats.sent.length === n0 + 1 && tx.ixs.length === 1 && tx.ixs[0].program === AP
      && tx.ixs[0].data.subarray(0, 8).equals(disc(IDL, 'approve_creative'))
      && tx.numSigners === 1 && tx.signers[0] === K.moderator.pub && tx.verified
      && tx.ixs[0].accounts[0] === K.moderator.pub && tx.ixs[0].accounts[2] === F.auctions.empty1[0], JSON.stringify(tx));
  check('E1 creative on chain: normalized click_url + Thai text + flags 0',
    tx.ixs[0].data.includes(Buffer.from('ร้านกาแฟเปิดใหม่')) && tx.ixs[0].data.includes(Buffer.from('https://example.com/'))
      && tx.ixs[0].data.subarray(-4).readUInt32LE() === 0);
  check('E1 moderation: image + text calls, model explicit, png mime',
    stats.moderations.filter((m) => m.model === 'omni-moderation-latest').length >= 2
      && stats.moderations.some((m) => m.kind === 'image' && m.mime === 'image/png'));
  const sha = sha256(png).toString('hex');
  r = await fetch(`${W}/ads/${sha}.png`); const bytes = Buffer.from(await r.arrayBuffer());
  check('E1 GET /ads → bytes = sha, image/png, nosniff, immutable',
    r.status === 200 && sha256(bytes).toString('hex') === sha && r.headers.get('content-type') === 'image/png'
      && r.headers.get('x-content-type-options') === 'nosniff' && /immutable/.test(r.headers.get('cache-control')));
  r = await fetch(`${W}/ads/${sha}.png`, { headers: { 'if-none-match': `"${sha}"` } });
  check('E1 GET /ads If-None-Match → 304', r.status === 304);
  r = await fetch(`${W}/ads/${'00'.repeat(32)}.png`);
  check('E1 GET /ads unknown → 404', r.status === 404);

  // E2 — image flagged (JPEG) → text-only in one tx with image_reject_flags = 4
  state.flagImageSha = [sha256(jpg).toString('hex')];
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty2', jpg, 'ข้อความอย่างเดียว', CLEAN));
  tx = last();
  check('E2 flagged image → 200 text_only', res.status === 200 && res.json?.status === 'text_only' && res.json?.image_reject_flags === 4, res.text);
  check('E2 tx approve_creative with empty image_url, zero sha, flags 4',
    tx.ixs[0].data.subarray(0, 8).equals(disc(IDL, 'approve_creative')) && tx.ixs[0].data.readUInt32LE(8) === 0
      && tx.ixs[0].data.subarray(12, 44).equals(Buffer.alloc(32)) && tx.ixs[0].data.subarray(-4).readUInt32LE() === 4);
  check('E2 jpeg sent to OpenAI as image/jpeg', stats.moderations.some((m) => m.mime === 'image/jpeg'));
  r = await fetch(`${W}/ads/${sha256(jpg).toString('hex')}.jpg`);
  check('E2 flagged image NOT stored in R2', r.status === 404);
  state.flagImageSha = [];

  // E3 — image-only on TextOnly → approve_image
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'textonly1', png, undefined, undefined));
  tx = last();
  check('E3 image-only → 200 image_approved (approve_image)', res.status === 200 && res.json?.status === 'image_approved'
    && tx.ixs[0].data.subarray(0, 8).equals(disc(IDL, 'approve_image')), res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'textonly2', png, 'x', CLEAN));
  check('E3b full on TextOnly → 409 WrongKind', res.status === 409 && res.json?.reason?.WrongKind !== undefined, res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty15', png, undefined, undefined));
  check('E3c image-only on Empty → 409 WrongKind', res.status === 409, res.text);

  // E4 — text flagged → reject_creative flags 8 ; + Web Risk threat → 8|32
  state.flagText = ['VIOLENT'];
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty3', png, 'VIOLENT threat', CLEAN));
  tx = last();
  check('E4 text flagged → 422 reject flags 8', res.status === 422 && res.json?.flags === 8
    && tx.ixs[0].data.subarray(0, 8).equals(disc(IDL, 'reject_creative')) && tx.ixs[0].data.readUInt32LE(8) === 8, res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty4', png, 'VIOLENT threat', BAD));
  check('E4b text + Web Risk → flags 40', res.status === 422 && res.json?.flags === 40, res.text);
  state.flagText = [];
  const wrBefore = stats.webrisk.length;
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty16', png, 'ok', BAD2));
  check('E4c Web Risk threat alone → 422 flags 32', res.status === 422 && res.json?.flags === 32, res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty17', png, 'ok', BAD2));
  check('E4d second lookup of the same URL comes from the cache', res.status === 422 && stats.webrisk.length === wrBefore + 1,
    `webrisk calls ${stats.webrisk.length - wrBefore}`);

  // E5 — fail closed: OpenAI 500 / Web Risk billing → 503, nothing sent
  n0 = stats.sent.length;
  state.openai = '500';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty5', png, 'ok', CLEAN));
  check('E5 OpenAI 500 → 503, no tx', res.status === 503 && res.json?.error === 'moderation_unavailable' && stats.sent.length === n0, res.text);
  state.openai = 'ok'; state.webrisk = 'billing';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty6', png, 'ok', 'https://example.com/fresh'));
  check('E5b Web Risk billing → 503 web_risk:billing_disabled, no tx', res.status === 503
    && JSON.stringify(res.json).includes('billing_disabled') && stats.sent.length === n0, res.text);
  state.webrisk = 'ok';

  // E6 — gate: replay, rate limit, in-flight
  const req7 = creativeRequest(F, K.adv, 'empty7', png, 'VIOLENT', BAD);
  state.flagText = ['VIOLENT'];
  await post('/v1/creative', req7);
  res = await post('/v1/creative', req7);
  check('E6 same signed request twice → 409 Replay', res.status === 409 && res.json?.reason === 'Replay', res.text);
  for (let i = 0; i < 3; i++) await post('/v1/creative', creativeRequest(F, K.adv, 'empty8', png, 'VIOLENT', BAD));
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty8', png, 'VIOLENT', BAD));
  check('E6b 4th submission in the hour → 429 RateLimited', res.status === 429 && res.json?.reason?.RateLimited, res.text);
  state.sendDelayMs = 1500; // < RPC_TIMEOUT_MS: the first one completes normally
  const [a1, a2] = await Promise.all([
    post('/v1/creative', creativeRequest(F, K.adv, 'empty9', png, 'VIOLENT', BAD)),
    sleep(400).then(() => post('/v1/creative', creativeRequest(F, K.adv, 'empty9', png, 'VIOLENT', BAD))),
  ]);
  check('E6c concurrent → first 422 (sent), second 409 InFlight', a1.status === 422 && a2.status === 409
    && JSON.stringify(a2.json).includes('InFlight'), `${a1.status} ${a2.status}`);
  state.sendDelayMs = 0; state.flagText = [];

  // E7 — send / confirm outcomes
  state.send = 'preflight6012';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty10', png, 'ok', CLEAN));
  check('E7 preflight custom 6012 → 502 tx_rejected program_error 6012', res.status === 502 && res.json?.program_error === 6012, res.text);
  state.send = 'unhealthy';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty11', png, 'ok', CLEAN));
  check('E7b node unhealthy → 503 rpc_unavailable', res.status === 503 && res.json?.error === 'rpc_unavailable', res.text);
  state.send = '429';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty18', png, 'ok', CLEAN));
  check('E7c RPC 429 → 503 rpc_rate_limited', res.status === 503 && res.json?.error === 'rpc_rate_limited', res.text);
  state.send = 'ok'; state.status = 'failed';
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty12', png, 'ok', CLEAN));
  check('E7d tx failed on chain → 502 tx_failed', res.status === 502 && res.json?.error === 'tx_failed' && res.json?.program_error === 6013, res.text);
  // a reject leaves the status as it is, so only the lock can stop a second tx here
  state.status = 'pending'; state.flagText = ['VIOLENT'];
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty13', png, 'VIOLENT', BAD));
  check('E7e never confirmed → 202 confirmed:false', res.status === 202 && res.json?.confirmed === false, res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty13', png, 'VIOLENT', BAD));
  check('E7f lock kept after a pending tx → 409 InFlight', res.status === 409 && JSON.stringify(res.json).includes('InFlight'), res.text);
  state.status = 'confirmed'; state.flagText = [];

  // E8 — chain checks
  res = await post('/v1/creative', creativeRequest(F, K.stranger, 'empty14', png, 'ok', CLEAN));
  check('E8 not the winner → 403', res.status === 403 && res.json?.reason === 'NotWinner', res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'bidding', png, 'ok', CLEAN));
  check('E8b bidding still open → 409 NotInCreativeWindow', res.status === 409 && res.json?.reason === 'NotInCreativeWindow', res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty19', m300, 'ok', CLEAN));
  check('E8c wrong W×H → 400', res.status === 400 && res.json?.reason?.Image?.WrongSize, res.text);
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty19', png, 'ok', 'https://bit.ly/x'));
  check('E8d shortener → 400 ClickUrl', res.status === 400 && res.json?.reason?.ClickUrl === 'Shortener', res.text);
  const bad = creativeRequest(F, K.adv, 'empty19', png, 'ok', CLEAN);
  const tampered = { ...bad, body: Buffer.from(bad.body.toString('latin1').replace('name="text"\r\n\r\nok', 'name="text"\r\n\r\nOK'), 'latin1') };
  res = await post('/v1/creative', tampered);
  check('E8e text changed after signing → 401', res.status === 401, res.text);

  // E11 — the auction became Approved while moderation ran → re-read refuses, no tx
  n0 = stats.sent.length;
  state.flipOnModeration = F.auctions.empty20[0];
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty20', png, 'ok', CLEAN));
  check('E11 status changed during moderation → 409 AlreadyApproved, no tx', res.status === 409
    && res.json?.reason === 'AlreadyApproved' && stats.sent.length === n0, res.text);
  // E11b — the mock applied E1's approve: a second full submission is refused before moderation
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty1', png, 'again', CLEAN));
  check('E11b after approve → 409 AlreadyApproved', res.status === 409 && res.json?.reason === 'AlreadyApproved', res.text);
  // E11c — click_url pointing at the Worker itself → on-chain reject URL_LAYER1 (never fetched)
  res = await post('/v1/creative', creativeRequest(F, K.adv, 'empty19', png, 'ok', 'https://ads.adpool-e2e.com/v1/health/rpc'));
  check('E11c click_url = Worker origin → 422 flags 16 SelfHost', res.status === 422 && res.json?.flags === 16
    && JSON.stringify(res.json).includes('SelfHost'), res.text);
  r = await fetch(W + '/health'); j = await r.json();
  check('E11d /health reports the 127.0.0.1 override', j.upstream_overridden === true);

  // E9 — default ad
  res = await post('/v1/default-ad', defaultAdRequest(F, K.stranger, 728, 90, png, 'พื้นฐาน', CLEAN));
  check('E9 not Admin → 403 NotAdmin', res.status === 403 && res.json?.reason === 'NotAdmin', res.text);
  res = await post('/v1/default-ad', defaultAdRequest(F, K.adv, 728, 90, png, 'พื้นฐาน', CLEAN));
  check('E9b winner of an auction is still not Admin → 403', res.status === 403, res.text);
  res = await post('/v1/default-ad', defaultAdRequest(F, K.admin, 728, 90, png, 'พื้นฐาน', CLEAN));
  tx = last();
  check('E9c Admin, new size → 200 created (pc create_default_ad as Moderator)', res.status === 200 && res.json?.status === 'created'
    && res.json?.enabled === true && tx.ixs[0].program === PC && tx.ixs[0].data.subarray(0, 8).equals(disc(PC_IDL, 'create_default_ad'))
    && tx.ixs[0].data[8] === 2 && tx.ixs[0].accounts[2] === F.default_ad_728x90 && tx.signers[0] === K.moderator.pub && tx.verified, res.text);
  res = await post('/v1/default-ad', defaultAdRequest(F, K.admin, 300, 250, m300, 'ใหม่', CLEAN));
  tx = last();
  check('E9d Admin, existing size → 200 updated, enabled false reported', res.status === 200 && res.json?.status === 'updated'
    && res.json?.enabled === false && tx.ixs[0].data.subarray(0, 8).equals(disc(PC_IDL, 'update_default_ad')), res.text);
  state.flagImageSha = [sha256(m300).toString('hex')];
  n0 = stats.sent.length;
  res = await post('/v1/default-ad', defaultAdRequest(F, K.admin, 300, 250, m300, 'ใหม่', CLEAN));
  check('E9e flagged image → 422, no text-only, no tx', res.status === 422 && stats.sent.length === n0, res.text);
  state.flagImageSha = [];
  // a creative signature must not pass as a default-ad one
  const cr = creativeRequest(F, K.admin, 'empty20', png, 'พื้นฐาน', CLEAN);
  const crFields = cr.body.toString('latin1');
  const sig = /name="signature"\r\n\r\n([^\r]+)/.exec(crFields)[1];
  const n = /name="nonce"\r\n\r\n([^\r]+)/.exec(crFields)[1];
  const ex = /name="expires"\r\n\r\n([^\r]+)/.exec(crFields)[1];
  res = await post('/v1/default-ad', multipart({ width: 728, height: 90, signer: K.admin.pub, nonce: n, expires: ex, signature: sig, text: 'พื้นฐาน', click_url: CLEAN }, png));
  check('E9f creative signature replayed on /v1/default-ad → 401', res.status === 401, res.text);

  // E10 — cron crank
  n0 = stats.sent.length;
  r = await fetch(W + '/__scheduled?cron=*/10+*+*+*+*');
  await sleep(1500);
  const crankTxs = stats.sent.slice(n0);
  const find = (name) => crankTxs.find((t) => t.ixs.some((i) => i.accounts[0] === F.auctions[name][0] || i.accounts[2] === F.auctions[name][0]));
  const s1 = find('settle_due'), s2 = find('settle_close_due'), s3 = find('close_due'), rv = find('revoke_me');
  check('E10 settle due → settle (game_owner from GameEntry, treasury), crank key pays',
    s1 && s1.ixs.length === 1 && s1.ixs[0].data.equals(disc(IDL, 'settle')) && s1.ixs[0].accounts[2] === F.game_owner
      && s1.ixs[0].accounts[4] === F.treasury && s1.signers[0] === K.crank.pub && s1.numSigners === 1 && s1.verified, JSON.stringify(s1));
  check('E10b settle + close in one tx', s2 && s2.ixs.length === 2 && s2.ixs[0].data.equals(disc(IDL, 'settle'))
    && s2.ixs[1].data.equals(disc(IDL, 'close_auction')) && s2.ixs[1].accounts[1] === K.adv.pub);
  check('E10c settled + ended → close_auction to rent_payer', s3 && s3.ixs.length === 1 && s3.ixs[0].data.equals(disc(IDL, 'close_auction'))
    && s3.ixs[0].accounts[1] === K.adv.pub && s3.signers[0] === K.crank.pub);
  check('E10d live Approved with a Web Risk threat → revoke_creative by the moderator key',
    rv && rv.ixs[0].data.subarray(0, 8).equals(disc(IDL, 'revoke_creative')) && rv.ixs[0].data.readUInt32LE(8) === 32
      && rv.signers[0] === K.moderator.pub && rv.verified, JSON.stringify(rv));
  const st = find('stale');
  check('E10e overdue settle also sent; nothing for auctions not due', st && crankTxs.length === 5, `sent ${crankTxs.length}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
