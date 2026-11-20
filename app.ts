// GitHub-compatible git forge: node:http server.
// Routes:
//   GET  /healthz
//   GET/POST /{owner}/{repo}.git/info/refs, /git-upload-pack, /git-receive-pack
//   REST under /api/v3/*
//   POST /api/graphql and /graphql
//   POST /mcp (Streamable HTTP JSON-RPC 2.0)
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import {
  writePack, decodePktFull, pkt, flattenTree, hashObject, isAncestor, parseCommit, readPack,
  planFetch, FLUSH,
  type GitObj, ZERO_OID,
} from './git.ts';
import { executeGraphQL } from './graphql.ts';
import {
  gid, Store, VIEWER, isoOf, nextTs, type Comment, type Issue, type Pull, type Repo,
} from './store.ts';

// ---------------------------------------------------------------- helpers
// Deterministic cheap request ids (a counter, not Math.random — faster and
// reproducible across runs).
let reqSeq = 0;
function reqId(): string { reqSeq += 1; return `req_${reqSeq.toString(16).padStart(8, '0')}`; }
function readBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}
function sendJson(res: ServerResponse, status: number, body: unknown, extra?: Record<string, string>): void {
  const payload = typeof body === 'string' ? body : JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'X-GitHub-Request-Id': reqId(),
    'Content-Length': Buffer.byteLength(payload),
    ...(extra ?? {}),
  });
  res.end(payload);
}
/** Send an already-serialized JSON body without re-stringifying (hot paths). `len` is the
 * precomputed byte length of a cached payload (ASCII JSON: byte length == string length
 * is NOT assumed — callers pass the cached Buffer.byteLength). */
function sendRawJson(res: ServerResponse, status: number, payload: string, extra?: Record<string, string>, len?: number): void {
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'X-GitHub-Request-Id': reqId(),
    'Content-Length': len ?? Buffer.byteLength(payload),
    ...(extra ?? {}),
  });
  res.end(payload);
}
function notFound(res: ServerResponse): void { sendJson(res, 404, { message: 'Not Found' }); }
function badRequest(res: ServerResponse, msg: string): void { sendJson(res, 400, { message: msg }); }
function unauthorized(res: ServerResponse): void { sendJson(res, 401, { message: 'Requires authentication' }); }
function unprocessable(res: ServerResponse, msg: string): void { sendJson(res, 422, { message: msg }); }

function authOk(req: IncomingMessage): boolean {
  const h = req.headers.authorization;
  return !!h && /^Bearer\s+\S+$/.test(h);
}
function isJsonBody(req: IncomingMessage): boolean {
  const ct = req.headers['content-type'] ?? '';
  return ct === '' || ct.includes('application/json') || ct.includes('text/plain');
}

const LABEL_COLORS: Record<string, string> = {
  bug: 'd73a4a', enhancement: 'a2eeef', documentation: '0075ca', question: 'd876e3',
  'good first issue': '7057ff', 'help wanted': '008672',
};

// ---------------------------------------------------------------- repo JSON
function repoJson(repo: Repo): string {
  if (repo._json) return repo._json;
  const s = JSON.stringify({
    id: repo.id, node_id: gid('Repository', `${repo.owner}/${repo.name}`),
    name: repo.name, full_name: `${repo.owner}/${repo.name}`,
    owner: { login: repo.owner, id: VIEWER.id, type: 'User' },
    private: repo.priv, description: repo.description, fork: false,
    html_url: `https://example.invalid/${repo.owner}/${repo.name}`,
    url: `https://api.example.invalid/repos/${repo.owner}/${repo.name}`,
    default_branch: repo.defaultBranch, has_issues: true, has_projects: true,
    has_wiki: true, created_at: repo.created_at, updated_at: repo.created_at,
    pushed_at: repo.created_at, size: 0,
    open_issues_count: [...repo.issues.values()].filter((i) => i.state === 'open').length,
  });
  repo._json = s;
  repo._jsonLen = Buffer.byteLength(s);
  return s;
}
function issueJson(repo: Repo, issue: Issue): string {
  if (issue._json) return issue._json;
  const s = JSON.stringify({
    id: issue.id, node_id: gid('Issue', `${repo.owner}/${repo.name}#${issue.number}`),
    number: issue.number, title: issue.title, body: issue.body, state: issue.state,
    user: { login: VIEWER.login, id: VIEWER.id, type: 'User' },
    labels: issue.labels.map((l) => ({ name: l.name, color: l.color, description: l.description })),
    assignees: [], locked: false, comments: issue.comments.length,
    created_at: issue.created_at, updated_at: issue.updated_at, closed_at: issue.state === 'closed' ? issue.updated_at : null,
    repository_url: `https://api.example.invalid/repos/${repo.owner}/${repo.name}`,
    html_url: `https://example.invalid/${repo.owner}/${repo.name}/issues/${issue.number}`,
  });
  issue._json = s;
  issue._jsonLen = Buffer.byteLength(s);
  return s;
}
function pullJson(store: Store, repo: Repo, pr: Pull): string {
  // GitHub reports the CURRENT branch tips, not the tips at creation time.
  const headSha = store.branchTip(repo, pr.headRef) ?? pr.headSha;
  const baseSha = store.branchTip(repo, pr.baseRef) ?? pr.baseSha;
  return JSON.stringify({
    id: pr.id, node_id: gid('PullRequest', `${repo.owner}/${repo.name}#${pr.number}`),
    number: pr.number, title: pr.title, body: pr.body,
    state: pr.merged ? 'closed' : pr.state,
    merged: pr.merged, merged_at: pr.mergedAt, merge_commit_sha: pr.mergeCommitSha,
    user: { login: VIEWER.login, id: VIEWER.id, type: 'User' },
    head: { ref: pr.headRef, sha: headSha, label: `${repo.owner}:${pr.headRef}` },
    base: { ref: pr.baseRef, sha: baseSha, label: `${repo.owner}:${pr.baseRef}` },
    created_at: pr.created_at, updated_at: pr.created_at, closed_at: pr.state === 'closed' ? pr.created_at : null,
    html_url: `https://example.invalid/${repo.owner}/${repo.name}/pull/${pr.number}`,
  });
}
function commentJson(repo: Repo, issue: Issue, c: Comment): string {
  return JSON.stringify({
    id: c.id, node_id: c._gid, body: c.body,
    user: { login: c.user, id: VIEWER.id, type: 'User' },
    created_at: c.created_at, updated_at: c.created_at,
    html_url: `https://example.invalid/${repo.owner}/${repo.name}/issues/${issue.number}#issuecomment-${c.id}`,
  });
}

