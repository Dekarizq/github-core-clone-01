// Git object model, pkt-line framing and packfiles (stdlib only).
// Mirrors the wire format git-client.ts speaks: SHA-1 object ids, zlib
// pack entries (no deltas on write), OFS/REF delta resolution on read.
import { createHash } from 'node:crypto';
import { inflateSync as zlibInflate, deflateSync as zlibDeflate } from 'node:zlib';

export const ZERO_OID = '0'.repeat(40);
export type ObjType = 'commit' | 'tree' | 'blob' | 'tag';
export interface GitObj { type: ObjType; data: Buffer }

const TYPE_CODE: Record<ObjType, number> = { commit: 1, tree: 2, blob: 3, tag: 4 };
const CODE_TYPE: Record<number, ObjType> = { 1: 'commit', 2: 'tree', 3: 'blob', 4: 'tag' };
const HEX40 = /^[0-9a-f]{40}$/;
export const isOid = (v: string): boolean => HEX40.test(v);

export function hashObject(type: ObjType, data: Buffer): string {
  const h = createHash('sha1');
  h.update(Buffer.from(`${type} ${data.length}\0`, 'utf8'));
  h.update(data);
  return h.digest('hex');
}
export function putObj(store: Map<string, GitObj>, type: ObjType, data: Buffer): string {
  const oid = hashObject(type, data);
  if (!store.has(oid)) store.set(oid, { type, data });
  return oid;
}

// ---------------------------------------------------------------- pkt-line
export function pkt(payload: string | Buffer): Buffer {
  const d = typeof payload === 'string' ? Buffer.from(payload, 'utf8') : payload;
  const h = Buffer.allocUnsafe(4);
  h.write((d.length + 4).toString(16).padStart(4, '0'), 0, 4, 'ascii');
  return Buffer.concat([h, d]);
}
export const FLUSH = Buffer.from('0000', 'ascii');
export const DELIM = Buffer.from('0001', 'ascii');
export function pktLines(items: Array<string | Buffer | 'flush' | 'delim'>): Buffer {
  const parts: Buffer[] = [];
  for (const it of items) {
    if (it === 'flush') parts.push(FLUSH);
    else if (it === 'delim') parts.push(DELIM);
    else parts.push(pkt(it));
  }
  return Buffer.concat(parts);
}
/** Decode a whole buffer into packets; `rest` is the unframed trailing bytes (pack data). */
export function decodePktFull(buf: Buffer): { packets: Array<{ kind: 'data' | 'flush' | 'delim' | 'end'; payload?: Buffer }>; rest: Buffer } {
  const packets: Array<{ kind: 'data' | 'flush' | 'delim' | 'end'; payload?: Buffer }> = [];
  let off = 0;
  while (off + 4 <= buf.length) {
    const head = buf.subarray(off, off + 4).toString('ascii');
    if (!/^[0-9a-fA-F]{4}$/.test(head)) break;
    const len = parseInt(head, 16);
    if (len === 0) { packets.push({ kind: 'flush' }); off += 4; continue; }
    if (len === 1) { packets.push({ kind: 'delim' }); off += 4; continue; }
    if (len === 2) { packets.push({ kind: 'end' }); off += 4; continue; }
    if (len < 4 || off + len > buf.length) break;
    packets.push({ kind: 'data', payload: buf.subarray(off + 4, off + len) });
    off += len;
  }
  return { packets, rest: buf.subarray(off) };
}

