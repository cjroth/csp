// Generates placeholder RGBA PNG icons (no native deps) so tauri.conf.json
// icon paths resolve. On macOS, regenerate the full set from a real source
// with `bunx tauri icon ./app-icon.png`.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { deflateSync } from "node:zlib";

const crcTable = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td), 0);
  return Buffer.concat([len, td, crc]);
}

// rgba = (x, y) => [r, g, b, a]
function png(size, rgba) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc(size * (1 + size * 4));
  let o = 0;
  for (let y = 0; y < size; y++) {
    raw[o++] = 0; // filter: none
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = rgba(x, y, size);
      raw[o++] = r;
      raw[o++] = g;
      raw[o++] = b;
      raw[o++] = a;
    }
  }
  return Buffer.concat([
    sig,
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// Brand mark: rounded square, indigo, with a lighter "sync" ring cut.
function appPixel(x, y, size) {
  const cx = size / 2;
  const cy = size / 2;
  const dx = x - cx;
  const dy = y - cy;
  const d = Math.sqrt(dx * dx + dy * dy);
  const r = size * 0.42;
  // rounded-square mask
  const m = size * 0.16;
  const inside =
    x > m && x < size - m && y > m && y < size - m
      ? 255
      : Math.max(0, 255 - (Math.min(x, y, size - x, size - y) < m / 2 ? 255 : 0));
  if (inside === 0) return [0, 0, 0, 0];
  if (d > r * 0.55 && d < r * 0.72) return [165, 180, 252, 255]; // ring
  return [79, 70, 229, 255]; // indigo-600
}

// macOS template tray icon: black + alpha only (OS masks/inverts it).
function trayPixel(x, y, size) {
  const cx = size / 2;
  const cy = size / 2;
  const d = Math.sqrt((x - cx) ** 2 + (y - cy) ** 2);
  const outer = size * 0.40;
  const inner = size * 0.22;
  const a = d <= outer && d >= inner ? 255 : 0;
  return [0, 0, 0, a];
}

function emit(path, size, fn) {
  const full = join(process.cwd(), path);
  mkdirSync(dirname(full), { recursive: true });
  writeFileSync(full, png(size, fn));
  console.log("wrote", path, `${size}x${size}`);
}

emit("src-tauri/icons/32x32.png", 32, appPixel);
emit("src-tauri/icons/128x128.png", 128, appPixel);
emit("src-tauri/icons/128x128@2x.png", 256, appPixel);
emit("src-tauri/icons/icon.png", 512, appPixel);
emit("src-tauri/icons/tray-template.png", 32, trayPixel);