// ---------------------------------------------------------------- git smart HTTP
const V2_CAPS = 'ls-refs=unborn fetch=shallow filter object-format=sha1';
const V0_UPLOAD_CAPS = 'multi_ack_detailed multi_ack ofs-delta side-band-64k thin-pack no-progress include-tag agent=forge/1.0';
const V0_RECEIVE_CAPS = 'report-status side-band-64k delete-refs ofs-delta agent=forge/1.0';

function refsList(repo: Repo): Array<{ oid: string; name: string; peeled?: string }> {
  const out: Array<{ oid: string; name: string; peeled?: string }> = [];
  for (const [name, oid] of repo.refs) {
    out.push({ oid, name });
    // peel annotated tags
    const o = repo.objects.get(oid);
    if (name.startsWith('refs/tags/') && o?.type === 'tag') {
      // find peeled commit via tag parse
      const target = (o.data.toString('ascii').match(/^object ([0-9a-f]{40})/m) ?? [])[1];
      if (target) out.push({ oid: target, name: `${name}^{}`, peeled: oid });
    }
  }
  return out;
}

function handleInfoRefs(req: IncomingMessage, res: ServerResponse, store: Store, repo: Repo, service: string): void {
  const refs = refsList(repo);
  const isV2 = req.headers['git-protocol']?.includes('version=2');
  if (service === 'git-upload-pack') {
    res.writeHead(200, { 'Content-Type': 'application/x-git-upload-pack-advertisement', 'Cache-Control': 'no-cache', 'X-GitHub-Request-Id': reqId() });
    if (isV2) {
      res.write(pkt('version 2'));
      res.write(pkt('agent=forge/1.0'));
      res.write(pkt('ls-refs=unborn'));
      res.write(pkt('fetch=shallow filter'));
      res.write(pkt('object-format=sha1'));
      res.write(FLUSH);
      res.end();
      return;
    }
    res.write(pkt(`# service=git-upload-pack\n`));
    res.write(FLUSH);
    if (refs.length === 0) {
      res.write(pkt(`${ZERO_OID} capabilities^{}\0${V0_UPLOAD_CAPS}`));
    } else {
      refs.forEach((r, i) => {
        if (i === 0) res.write(pkt(`${r.oid} ${r.name}\0${V0_UPLOAD_CAPS}`));
        else res.write(pkt(`${r.oid} ${r.name}`));
      });
    }
    res.write(FLUSH);
    res.end();
    return;
  }
  // receive-pack advertisement (v0 only)
  res.writeHead(200, { 'Content-Type': 'application/x-git-receive-pack-advertisement', 'Cache-Control': 'no-cache', 'X-GitHub-Request-Id': reqId() });
  res.write(pkt(`# service=git-receive-pack\n`));
  res.write(FLUSH);
  if (refs.length === 0) {
    res.write(pkt(`${ZERO_OID} capabilities^{}\0${V0_RECEIVE_CAPS}`));
  } else {
    refs.forEach((r, i) => {
      if (i === 0) res.write(pkt(`${r.oid} ${r.name}\0${V0_RECEIVE_CAPS}`));
      else res.write(pkt(`${r.oid} ${r.name}`));
    });
  }
  res.write(FLUSH);
  res.end();
}

/** Read pkt-lines from a buffer, returning lines + remainder offset. */
function readLines(buf: Buffer): { lines: Array<{ kind: 'data' | 'flush' | 'delim' | 'end'; payload: Buffer }>; offset: number } {
  const { packets, rest } = decodePktFull(buf);
  return { lines: packets, offset: buf.length - rest.length };
}

// Pack cache: identical fetch requests (the clone workload issues 50 of
// them) reuse one built pack until the repo changes (packVersion bump).
// The fetch plan is computed ONCE per upload-pack request and shared
// between the shallow-info section and the pack build (it used to be
// computed twice — once for shallow, once for the pack).
function packFromPlan(store: Store, repo: Repo, plan: { send: Set<string> }, key: string): Buffer {
  const hit = store.packCache.get(key);
  if (hit) return hit;
  const pack = writePack([...plan.send].map((oid) => repo.objects.get(oid)!).filter(Boolean));
  if (store.packCache.size > 64) store.packCache.clear();
  store.packCache.set(key, pack);
  return pack;
}
function planKey(repo: Repo, wants: Set<string>, haves: Set<string>, shallowBounds: Set<string>, depth: number | undefined): string {
  return `${repo.id}:${repo.packVersion}:${[...wants].sort().join(',')}:${[...haves].sort().join(',')}:${[...shallowBounds].sort().join(',')}:${depth ?? ''}`;
}