// ---------------------------------------------------------------- trees / commits
export interface TreeEntry { mode: string; name: string; oid: string }
export function parseTree(data: Buffer): TreeEntry[] {
  const entries: TreeEntry[] = [];
  let off = 0;
  while (off < data.length) {
    const sp = data.indexOf(0x20, off);
    const nul = data.indexOf(0x00, sp + 1);
    entries.push({ mode: data.subarray(off, sp).toString('ascii'), name: data.subarray(sp + 1, nul).toString('utf8'), oid: data.subarray(nul + 1, nul + 21).toString('hex') });
    off = nul + 21;
  }
  return entries;
}
export function buildTree(entries: TreeEntry[]): Buffer {
  const sorted = [...entries].sort((a, b) => {
    const l = Buffer.from(a.mode === '40000' ? `${a.name}/` : a.name, 'utf8');
    const r = Buffer.from(b.mode === '40000' ? `${b.name}/` : b.name, 'utf8');
    return Buffer.compare(l, r);
  });
  const parts: Buffer[] = [];
  for (const e of sorted) {
    parts.push(Buffer.from(`${e.mode} ${e.name}\0`, 'utf8'));
    parts.push(Buffer.from(e.oid, 'hex'));
  }
  return Buffer.concat(parts);
}
export interface CommitView { tree: string; parents: string[]; author: string; committer: string; message: string }
export function parseCommit(data: Buffer): CommitView {
  const blank = data.indexOf('\n\n');
  const head = blank < 0 ? data.toString('utf8') : data.subarray(0, blank).toString('utf8');
  const message = blank < 0 ? '' : data.subarray(blank + 2).toString('utf8');
  let tree = ''; const parents: string[] = []; let author = ''; let committer = '';
  for (const line of head.split('\n')) {
    const sp = line.indexOf(' ');
    const k = sp < 0 ? line : line.slice(0, sp);
    const v = sp < 0 ? '' : line.slice(sp + 1);
    if (k === 'tree') tree = v; else if (k === 'parent') parents.push(v);
    else if (k === 'author') author = v; else if (k === 'committer') committer = v;
  }
  return { tree, parents, author, committer, message };
}
export function buildCommit(tree: string, parents: string[], author: string, committer: string, message: string): Buffer {
  const lines = [`tree ${tree}`];
  for (const p of parents) lines.push(`parent ${p}`);
  lines.push(`author ${author}`, `committer ${committer}`, '');
  return Buffer.from(`${lines.join('\n')}\n${message}`, 'utf8');
}
export function parseTag(data: Buffer): { object: string; type: ObjType; tag: string; tagger: string; message: string } {
  const blank = data.indexOf('\n\n');
  const head = blank < 0 ? data.toString('utf8') : data.subarray(0, blank).toString('utf8');
  const m = new Map<string, string>();
  for (const line of head.split('\n')) {
    const sp = line.indexOf(' ');
    if (sp > 0) m.set(line.slice(0, sp), line.slice(sp + 1));
  }
  return { object: m.get('object') ?? '', type: (m.get('type') ?? 'commit') as ObjType, tag: m.get('tag') ?? '', tagger: m.get('tagger') ?? '', message: blank < 0 ? '' : data.subarray(blank + 2).toString('utf8') };
}

