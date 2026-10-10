// node fuzz-wasm/run.mjs <wasm> <fixtures dir> — exits 1 on any trap or wrong verdict.
import fs from 'node:fs';
const [wasmPath, fixtures] = process.argv.slice(2);
const { instance } = await WebAssembly.instantiate(fs.readFileSync(wasmPath), {});
const { memory, buf_ptr, sniff, fuzz } = instance.exports;
const load = (b) => { new Uint8Array(memory.buffer).set(b, buf_ptr()); return b.length; };
const le32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n >>> 0); return b; };
const riff = (chunks) => {
  let body = Buffer.from('WEBP');
  for (const [k, d, forced] of chunks) body = Buffer.concat([body, Buffer.from(k), le32(forced ?? d.length), d, d.length % 2 ? Buffer.from([0]) : Buffer.alloc(0)]);
  return Buffer.concat([Buffer.from('RIFF'), le32(body.length), body]);
};
const vp8l = (w, h) => { const b = Buffer.alloc(5); b[0] = 0x2f; b.writeUInt32LE(((w - 1) | ((h - 1) << 14)) >>> 0, 1); return b; };
let failed = 0;
const expect = (name, bytes, want) => {
  try {
    const got = sniff(load(bytes)) === 0 ? 'Ok' : 'Err';
    if (got !== want) { failed++; console.log(`FAIL ${name}: ${got} (want ${want})`); } else console.log(`ok   ${name}: ${got}`);
  } catch (e) { failed++; console.log(`TRAP ${name}: ${e}`); }
};
expect('webp valid', riff([['VP8L', vp8l(300, 250)]]), 'Ok');
for (const n of [0xffffffff, 0xfffffffe, 0xfffffff8, 0x80000000]) expect(`webp chunk len 0x${n.toString(16)}`, riff([['VP8L', Buffer.alloc(0), n]]), 'Err');
const png = fs.readFileSync(`${fixtures}/b728x90.png`);
const huge = Buffer.from(png); huge.writeUInt32BE(0xffffffe8, 33); expect('png chunk len 0xffffffe8', huge, 'Err');
for (const f of ['b728x90.png', 'b728x90.jpg']) expect(`fixture ${f}`, fs.readFileSync(`${fixtures}/${f}`), 'Ok');
const seeds = [fs.readFileSync(`${fixtures}/b728x90.png`), fs.readFileSync(`${fixtures}/b728x90.jpg`), riff([['VP8X', Buffer.from([0, 0, 0, 0, 0xd7, 0x02, 0x00, 0x59, 0, 0])], ['VP8L', vp8l(728, 90)]])];
for (const s of seeds) {
  try { load(s); fuzz(s.length, 20000); console.log(`ok   fuzz ${s.length} B x20000`); } catch (e) { failed++; console.log(`TRAP fuzz: ${e}`); }
}
process.exit(failed ? 1 : 0);