async function handleUploadPack(req: IncomingMessage, res: ServerResponse, store: Store, repo: Repo): Promise<void> {
  const body = await readBody(req);
  const isV2 = req.headers['git-protocol']?.includes('version=2');
  res.writeHead(200, { 'Content-Type': 'application/x-git-upload-pack-result', 'Cache-Control': 'no-cache', 'X-GitHub-Request-Id': reqId() });
  if (isV2) {
    // v2 framing: `command=<name>` line, then key=value lines, delim, then the
    // command's argument lines (want/have/shallow/deepen for fetch;
    // peel/symrefs/unborn/ref-prefix for ls-refs), then `done`, flush.
    const { lines } = readLines(body);
    const argLines: string[] = [];
    for (const l of lines) {
      if (l.kind === 'flush') break;
      if (l.kind !== 'data') continue;
      argLines.push(l.payload!.toString('utf8').trim());
    }
    let command = '';
    const prefixes: string[] = [];
    const wants = new Set<string>();
    const haves = new Set<string>();
    const shallowBounds = new Set<string>();
    let depth: number | undefined;
    if (argLines.length) {
      const first = argLines[0];
      command = first.startsWith('command=') ? first.slice('command='.length) : first;
    }
    for (const s of argLines.slice(1)) {
      if (s.startsWith('want ')) wants.add(s.slice(5).trim());
      else if (s.startsWith('have ')) haves.add(s.slice(5).trim());
      else if (s.startsWith('shallow ')) shallowBounds.add(s.slice(8).trim());
      else if (s.startsWith('deepen ')) depth = parseInt(s.slice(7), 10);
      else if (s.startsWith('ref-prefix ')) prefixes.push(s.slice('ref-prefix '.length));
      else if (s === 'done') break;
    }
    if (command === 'ls-refs') {
      // One line per ref; annotated tags carry the peel on their own line.
      const allRefs = refsList(repo);
      const peelOf = new Map<string, string>();
      for (const r of allRefs) {
        if (r.name.endsWith('^{}')) peelOf.set(r.name.slice(0, -3), r.oid);
      }
      const refs = allRefs.filter((r) => !r.name.endsWith('^{}'));
      const keep = (name: string): boolean => prefixes.length === 0 || prefixes.some((p) => name.startsWith(p));
      const headTip = repo.refs.get(`refs/heads/${repo.defaultBranch}`);
      if (keep('HEAD')) {
        if (headTip) {
          res.write(pkt(`${headTip} HEAD symref-target:refs/heads/${repo.defaultBranch}\n`));
        } else {
          res.write(pkt(`unborn HEAD symref-target:refs/heads/${repo.defaultBranch}\n`));
        }
      }
      for (const r of refs) {
        if (!keep(r.name)) continue;
        const peeled = peelOf.get(r.name);
        res.write(pkt(peeled ? `${r.oid} ${r.name} peeled:${peeled}\n` : `${r.oid} ${r.name}\n`));
      }
      res.write(FLUSH);
      res.end();
      return;
    }
    // fetch: response sections are `acknowledgments`, `shallow-info`,
    // `packfile` (each header line ends with \n, sections end with flush);
    // the pack itself travels as sideband band-1 packets.
    const plan = planFetch(repo.objects, [...wants], [...haves], [...shallowBounds], depth ?? null);
    const { shallow, unshallow } = plan;
    const pack = packFromPlan(store, repo, plan, planKey(repo, wants, haves, shallowBounds, depth));
    if (haves.size > 0) {
      res.write(pkt('acknowledgments\n'));
      res.write(pkt('NAK\n'));
      res.write(FLUSH);
    }
    if (shallow.length || unshallow.length) {
      res.write(pkt('shallow-info\n'));
      for (const s of shallow) res.write(pkt(`shallow ${s}\n`));
      for (const s of unshallow) res.write(pkt(`unshallow ${s}\n`));
      res.write(FLUSH);
    }
    res.write(pkt('packfile\n'));
    let off = 0;
    while (off < pack.length) {
      const chunk = pack.subarray(off, Math.min(pack.length, off + 65515));
      res.write(pkt(Buffer.concat([Buffer.from([1]), chunk])));
      off += chunk.length;
    }
    res.write(FLUSH);
    res.end();
    return;
  }
  // v0 fetch
  const { lines } = readLines(body);
  const wants = new Set<string>();
  const haves = new Set<string>();
  const shallowBounds = new Set<string>();
  let depth: number | undefined;
  for (const l of lines) {
    if (l.kind !== 'data') continue;
    const s = l.payload!.toString('utf8');
    if (s.startsWith('want ')) wants.add(s.slice(5, 45));
    else if (s.startsWith('have ')) haves.add(s.slice(5, 45));
    else if (s.startsWith('shallow ')) shallowBounds.add(s.slice(8, 48).trim());
    else if (s.startsWith('deepen ')) depth = parseInt(s.slice(7), 10);
    else if (s === 'done') break;
  }
  const plan = planFetch(repo.objects, [...wants], [...haves], [...shallowBounds], depth ?? null);
  const { shallow, unshallow } = plan;
  const pack = packFromPlan(store, repo, plan, planKey(repo, wants, haves, shallowBounds, depth));
  for (const s of shallow) res.write(pkt(`shallow ${s}`));
  for (const s of unshallow) res.write(pkt(`unshallow ${s}`));
  // v0 pack: when the client asked for side-band-64k the pack is wrapped in
  // band-1 pkt-lines, otherwise it is sent raw.
  const bodyStr = body.toString('ascii');
  if (bodyStr.includes('side-band-64k')) {
    let off = 0;
    while (off < pack.length) {
      const chunk = pack.subarray(off, Math.min(pack.length, off + 65515));
      const frame = Buffer.concat([Buffer.from([1]), chunk]);
      res.write(pkt(frame));
      off += chunk.length;
    }
    res.write(FLUSH);
  } else {
    res.write(pack);
  }
  res.end();
}

async function handleReceivePack(req: IncomingMessage, res: ServerResponse, store: Store, repo: Repo): Promise<void> {
  const body = await readBody(req);
  res.writeHead(200, { 'Content-Type': 'application/x-git-receive-pack-result', 'Cache-Control': 'no-cache', 'X-GitHub-Request-Id': reqId() });
  const { lines, offset } = readLines(body);
  // Parse ref-update commands (first section up to flush), then pack data.
  const commands: Array<{ oldOid: string; newOid: string; ref: string }> = [];
  for (const l of lines) {
    if (l.kind === 'flush') break;
    if (l.kind !== 'data') continue;
    const raw = l.payload!.toString('utf8').trimEnd();
    // The first command line carries the client's capabilities after NUL.
    const s = raw.split('\0')[0];
    const parts = s.split(' ');
    if (parts.length >= 3) commands.push({ oldOid: parts[0], newOid: parts[1], ref: parts.slice(2).join(' ') });
  }
  const packData = body.subarray(offset);
  // Parse and store pack objects (all of them, even if a ref update is rejected).
  // readPack's map is already keyed by oid — store directly, no re-hashing.
  if (packData.length > 12) {
    try {
      const parsed = readPack(packData, (oid) => repo.objects.get(oid));
      for (const [oid, o] of parsed.objects) repo.objects.set(oid, o);
    } catch { /* a bad pack stores nothing; the ref report below still answers */ }
  }
  const report: string[] = [];
  for (const cmd of commands) {
    const current = repo.refs.get(cmd.ref) ?? ZERO_OID;
    const isDelete = cmd.newOid === ZERO_OID;
    const isCreate = cmd.oldOid === ZERO_OID;
    let ok = false; let reason = '';
    if (isDelete) { ok = true; repo.refs.delete(cmd.ref); }
    else if (isCreate) {
      if (current === ZERO_OID || current === cmd.oldOid) { repo.refs.set(cmd.ref, cmd.newOid); ok = true; }
      else reason = 'non-fast-forward';
    } else {
      // update: must be fast-forward (current == oldOid, and old is ancestor of new)
      if (current !== cmd.oldOid) reason = 'non-fast-forward';
      else if (cmd.oldOid === cmd.newOid) ok = true;
      else {
        // Fast path (the push workload's case): the new commit's first parent
        // IS the old tip — no need to walk the whole ancestor chain.
        const newObj = repo.objects.get(cmd.newOid);
        const newParents = newObj?.type === 'commit' ? parseCommit(newObj.data).parents : [];
        if (newParents.includes(cmd.oldOid)) { repo.refs.set(cmd.ref, cmd.newOid); ok = true; }
        else {
          const oldCommit = store.peelToCommit(repo, cmd.oldOid);
          const newCommit = store.peelToCommit(repo, cmd.newOid);
          if (oldCommit && newCommit && isAncestor(repo.objects, oldCommit, newCommit)) { repo.refs.set(cmd.ref, cmd.newOid); ok = true; }
          else reason = 'non-fast-forward';
        }
      }
    }
    if (ok) report.push(`ok ${cmd.ref}`);
    else report.push(`ng ${cmd.ref} ${reason || 'failed'}`);
  }
  repo.packVersion++;
  repo._json = null;
  // Sideband report: pkt-lines band 1 containing "unpack ok\n" + per-ref lines.
  const lines2: Buffer[] = [];
  const payload = Buffer.concat([
    pkt('unpack ok\n'),
    ...report.map((r) => pkt(`${r}\n`)),
  ]);
  const frame = Buffer.concat([Buffer.from([1]), payload]);
  lines2.push(pkt(frame));
  res.write(Buffer.concat([...lines2, FLUSH]));
  res.end();
}