/** Every object reachable from roots (commit->tree+parents, tag->target, tree->entries). */
export function collectReachable(roots: Iterable<string>, lookup: (oid: string) => GitObj | undefined): Set<string> {
  const seen = new Set<string>();
  const queue: string[] = [];
  for (const r of roots) if (isOid(r)) queue.push(r);
  while (queue.length) {
    const oid = queue.pop()!;
    if (seen.has(oid)) continue;
    const o = lookup(oid);
    if (!o) continue;
    seen.add(oid);
    if (o.type === 'commit') {
      const c = parseCommit(o.data);
      queue.push(c.tree);
      for (const p of c.parents) queue.push(p);
    } else if (o.type === 'tag') {
      queue.push(parseTag(o.data).object);
    } else if (o.type === 'tree') {
      for (const e of parseTree(o.data)) if (e.mode !== '160000') queue.push(e.oid);
    }
  }
  return seen;
}
/** Is `anc` an ancestor of `oid` (or equal)? Walks commit parents only. */
export function isAncestor(store: Map<string, GitObj>, anc: string, oid: string): boolean {
  if (anc === oid) return true;
  const seen = new Set<string>();
  const queue = [oid];
  while (queue.length) {
    const cur = queue.pop()!;
    if (seen.has(cur)) continue;
    seen.add(cur);
    const o = store.get(cur);
    if (!o || o.type !== 'commit') continue;
    for (const p of parseCommit(o.data).parents) {
      if (p === anc) return true;
      queue.push(p);
    }
  }
  return false;
}
/** First common ancestor of two commits (BFS from both sides, deterministic). */
export function mergeBase(store: Map<string, GitObj>, a: string, b: string): string | null {
  if (a === b) return a;
  const ancA = new Map<string, number>();
  const qa: Array<[string, number]> = [[a, 0]];
  while (qa.length) {
    const [cur, d] = qa.shift()!;
    if (ancA.has(cur)) continue;
    ancA.set(cur, d);
    const o = store.get(cur);
    if (o?.type === 'commit') for (const p of parseCommit(o.data).parents) qa.push([p, d + 1]);
  }
  const qb: Array<[string, number]> = [[b, 0]];
  const seenB = new Set<string>();
  let best: string | null = null; let bestScore = Infinity;
  while (qb.length) {
    const [cur, d] = qb.shift()!;
    if (seenB.has(cur)) continue;
    seenB.add(cur);
    const da = ancA.get(cur);
    if (da !== undefined) {
      const score = da + d;
      if (score < bestScore) { bestScore = score; best = cur; }
    }
    const o = store.get(cur);
    if (o?.type === 'commit') for (const p of parseCommit(o.data).parents) qb.push([p, d + 1]);
  }
  return best;
}
/** Flat path -> {oid, mode} map of a tree (recursive). */
export function flattenTree(store: Map<string, GitObj>, treeOid: string, prefix = ''): Map<string, { oid: string; mode: string }> {
  const out = new Map<string, { oid: string; mode: string }>();
  const walk = (oid: string, pre: string): void => {
    const o = store.get(oid);
    if (!o || o.type !== 'tree') return;
    for (const e of parseTree(o.data)) {
      if (e.mode === '40000') walk(e.oid, pre + e.name + '/');
      else out.set(pre + e.name, { oid: e.oid, mode: e.mode });
    }
  };
  walk(treeOid, prefix);
  return out;
}

// ---------------------------------------------------------------- delta / pack read
function applyDelta(base: Buffer, delta: Buffer): Buffer {
  let off = 0;
  const readSize = (): number => {
    let v = 0; let sh = 0;
    for (;;) {
      const b = delta[off++];
      v += (b & 0x7f) * 2 ** sh; sh += 7;
      if (!(b & 0x80)) break;
    }
    return v;
  };
  const srcSize = readSize();
  const tgtSize = readSize();
  if (srcSize !== base.length) throw new Error('delta base size mismatch');
  const out = Buffer.allocUnsafe(tgtSize);
  let written = 0;
  while (off < delta.length) {
    const op = delta[off++];
    if (op & 0x80) {
      let co = 0; let cs = 0; let sh = 0;
      for (let bit = 0; bit < 4; bit++) { if (op & (1 << bit)) co += delta[off++] * 2 ** sh; sh += 8; }
      sh = 0;
      for (let bit = 4; bit < 7; bit++) { if (op & (1 << bit)) cs += delta[off++] * 2 ** sh; sh += 8; }
      if (cs === 0) cs = 0x10000;
      base.copy(out, written, co, co + cs);
      written += cs;
    } else if (op !== 0) {
      const n = op & 0x7f;
      delta.copy(out, written, off, off + n);
      off += n; written += n;
    } else throw new Error('delta opcode 0');
  }
  if (written !== tgtSize) throw new Error('delta size mismatch');
  return out;
}

export interface PackRead { objects: Map<string, GitObj>; deltaCount: number }
/** Parse a packfile; resolveBase supplies thin-pack bases from the local store. */
export function readPack(pack: Buffer, resolveBase?: (oid: string) => GitObj | undefined): PackRead {
  if (pack.length < 32 || pack.subarray(0, 4).toString('ascii') !== 'PACK') throw new Error('bad pack header');
  const version = pack.readUInt32BE(4);
  if (version !== 2 && version !== 3) throw new Error('bad pack version');
  const count = pack.readUInt32BE(8);
  const trailerStart = pack.length - 20;
  interface Raw { offset: number; typeCode: number; size: number; data: Buffer; baseOffset?: number; baseOid?: string; resolved?: GitObj; oid?: string }
  const raw: Raw[] = [];
  const byOffset = new Map<number, Raw>();
  let off = 12;
  const readVarint = (start: number): { size: number; next: number; first: number } => {
    let b = pack[start];
    const first = b;
    let size = b & 0x0f; let sh = 4; let o = start + 1;
    while (b & 0x80) { b = pack[o++]; size += (b & 0x7f) * 2 ** sh; sh += 7; }
    return { size, next: o, first };
  };
  for (let i = 0; i < count; i++) {
    const entryOff = off;
    const hv = readVarint(off); off = hv.next;
    const typeCode = (hv.first >> 4) & 0x07;
    const e: Raw = { offset: entryOff, typeCode, size: hv.size, data: Buffer.alloc(0) };
    if (typeCode === 6) {
      let b = pack[off]; off++;
      let v = b & 0x7f;
      while (b & 0x80) { b = pack[off++]; v = ((v + 1) * 128) + (b & 0x7f); }
      e.baseOffset = entryOff - v;
    } else if (typeCode === 7) {
      e.baseOid = pack.subarray(off, off + 20).toString('hex'); off += 20;
    } else if (!CODE_TYPE[typeCode]) throw new Error('bad pack type');
    const inflated = zlibInflate(pack.subarray(off, trailerStart), { info: true }) as unknown as { buffer: Buffer; engine: { bytesWritten: number } };
    if (inflated.buffer.length !== hv.size) throw new Error('pack entry size mismatch');
    e.data = inflated.buffer;
    off += inflated.engine.bytesWritten;
    raw.push(e); byOffset.set(entryOff, e);
  }
  const objects = new Map<string, GitObj>();
  const resolvedByOid = new Map<string, Raw>();
  let deltaCount = 0;
  const resolving = new Set<number>();
  const resolve = (e: Raw): GitObj & { oid: string } => {
    if (e.resolved && e.oid) return { ...e.resolved, oid: e.oid };
    if (resolving.has(e.offset)) throw new Error('delta cycle');
    resolving.add(e.offset);
    let type: ObjType; let data: Buffer;
    if (e.typeCode === 6) {
      const base = resolve(byOffset.get(e.baseOffset!)!);
      type = base.type; data = applyDelta(base.data, e.data);
    } else if (e.typeCode === 7) {
      const be = resolvedByOid.get(e.baseOid!);
      if (be) { const base = resolve(be); type = base.type; data = applyDelta(base.data, e.data); }
      else {
        const ext = resolveBase?.(e.baseOid!);
        if (!ext) throw new Error('ref-delta base missing');
        type = ext.type; data = applyDelta(ext.data, e.data);
      }
    } else { type = CODE_TYPE[e.typeCode]; data = e.data; }
    const oid = hashObject(type, data);
    e.resolved = { type, data }; e.oid = oid;
    resolvedByOid.set(oid, e);
    resolving.delete(e.offset);
    return { type, data, oid };
  };
  for (const e of raw) {
    const r = resolve(e);
    if (e.typeCode === 6 || e.typeCode === 7) deltaCount++;
    if (!objects.has(r.oid)) objects.set(r.oid, { type: r.type, data: r.data });
  }
  return { objects, deltaCount };
}

// ---------------------------------------------------------------- pack write (no deltas)
function packObjHeader(type: ObjType, size: number): Buffer {
  const code = TYPE_CODE[type];
  const bytes: number[] = [];
  let rem = size;
  let first = (code << 4) | (rem & 0x0f);
  rem = Math.floor(rem / 16);
  while (rem > 0) { bytes.push(first | 0x80); first = rem & 0x7f; rem = Math.floor(rem / 128); }
  bytes.push(first);
  return Buffer.from(bytes);
}
/** Build a pack from plain objects; `level` 1 is fastest and small enough here. */
export function writePack(objs: Array<{ type: ObjType; data: Buffer }>, level = 1): Buffer {
  const parts: Buffer[] = [];
  const header = Buffer.allocUnsafe(12);
  header.write('PACK', 0, 4, 'ascii');
  header.writeUInt32BE(2, 4);
  header.writeUInt32BE(objs.length, 8);
  parts.push(header);
  for (const o of objs) {
    parts.push(packObjHeader(o.type, o.data.length));
    parts.push(zlibDeflate(o.data, { level }));
  }
  const body = Buffer.concat(parts);
  return Buffer.concat([body, createHash('sha1').update(body).digest()]);
}