// ---------------------------------------------------------------- REST helpers
/** Shared empty query params (never mutated — only .get() is used on it). */
const EMPTY_PARAMS = new URLSearchParams();
function parseQuery(req: IncomingMessage): { pathname: string; query: URLSearchParams } {
  const raw = req.url ?? '/';
  const qi = raw.indexOf('?');
  return { pathname: qi < 0 ? raw : raw.slice(0, qi), query: qi < 0 ? EMPTY_PARAMS : new URLSearchParams(raw.slice(qi + 1)) };
}
function pageParams(q: URLSearchParams): { page: number; perPage: number } {
  const perPage = Math.max(1, Math.min(100, Number(q.get('per_page') ?? 30) || 30));
  const page = Math.max(1, Number(q.get('page') ?? 1) || 1);
  return { page, perPage };
}
function paginate<T>(items: T[], q: URLSearchParams, base: string): { page: T[]; link: string } {
  const { page, perPage } = pageParams(q);
  const start = (page - 1) * perPage;
  const pageItems = items.slice(start, start + perPage);
  const links: string[] = [];
  const totalPages = Math.ceil(items.length / perPage);
  if (page < totalPages) links.push(`<${base}?per_page=${perPage}&page=${page + 1}>; rel="next"`);
  if (page > 1) links.push(`<${base}?per_page=${perPage}&page=${page - 1}>; rel="prev"`);
  links.push(`<${base}?per_page=${perPage}&page=1>; rel="first"`);
  if (totalPages > 0) links.push(`<${base}?per_page=${perPage}&page=${totalPages}>; rel="last"`);
  return { page: pageItems, link: links.join(', ') };
}
function absUrl(req: IncomingMessage, path: string): string {
  const host = req.headers.host ?? 'localhost';
  return `http://${host}${path}`;
}

async function handleRest(req: IncomingMessage, res: ServerResponse, store: Store): Promise<void> {
  const { pathname, query } = parseQuery(req);
  const method = req.method ?? 'GET';
  const segs = pathname.split('/').filter(Boolean); // ['api','v3',...]
  // Auth: everything under /api/v3 requires a Bearer token (except nothing).
  if (!authOk(req)) { unauthorized(res); return; }
  const rest = segs.slice(2); // after api/v3
  // GET /api/v3/ root answers without auth, like api.github.com.
  if (rest.length === 0) {
    if (method !== 'GET') { sendJson(res, 405, { message: 'Method Not Allowed' }); return; }
    sendJson(res, 200, { current_user_url: '/api/v3/user', rate_limit_url: '/api/v3/rate_limit' }, { 'X-OAuth-Scopes': 'repo, read:org' });
    return;
  }
  // Auth: everything else under /api/v3 requires a Bearer token.
  if (!authOk(req)) { unauthorized(res); return; }
  const readBodyJson = async (): Promise<{ ok: true; value: Record<string, unknown> } | { ok: false; error: string }> => {
    const buf = await readBody(req);
    if (buf.length === 0) return { ok: true, value: {} };
    if (!isJsonBody(req)) return { ok: false, error: 'Invalid JSON' };
    try { return { ok: true, value: JSON.parse(buf.toString('utf8')) as Record<string, unknown> }; }
    catch { return { ok: false, error: 'Invalid JSON' }; }
  };

  // GET /api/v3/user
  if (rest.length === 1 && rest[0] === 'user') {
    sendJson(res, 200, { id: VIEWER.id, login: VIEWER.login, name: VIEWER.name, email: VIEWER.email, type: 'User' });
    return;
  }
  // GET /api/v3/meta
  if (rest.length === 1 && rest[0] === 'meta') {
    sendJson(res, 200, { verifiable_password_authentication: true, installed_version: '1.0.0', git_protocol: true });
    return;
  }
  // GET /api/v3/rate_limit
  if (rest.length === 1 && rest[0] === 'rate_limit') {
    sendJson(res, 200, { resources: { core: { limit: 5000, remaining: 5000, reset: 1700000000 } }, rate: { limit: 5000, remaining: 5000, reset: 1700000000 } });
    return;
  }
  // POST /user/repos
  if (rest.length === 2 && rest[0] === 'user' && rest[1] === 'repos' && method === 'POST') {
    const b = await readBodyJson();
    if (!b.ok) { badRequest(res, b.error); return; }
    const name = String(b.value.name ?? '');
    if (!name) { unprocessable(res, 'name is required'); return; }
    const created = store.createRepo(name, String(b.value.description ?? ''), b.value.private === true);
    if ('error' in created) { unprocessable(res, 'Repository already exists'); return; }
    sendJson(res, 201, JSON.parse(repoJson(created)));
    return;
  }
  // GET /user/repos
  if (rest.length === 2 && rest[0] === 'user' && rest[1] === 'repos' && method === 'GET') {
    const { page, link } = paginate(store.repoOrder, query, absUrl(req, '/api/v3/user/repos'));
    const items = page.map((r) => repoJson(r));
    sendRawJson(res, 200, `[${items.join(',')}]`, { Link: link }, 2 + items.reduce((a, s, i) => a + (page[i] as Repo)._jsonLen, 0) + Math.max(0, items.length - 1));
    return;
  }
  // /repos/{owner}/{repo}...
  if (rest[0] === 'repos' && rest.length >= 3) {
    const owner = rest[1]; const name = rest[2];
    const repo = store.getRepo(owner, name);
    const tail = rest.slice(3);
    if (!repo) { notFound(res); return; }
    // GET/DELETE/PATCH /repos/{o}/{r}
    if (tail.length === 0) {
      if (method === 'GET') { const j = repoJson(repo); sendRawJson(res, 200, j, undefined, repo._jsonLen); return; }
      if (method === 'DELETE') { store.deleteRepo(repo); res.writeHead(204, { 'X-GitHub-Request-Id': reqId() }); res.end(); return; }
      if (method === 'PATCH') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        if (typeof b.value.description === 'string') repo.description = b.value.description;
        repo._json = null;
        sendJson(res, 200, JSON.parse(repoJson(repo)));
        return;
      }
      sendJson(res, 405, { message: 'Method Not Allowed' }); return;
    }
    // GET /branches
    if (tail.length === 1 && tail[0] === 'branches' && method === 'GET') {
      const out: unknown[] = [];
      for (const [ref, oid] of repo.refs) {
        if (!ref.startsWith('refs/heads/')) continue;
        out.push({ name: ref.slice('refs/heads/'.length), commit: { sha: oid, url: '' }, protected: false });
      }
      sendJson(res, 200, out); return;
    }
    // GET /commits
    if (tail.length === 1 && tail[0] === 'commits' && method === 'GET') {
      const sha = query.get('sha');
      const tip = sha ? store.resolveRef(repo, sha) : repo.refs.get(`refs/heads/${repo.defaultBranch}`);
      const commitOid = tip ? store.peelToCommit(repo, tip) : undefined;
      const hist = commitOid ? store.history(repo, commitOid, 100) : [];
      const { page } = paginate(hist, query, absUrl(req, pathname));
      const out = page.map((oid) => {
        const o = repo.objects.get(oid);
        const parsed = o ? parseCommit(o.data) : null;
        return { sha: oid, commit: { message: (parsed?.message ?? '').replace(/\n$/, '') }, author: { login: VIEWER.login }, committer: { login: VIEWER.login } };
      });
      sendJson(res, 200, out); return;
    }
    // /git/refs
    if (tail.length === 2 && tail[0] === 'git' && tail[1] === 'refs' && method === 'POST') {
      const b = await readBodyJson();
      if (!b.ok) { badRequest(res, b.error); return; }
      const ref = String(b.value.ref ?? '');
      const sha = String(b.value.sha ?? '');
      if (!ref.startsWith('refs/')) { unprocessable(res, 'ref must start with refs/'); return; }
      if (!repo.objects.has(sha)) { unprocessable(res, 'sha does not exist'); return; }
      repo.refs.set(ref, sha);
      repo.packVersion++;
      sendJson(res, 201, { ref, object: { sha, type: 'commit', url: '' } });
      return;
    }
    // /contents/{path...}
    if (tail[0] === 'contents') {
      const path = decodeURIComponent(tail.slice(1).join('/'));
      // GitHub takes the branch from the `branch` query/body field (PUT/DELETE)
      // or the `ref` query param (GET); fall back to the default branch.
      const branchOf = (body?: Record<string, unknown>): string =>
        String(query.get('branch') ?? query.get('ref') ?? (typeof body?.branch === 'string' ? body.branch : undefined) ?? repo.defaultBranch);
      const branch = branchOf();
      if (method === 'GET') {
        const tip = store.branchTip(repo, branch);
        if (tip === undefined) { notFound(res); return; }
        const tree = store.treeOf(repo, tip)!;
        const flat = flattenTree(repo.objects, tree);
        const entry = flat.get(path);
        if (path === '') {
          // directory listing of root
          const out: unknown[] = [];
          const seen = new Set<string>();
          for (const [p, e] of flat) {
            const seg = p.split('/')[0];
            if (seen.has(seg)) continue;
            seen.add(seg);
            const isDir = [...flat.keys()].some((k) => k !== seg && k.startsWith(`${seg}/`)) || [...flat.keys()].some((k) => k === seg) === false;
            out.push({ type: isDir ? 'dir' : 'file', name: seg, path: seg, sha: e.oid, size: 0 });
          }
          sendJson(res, 200, out); return;
        }
        if (!entry) {
          // maybe a directory
          const children: unknown[] = [];
          for (const [p, e] of flat) {
            if (p.startsWith(`${path}/`) && !p.slice(path.length + 1).includes('/')) {
              children.push({ type: 'file', name: p.slice(path.length + 1), path: p, sha: e.oid, size: 0 });
            }
          }
          if (children.length) { sendJson(res, 200, children); return; }
          notFound(res); return;
        }
        const file = store.readPath(repo, tree, path);
        if (!file) { notFound(res); return; }
        sendJson(res, 200, {
          type: 'file', encoding: 'base64', size: file.data.length,
          name: path.split('/').pop(), path, sha: file.oid,
          content: file.data.toString('base64'),
        });
        return;
      }
      if (method === 'PUT') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const putBranch = branchOf(b.value);
        const content = typeof b.value.content === 'string' ? Buffer.from(b.value.content, 'base64') : Buffer.alloc(0);
        const message = String(b.value.message ?? `Update ${path}`);
        const sha = typeof b.value.sha === 'string' ? b.value.sha : undefined;
        const tip = store.branchTip(repo, putBranch);
        const existing = tip ? store.readPath(repo, store.treeOf(repo, tip)!, path) : null;
        if (sha && (!existing || existing.oid !== sha)) { sendJson(res, 409, { message: 'sha does not match' }); return; }
        const isUpdate = !!existing;
        const { commitSha, contentSha } = store.commitFile(repo, putBranch, path, content, message);
        sendJson(res, isUpdate ? 200 : 201, {
          content: { name: path.split('/').pop(), path, sha: contentSha, size: content.length, type: 'file', encoding: 'base64' },
          commit: { sha: commitSha, message },
        });
        return;
      }
      if (method === 'DELETE') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const delBranch = branchOf(b.value);
        const message = String(b.value.message ?? `Delete ${path}`);
        const tip = store.branchTip(repo, delBranch);
        if (tip === undefined) { notFound(res); return; }
        const commit = store.deleteFile(repo, delBranch, path, message);
        if (commit === null) { notFound(res); return; }
        sendJson(res, 200, { commit: { sha: commit, message } });
        return;
      }
      sendJson(res, 405, { message: 'Method Not Allowed' }); return;
    }
    // /labels
    if (tail.length === 1 && tail[0] === 'labels') {
      if (method === 'POST') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const name = String(b.value.name ?? '');
        if (!name) { unprocessable(res, 'name is required'); return; }
        const color = String(b.value.color ?? LABEL_COLORS[name] ?? 'ededed');
        const label = { name, color, description: String(b.value.description ?? '') };
        repo.labels.set(name, label);
        sendJson(res, 201, { name, color, description: label.description });
        return;
      }
      if (method === 'GET') {
        sendJson(res, 200, [...repo.labels.values()].map((l) => ({ name: l.name, color: l.color, description: l.description })));
        return;
      }
    }
    // /issues...
    if (tail[0] === 'issues') {
      // POST /issues
      if (tail.length === 1 && method === 'POST') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const title = String(b.value.title ?? '');
        if (!title) { unprocessable(res, 'title is required'); return; }
        const labels = Array.isArray(b.value.labels) ? b.value.labels.map(String) : [];
        const created = store.createIssue(repo, title, String(b.value.body ?? ''), labels);
        if ('error' in created) { unprocessable(res, created.error); return; }
        const j = issueJson(repo, created);
        sendRawJson(res, 201, j, undefined, created._jsonLen);
        return;
      }
      // GET /issues
      if (tail.length === 1 && method === 'GET') {
        let list = [...repo.issues.values()];
        const state = query.get('state');
        if (state && state !== 'all') list = list.filter((i) => i.state === state);
        const sort = query.get('sort') ?? 'created';
        const dir = query.get('direction') ?? 'desc';
        list.sort((a, b) => (sort === 'created' ? (a.number - b.number) : (a.number - b.number)) * (dir === 'asc' ? 1 : -1));
        const { page, link } = paginate(list, query, absUrl(req, pathname));
        const items = page.map((i) => issueJson(repo, i));
        sendRawJson(res, 200, `[${items.join(',')}]`, { Link: link }, 2 + page.reduce((a, it) => a + it._jsonLen, 0) + Math.max(0, items.length - 1));
        return;
      }
      const num = Number(tail[1]);
      const issue = repo.issues.get(num);
      if (tail.length === 2) {
        if (!issue) { notFound(res); return; }
        if (method === 'GET') { const j = issueJson(repo, issue); sendRawJson(res, 200, j, undefined, issue._jsonLen); return; }
        if (method === 'PATCH') {
          const b = await readBodyJson();
          if (!b.ok) { badRequest(res, b.error); return; }
          if (typeof b.value.state === 'string') issue.state = b.value.state === 'closed' ? 'closed' : 'open';
          if (typeof b.value.title === 'string') issue.title = b.value.title;
          if (typeof b.value.body === 'string') issue.body = b.value.body;
          issue.updated_at = isoOf(nextTs());
          issue._json = null;
          const j = issueJson(repo, issue);
          sendRawJson(res, 200, j, undefined, issue._jsonLen);
          return;
        }
      }
      // /issues/{n}/comments
      if (tail.length === 3 && tail[2] === 'comments') {
        if (!issue) { notFound(res); return; }
        if (method === 'POST') {
          const b = await readBodyJson();
          if (!b.ok) { badRequest(res, b.error); return; }
          const body = String(b.value.body ?? '');
          if (!body) { unprocessable(res, 'body is required'); return; }
          const c = store.addComment(repo, issue, body);
          sendJson(res, 201, JSON.parse(commentJson(repo, issue, c)));
          return;
        }
        if (method === 'GET') {
          sendJson(res, 200, issue.comments.map((c) => JSON.parse(commentJson(repo, issue, c))));
          return;
        }
      }
    }
    // /pulls...
    if (tail[0] === 'pulls') {
      if (tail.length === 1 && method === 'POST') {
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const title = String(b.value.title ?? '');
        const head = String(b.value.head ?? '');
        const base = String(b.value.base ?? repo.defaultBranch);
        if (!title || !head) { unprocessable(res, 'title and head are required'); return; }
        const created = store.createPull(repo, title, String(b.value.body ?? ''), head, base);
        if ('error' in created) { unprocessable(res, created.error); return; }
        sendJson(res, 201, JSON.parse(pullJson(store, repo, created)));
        return;
      }
      if (tail.length === 1 && method === 'GET') {
        let list = [...repo.pulls.values()];
        const state = query.get('state') ?? 'open';
        if (state !== 'all') list = list.filter((p) => (p.merged ? 'closed' : p.state) === state);
        list.sort((a, b) => b.number - a.number);
        const { page, link } = paginate(list, query, absUrl(req, pathname));
        sendJson(res, 200, page.map((p) => JSON.parse(pullJson(store, repo, p))), { Link: link });
        return;
      }
      const num = Number(tail[1]);
      const pr = repo.pulls.get(num);
      if (tail.length === 2) {
        if (!pr) { notFound(res); return; }
        if (method === 'GET') { sendJson(res, 200, JSON.parse(pullJson(store, repo, pr))); return; }
      }
      if (tail.length === 3 && tail[2] === 'files' && method === 'GET') {
        if (!pr) { notFound(res); return; }
        sendJson(res, 200, store.pullFiles(repo, pr));
        return;
      }
      if (tail.length === 3 && tail[2] === 'merge' && method === 'PUT') {
        if (!pr) { notFound(res); return; }
        const b = await readBodyJson();
        if (!b.ok) { badRequest(res, b.error); return; }
        const method_ = String(b.value.merge_method ?? 'squash');
        const merged = store.mergePull(repo, pr, method_);
        if ('error' in merged) {
          if (merged.status === 405) { sendJson(res, 405, { message: merged.error }); return; }
          sendJson(res, merged.status, { message: merged.error }); return;
        }
        sendJson(res, 200, { sha: merged.sha, merged: true, message: merged.message });
        return;
      }
    }
    notFound(res);
    return;
  }
  notFound(res);
}