// ---------------------------------------------------------------- fetch planning
export interface FetchPlan { send: Set<string>; shallow: string[]; unshallow: string[] }
/**
 * Compute what a fetch/upload-pack must send.
 * wants: wanted oids; haves: oids the client holds; shallowBounds: the client's
 * current shallow boundary commits; depth: null = full, else max commit generations.
 */
export function planFetch(
  store: Map<string, GitObj>,
  wants: string[],
  haves: string[],
  shallowBounds: string[],
  depth: number | null,
): FetchPlan {
  const lookup = (oid: string): GitObj | undefined => store.get(oid);
  const shallowSet = new Set(shallowBounds);
  let included = new Set<string>();
  let boundary = new Set<string>();
  if (depth !== null) {
    // Depth-limited walk from wants over commits; include each reached commit's
    // tree+blobs fully, stop expanding parents past depth-1.
    const dist = new Map<string, number>();
    const queue: string[] = [];
    for (const w of wants) {
      const o = store.get(w);
      if (!o) continue;
      if (o.type === 'tag') { const t = parseTag(o.data).object; dist.set(w, 0); if (!dist.has(t)) { dist.set(t, 0); queue.push(t); } }
      else if (o.type === 'commit') { dist.set(w, 0); queue.push(w); }
      else { dist.set(w, 0); }
    }
    const commitAt = new Map<string, number>();
    while (queue.length) {
      const cur = queue.shift()!;
      const d = dist.get(cur)!;
      const o = store.get(cur);
      if (!o) continue;
      if (o.type === 'commit') {
        commitAt.set(cur, d);
        if (d < depth - 1) {
          for (const p of parseCommit(o.data).parents) {
            if (!dist.has(p)) { dist.set(p, d + 1); queue.push(p); }
          }
        }
      }
    }
    // boundary = included commits whose parents are not included
    for (const [c, d] of commitAt) {
      const o = store.get(c)!;
      const parents = parseCommit(o.data).parents;
      if (d === depth - 1 || parents.length === 0 || parents.every((p) => !commitAt.has(p))) boundary.add(c);
    }
    // included objects: for each included commit, its tree closure; plus tags wanted.
    // Closure restricted to included commits (parents past the depth cut are excluded).
    const keep = new Set<string>();
    const addClosure = (oid: string): void => {
      const o = store.get(oid);
      if (!o || keep.has(oid)) return;
      keep.add(oid);
      if (o.type === 'commit') {
        if (commitAt.has(oid)) {
          addClosure(parseCommit(o.data).tree);
          for (const p of parseCommit(o.data).parents) if (commitAt.has(p)) addClosure(p);
        }
      } else if (o.type === 'tag') addClosure(parseTag(o.data).object);
      else if (o.type === 'tree') for (const e of parseTree(o.data)) addClosure(e.oid);
    };
    for (const w of wants) addClosure(w);
    included = keep;
  } else {
    included = collectReachable(wants, lookup);
  }
  // Subtract what the client already has.
  if (haves.length) {
    if (depth !== null) {
      // Shallow-held haves: client holds the commit + its tree/blobs, not parents.
      const held = new Set<string>();
      for (const h of haves) {
        const o = store.get(h);
        if (!o) continue;
        if (shallowSet.has(h) && o.type === 'commit') {
          held.add(h);
          const c = parseCommit(o.data);
          for (const x of collectReachable([c.tree], lookup)) held.add(x);
        } else {
          for (const x of collectReachable([h], lookup)) held.add(x);
        }
      }
      for (const x of held) included.delete(x);
    } else {
      const held = collectReachable(haves, lookup);
      for (const x of held) included.delete(x);
    }
  }
  const unshallow: string[] = [];
  for (const b of shallowBounds) {
    if (boundary.has(b)) continue;
    // A former boundary is unshallow when it is now sent with its parents included
    // (i.e. it is in the included set and not a boundary any more).
    if (included.has(b) && !boundary.has(b)) unshallow.push(b);
  }
  return { send: included, shallow: [...boundary].sort(), unshallow: unshallow.sort() };
}