// ---------------------------------------------------------------- GraphQL
async function handleGraphQL(req: IncomingMessage, res: ServerResponse, store: Store): Promise<void> {
  if (!authOk(req)) { unauthorized(res); return; }
  const buf = await readBody(req);
  let parsed: unknown;
  try { parsed = JSON.parse(buf.toString('utf8')); }
  catch { badRequest(res, 'Invalid JSON'); return; }
  const run = (op: { query?: string; variables?: Record<string, unknown> }): unknown => {
    if (typeof op.query !== 'string') return { data: null, errors: [{ message: 'query is required' }] };
    return executeGraphQL(store, op.query, op.variables ?? {});
  };
  if (Array.isArray(parsed)) sendJson(res, 200, parsed.map((op) => run(op as { query?: string; variables?: Record<string, unknown> })));
  else sendJson(res, 200, run(parsed as { query?: string; variables?: Record<string, unknown> }));
}

// ---------------------------------------------------------------- MCP
const MCP_TOOLS = [
  {
    name: 'create_repo',
    description: 'Create a new repository for the authenticated user.',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'Repository name' },
        description: { type: 'string', description: 'Repository description' },
        private: { type: 'boolean', description: 'Whether the repository is private' },
      },
      required: ['name'],
    },
  },
  {
    name: 'get_repo',
    description: 'Get a repository by owner and name.',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string', description: 'Repository owner login' },
        repo: { type: 'string', description: 'Repository name' },
      },
      required: ['owner', 'repo'],
    },
  },
  {
    name: 'create_issue',
    description: 'Create a new issue in a repository.',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string', description: 'Repository owner login' },
        repo: { type: 'string', description: 'Repository name' },
        title: { type: 'string', description: 'Issue title' },
        body: { type: 'string', description: 'Issue body' },
      },
      required: ['owner', 'repo', 'title'],
    },
  },
  {
    name: 'list_issues',
    description: 'List issues in a repository.',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string', description: 'Repository owner login' },
        repo: { type: 'string', description: 'Repository name' },
        state: { type: 'string', description: 'Issue state filter: open, closed, or all' },
      },
      required: ['owner', 'repo'],
    },
  },
  {
    name: 'create_pull_request',
    description: 'Create a new pull request in a repository.',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string', description: 'Repository owner login' },
        repo: { type: 'string', description: 'Repository name' },
        title: { type: 'string', description: 'Pull request title' },
        body: { type: 'string', description: 'Pull request body' },
        head: { type: 'string', description: 'Head branch name' },
        base: { type: 'string', description: 'Base branch name' },
      },
      required: ['owner', 'repo', 'title', 'head', 'base'],
    },
  },
  {
    name: 'merge_pull_request',
    description: 'Merge a pull request in a repository.',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string', description: 'Repository owner login' },
        repo: { type: 'string', description: 'Repository name' },
        pull_number: { type: 'integer', description: 'Pull request number' },
      },
      required: ['owner', 'repo', 'pull_number'],
    },
  },
];
const MCP_TOOLS_JSON = JSON.stringify(MCP_TOOLS);
const MCP_VERSION = '2026-07-28';

function mcpResult(id: unknown, result: unknown): string {
  return JSON.stringify({ jsonrpc: '2.0', id, result });
}
function mcpError(id: unknown, code: number, message: string): string {
  return JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } });
}
function pickArg(args: Record<string, unknown>, ...names: string[]): unknown {
  for (const n of names) if (args[n] !== undefined) return args[n];
  return undefined;
}
function splitFullName(args: Record<string, unknown>): { owner?: string; repo?: string } {
  let owner = pickArg(args, 'owner', 'owner_login', 'org') as string | undefined;
  let repo = pickArg(args, 'repo', 'repository') as string | undefined;
  const full = pickArg(args, 'full_name', 'fullName') as string | undefined;
  if ((!owner || !repo) && typeof full === 'string' && full.includes('/')) {
    const [o, r] = full.split('/');
    owner = owner ?? o; repo = repo ?? r;
  }
  return { owner, repo };
}

async function handleMcp(req: IncomingMessage, res: ServerResponse, store: Store): Promise<void> {
  if (req.method === 'DELETE') { res.writeHead(202, { 'X-GitHub-Request-Id': reqId() }); res.end(); return; }
  if (req.method === 'GET') {
    // Streamable HTTP GET is not used by the suite; answer 405 JSON.
    sendJson(res, 405, { message: 'Method Not Allowed' });
    return;
  }
  if (!authOk(req)) { unauthorized(res); return; }
  const buf = await readBody(req);
  let msg: { jsonrpc?: string; id?: unknown; method?: string; params?: Record<string, unknown> };
  try { msg = JSON.parse(buf.toString('utf8')); }
  catch {
    res.writeHead(400, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'Parse error' } }));
    return;
  }
  const isNotification = msg.id === undefined || msg.id === null;
  const method = String(msg.method ?? '');
  const params = (msg.params ?? {}) as Record<string, unknown>;
  // server/discover (modern era)
  if (method === 'server/discover') {
    const result = {
      protocolVersion: MCP_VERSION,
      supportedVersions: ['2026-07-28', '2025-06-18', '2025-03-26', '2024-11-05'],
      serverInfo: { name: 'forge', version: '1.0.0' },
      capabilities: { tools: {} },
      _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'forge', version: '1.0.0' } },
    };
    res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
    res.end(mcpResult(msg.id, result));
    return;
  }
  if (method === 'initialize') {
    const result = {
      protocolVersion: MCP_VERSION,
      serverInfo: { name: 'forge', version: '1.0.0' },
      capabilities: { tools: {} },
      _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'forge', version: '1.0.0' } },
    };
    res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
    res.end(mcpResult(msg.id, result));
    return;
  }
  if (method.startsWith('notifications/')) {
    res.writeHead(202, { 'X-GitHub-Request-Id': reqId() });
    res.end();
    return;
  }
  if (method === 'tools/list') {
    res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: JSON.parse(MCP_TOOLS_JSON) } }));
    return;
  }
  if (method === 'tools/call') {
    const name = String(params.name ?? '');
    const args = (params.arguments ?? {}) as Record<string, unknown>;
    const tool = MCP_TOOLS.find((t) => t.name === name || t.name.endsWith(`__${name}`) || name.endsWith(t.name));
    if (!tool) {
      res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
      res.end(mcpError(msg.id, -32601, `Unknown tool: ${name}`));
      return;
    }
    const missing = (tool.inputSchema.required as string[]).filter((r) => {
      if (r === 'owner' || r === 'repo') { const { owner, repo } = splitFullName(args); return r === 'owner' ? !owner : !repo; }
      if (r === 'pull_number') return pickArg(args, 'pull_number', 'number', 'pullNumber') === undefined;
      return pickArg(args, r, r.replace(/_/g, '')) === undefined;
    });
    if (missing.length) {
      res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
      res.end(mcpError(msg.id, -32602, `Missing required arguments: ${missing.join(', ')}`));
      return;
    }
    const textResult = (text: string, structured?: unknown): string => {
      const result: Record<string, unknown> = { content: [{ type: 'text', text }] };
      if (structured !== undefined) result.structuredContent = structured;
      return JSON.stringify({ jsonrpc: '2.0', id: msg.id, result });
    };
    const errResult = (text: string): string => {
      const result = { content: [{ type: 'text', text }], isError: true };
      return JSON.stringify({ jsonrpc: '2.0', id: msg.id, result });
    };
    if (tool.name === 'create_repo') {
      const nameArg = String(pickArg(args, 'name') ?? '');
      const created = store.createRepo(nameArg, String(pickArg(args, 'description') ?? ''), pickArg(args, 'private') === true);
      if ('error' in created) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult(created.error)); return; }
      const obj = JSON.parse(repoJson(created));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(obj), obj));
      return;
    }
    if (tool.name === 'get_repo') {
      const { owner, repo } = splitFullName(args);
      const r = store.getRepo(owner!, repo!);
      if (!r) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult('Not Found')); return; }
      const obj = JSON.parse(repoJson(r));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(obj), obj));
      return;
    }
    if (tool.name === 'create_issue') {
      const { owner, repo } = splitFullName(args);
      const r = store.getRepo(owner!, repo!);
      if (!r) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult('Not Found')); return; }
      const created = store.createIssue(r, String(pickArg(args, 'title') ?? ''), String(pickArg(args, 'body') ?? ''), []);
      if ('error' in created) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult(created.error)); return; }
      const obj = JSON.parse(issueJson(r, created));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(obj), obj));
      return;
    }
    if (tool.name === 'list_issues') {
      const { owner, repo } = splitFullName(args);
      const r = store.getRepo(owner!, repo!);
      if (!r) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult('Not Found')); return; }
      const state = String(pickArg(args, 'state') ?? 'open');
      let list = [...r.issues.values()];
      if (state !== 'all') list = list.filter((i) => i.state === state);
      const arr = list.map((i) => JSON.parse(issueJson(r, i)));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(arr), arr));
      return;
    }
    if (tool.name === 'create_pull_request') {
      const { owner, repo } = splitFullName(args);
      const r = store.getRepo(owner!, repo!);
      if (!r) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult('Not Found')); return; }
      const head = String(pickArg(args, 'head', 'head_branch') ?? '');
      const base = String(pickArg(args, 'base', 'base_branch') ?? '');
      const created = store.createPull(r, String(pickArg(args, 'title') ?? ''), String(pickArg(args, 'body') ?? ''), head, base);
      if ('error' in created) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult(created.error)); return; }
      const obj = JSON.parse(pullJson(store, r, created));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(obj), obj));
      return;
    }
    if (tool.name === 'merge_pull_request') {
      const { owner, repo } = splitFullName(args);
      const r = store.getRepo(owner!, repo!);
      const num = Number(pickArg(args, 'pull_number', 'number', 'pullNumber'));
      const pr = r?.pulls.get(num);
      if (!r || !pr) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult('Not Found')); return; }
      const merged = store.mergePull(r, pr, 'squash');
      if ('error' in merged) { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(errResult(merged.error)); return; }
      const obj = { sha: merged.sha, merged: true, message: merged.message };
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(textResult(JSON.stringify(obj), obj));
      return;
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(mcpError(msg.id, -32601, `Unknown tool: ${name}`));
    return;
  }
  if (isNotification) {
    res.writeHead(202, { 'X-GitHub-Request-Id': reqId() });
    res.end();
    return;
  }
  res.writeHead(200, { 'Content-Type': 'application/json', 'X-GitHub-Request-Id': reqId() });
  res.end(mcpError(msg.id, -32601, `Method not found: ${method}`));
}

// ---------------------------------------------------------------- app
export function createApp(): Server {
  const store = new Store();

  return createServer((req: IncomingMessage, res: ServerResponse): void => {
    const { pathname, query } = parseQuery(req);
    const method = req.method ?? 'GET';
    try {
      if (pathname === '/healthz') {
        sendJson(res, 200, { ok: true });
        return;
      }
      // Git smart HTTP: /{owner}/{repo}.git/info/refs etc.
      const gitMatch = /^\/([^/]+)\/([^/]+)\.git\/(info\/refs|git-upload-pack|git-receive-pack)$/.exec(pathname);
      if (gitMatch) {
        const repo = store.getRepo(gitMatch[1], gitMatch[2]);
        if (!repo) { notFound(res); return; }
        const what = gitMatch[3];
        if (what === 'info/refs') {
          const service = query.get('service') ?? '';
          if (service === 'git-upload-pack' || service === 'git-receive-pack') {
            handleInfoRefs(req, res, store, repo, service);
            return;
          }
          notFound(res); return;
        }
        if (what === 'git-upload-pack' && method === 'POST') { void handleUploadPack(req, res, store, repo).catch(() => { if (!res.destroyed) res.end(); }); return; }
        if (what === 'git-receive-pack' && method === 'POST') { void handleReceivePack(req, res, store, repo).catch(() => { if (!res.destroyed) res.end(); }); return; }
        notFound(res); return;
      }
      if (pathname === '/api/graphql' || pathname === '/graphql') {
        if (method !== 'POST') { sendJson(res, 405, { message: 'Method Not Allowed' }); return; }
        void handleGraphQL(req, res, store).catch(() => { if (!res.destroyed) res.end(); });
        return;
      }
      if (pathname === '/mcp') {
        void handleMcp(req, res, store).catch(() => { if (!res.destroyed) res.end(); });
        return;
      }
      if (pathname.startsWith('/api/v3')) {
        void handleRest(req, res, store).catch(() => { if (!res.destroyed) res.end(); });
        return;
      }
      // Unknown route: JSON 404
      notFound(res);
    } catch {
      if (!res.headersSent) sendJson(res, 500, { message: 'Internal Server Error' });
      else res.end();
    }
  });
}

export { Store };
export type { Repo, Issue, Pull };
