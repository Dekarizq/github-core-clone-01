// In-memory forge store: repositories, git objects/refs, issues, pull requests.
// Everything deterministic: ids and timestamps come from counters seeded at
// fixed values, never from the wall clock or Math.random.
import {
  ZERO_OID, buildCommit, buildTree, collectReachable, flattenTree, hashObject,
  isAncestor, isOid, mergeBase, parseCommit, parseTag, parseTree, putObj,
  type GitObj, type ObjType,
} from './git.ts';

export const VIEWER = { id: 1000, login: 'adaiversen', name: 'ada iversen', email: 'adaiversen@example.invalid' };
export const IDENTITY = 'ada iversen <adaiversen@example.invalid>';
/** Fixed epoch for server-made commits; +1s per server commit keeps bytes deterministic. */
const EPOCH = 1700000000;

let seq = 0;
export function nextTs(): number { seq += 1; return EPOCH + seq; }
export function isoOf(ts: number): string { return new Date(ts * 1000).toISOString().replace('.000Z', 'Z'); }

export interface Label { name: string; color: string; description: string }
export interface Comment { id: number; body: string; user: string; created_at: string; _gid: string }
export interface Issue {
  id: number; number: number; title: string; body: string; state: 'open' | 'closed';
  labels: Label[]; comments: Comment[]; seq: number; created_at: string; updated_at: string;
  _json: string | null; _jsonLen: number;
}
export interface Pull {
  id: number; number: number; title: string; body: string; state: 'open' | 'closed';
  headRef: string; baseRef: string; headSha: string; baseSha: string;
  merged: boolean; mergeCommitSha: string | null; mergedAt: string | null;
  seq: number; created_at: string;
}
export interface Repo {
  id: number; name: string; owner: string; description: string; priv: boolean;
  defaultBranch: string; seq: number; created_at: string;
  objects: Map<string, GitObj>;
  refs: Map<string, string>; // refname -> oid (insertion ordered)
  labels: Map<string, Label>;
  issues: Map<number, Issue>;
  pulls: Map<number, Pull>;
  nextNumber: number; // shared issue/PR counter
  nextCommentId: number;
  packVersion: number; // bumped on any ref/object change; invalidates pack cache
  _json: string | null; _jsonLen: number;
}

export class Store {
  packCache = new Map<string, Buffer>();
  repos = new Map<string, Repo>(); // key owner/name
  repoOrder: Repo[] = [];
  nextRepoId = 5000;
  nextGlobalId = 10000; // REST object ids for issues/PRs/comments share one counter space per type below
  nextIssueId = 20000;
  nextCommentSeq = 1;

  key(owner: string, name: string): string { return `${owner}/${name}`; }
  getRepo(owner: string, name: string): Repo | undefined { return this.repos.get(this.key(owner, name)); }

  createRepo(name: string, description: string, priv: boolean): Repo | { error: string } {
    if (this.repos.has(this.key(VIEWER.login, name))) return { error: 'Repository already exists' };
    const id = this.nextRepoId++;
    const repo: Repo = {
      id, name, owner: VIEWER.login, description, priv, defaultBranch: 'main',
      seq: this.repoOrder.length, created_at: isoOf(EPOCH + id),
      objects: new Map(), refs: new Map(), labels: new Map(),
      issues: new Map(), pulls: new Map(), nextNumber: 1, nextCommentId: 1,
      packVersion: 0, _json: null, _jsonLen: 0,
    };
    this.repos.set(this.key(VIEWER.login, name), repo);
    this.repoOrder.push(repo);
    return repo;
  }

  deleteRepo(repo: Repo): void {
    this.repos.delete(this.key(repo.owner, repo.name));
    const i = this.repoOrder.indexOf(repo);
    if (i >= 0) this.repoOrder.splice(i, 1);
  }

  // ------------------------------------------------------------ git helpers
  refTip(repo: Repo, ref: string): string | undefined { return repo.refs.get(ref); }
  branchTip(repo: Repo, branch: string): string | undefined { return repo.refs.get(`refs/heads/${branch}`); }
  /** Resolve a ref-ish: branch name, refs/heads/x, refs/tags/x, or a raw oid. */
  resolveRef(repo: Repo, refish: string): string | undefined {
    if (isOid(refish) && repo.objects.has(refish)) return refish;
    if (repo.refs.has(refish)) return repo.refs.get(refish);
    if (repo.refs.has(`refs/heads/${refish}`)) return repo.refs.get(`refs/heads/${refish}`);
    if (repo.refs.has(`refs/tags/${refish}`)) return repo.refs.get(`refs/tags/${refish}`);
    return undefined;
  }
  /** Peel a ref to a commit oid (tag -> target commit). */
  peelToCommit(repo: Repo, oid: string): string | undefined {
    const o = repo.objects.get(oid);
    if (!o) return undefined;
    if (o.type === 'commit') return oid;
    if (o.type === 'tag') return this.peelToCommit(repo, parseTag(o.data).object);
    return undefined;
  }
  /** Commit history from a tip, newest first (DFS pre-order, parents in order). */
  history(repo: Repo, tip: string, limit = 1000): string[] {
    const out: string[] = [];
    const seen = new Set<string>();
    const stack = [tip];
    while (stack.length && out.length < limit) {
      const cur = stack.pop()!;
      if (seen.has(cur)) continue;
      seen.add(cur);
      const o = repo.objects.get(cur);
      if (!o || o.type !== 'commit') continue;
      out.push(cur);
      const parents = parseCommit(o.data).parents;
      for (let i = parents.length - 1; i >= 0; i--) stack.push(parents[i]);
    }
    return out;
  }
  /** Tree oid at a commit. */
  treeOf(repo: Repo, commitOid: string): string | undefined {
    const o = repo.objects.get(commitOid);
    return o?.type === 'commit' ? parseCommit(o.data).tree : undefined;
  }
  /** Read a file's blob from a tree. */
  readPath(repo: Repo, treeOid: string, path: string): { oid: string; data: Buffer } | null {
    const flat = flattenTree(repo.objects, treeOid);
    const e = flat.get(path);
    if (!e) return null;
    const o = repo.objects.get(e.oid);
    if (!o || o.type !== 'blob') return null;
    return { oid: e.oid, data: o.data };
  }
  /** Build a new tree with one path replaced/added/deleted; returns new tree oid. */
  writePath(repo: Repo, baseTree: string | undefined, path: string, data: Buffer | null): string {
    // Build nested tree structure from the flat map, then rebuild bottom-up.
    const flat = baseTree ? flattenTree(repo.objects, baseTree) : new Map<string, { oid: string; mode: string }>();
    if (data === null) flat.delete(path);
    else {
      const blobOid = putObj(repo.objects, 'blob', data);
      flat.set(path, { oid: blobOid, mode: '100644' });
    }
    interface Node { files: Map<string, { oid: string; mode: string }>; dirs: Map<string, Node> }
    const mk = (): Node => ({ files: new Map(), dirs: new Map() });
    const root = mk();
    for (const [p, e] of flat) {
      const segs = p.split('/');
      let node = root;
      for (let i = 0; i < segs.length - 1; i++) {
        if (!node.dirs.has(segs[i])) node.dirs.set(segs[i], mk());
        node = node.dirs.get(segs[i])!;
      }
      node.files.set(segs[segs.length - 1], e);
    }
    const build = (node: Node): string => {
      const entries: Array<{ mode: string; name: string; oid: string }> = [];
      for (const [name, e] of node.files) entries.push({ mode: e.mode, name, oid: e.oid });
      for (const [name, child] of node.dirs) entries.push({ mode: '40000', name, oid: build(child) });
      return putObj(repo.objects, 'tree', buildTree(entries));
    };
    return build(root);
  }
  /** Create a commit object on top of `parent` (or root) with the given tree. */
  makeCommit(repo: Repo, tree: string, parents: string[], message: string): string {
    const ts = nextTs();
    const who = `${IDENTITY} ${ts} +0000`;
    const msg = message.endsWith('\n') ? message : `${message}\n`;
    const data = buildCommit(tree, parents, who, who, msg);
    return putObj(repo.objects, 'commit', data);
  }
  /** Commit one file change on a branch; creates the branch from the default tip when missing. */
  commitFile(repo: Repo, branch: string, path: string, data: Buffer, message: string): { commitSha: string; contentSha: string } {
    let tip = this.branchTip(repo, branch);
    if (tip === undefined) {
      tip = this.branchTip(repo, repo.defaultBranch);
      // branch from default tip (or unborn)
      if (tip === undefined) {
        const tree = this.writePath(repo, undefined, path, data);
        const commit = this.makeCommit(repo, tree, [], message);
        repo.refs.set(`refs/heads/${branch}`, commit);
        repo.packVersion++;
        return { commitSha: commit, contentSha: hashObject('blob', data) };
      }
      repo.refs.set(`refs/heads/${branch}`, tip);
    }
    const tree = this.treeOf(repo, tip)!;
    const newTree = this.writePath(repo, tree, path, data);
    const commit = this.makeCommit(repo, newTree, [tip], message);
    repo.refs.set(`refs/heads/${branch}`, commit);
    repo.packVersion++;
    return { commitSha: commit, contentSha: hashObject('blob', data) };
  }
  deleteFile(repo: Repo, branch: string, path: string, message: string): string | null {
    const tip = this.branchTip(repo, branch);
    if (tip === undefined) return null;
    const tree = this.treeOf(repo, tip)!;
    if (!flattenTree(repo.objects, tree).has(path)) return null;
    const newTree = this.writePath(repo, tree, path, null);
    const commit = this.makeCommit(repo, newTree, [tip], message);
    repo.refs.set(`refs/heads/${branch}`, commit);
    repo.packVersion++;
    return commit;
  }

  // ------------------------------------------------------------ issues
  createIssue(repo: Repo, title: string, body: string, labelNames: string[]): Issue | { error: string } {
    const labels: Label[] = [];
    for (const n of labelNames) {
      const l = repo.labels.get(n);
      if (!l) return { error: `Label '${n}' does not exist` };
      labels.push(l);
    }
    const number = repo.nextNumber++;
    const id = this.nextIssueId++;
    const created = isoOf(EPOCH + 100000 + id);
    const issue: Issue = {
      id, number, title, body: body ?? '', state: 'open', labels, comments: [],
      seq: number, created_at: created, updated_at: created, _json: null, _jsonLen: 0,
    };
    repo.issues.set(number, issue);
    return issue;
  }
  addComment(repo: Repo, issue: Issue, body: string): Comment {
    const id = repo.nextCommentId++;
    const c: Comment = { id, body, user: VIEWER.login, created_at: isoOf(EPOCH + 200000 + id), _gid: gid('Comment', `${repo.owner}/${repo.name}#${issue.number}/c${id}`) };
    issue.comments.push(c);
    return c;
  }

  // ------------------------------------------------------------ pulls
  createPull(repo: Repo, title: string, body: string, head: string, base: string): Pull | { error: string } {
    const headTip = this.branchTip(repo, head);
    const baseTip = this.branchTip(repo, base);
    if (headTip === undefined) return { error: `Head branch '${head}' does not exist` };
    if (baseTip === undefined) return { error: `Base branch '${base}' does not exist` };
    for (const p of repo.pulls.values()) {
      if (p.state === 'open' && p.headRef === head && p.baseRef === base) return { error: 'A pull request already exists for this head branch' };
    }
    const number = repo.nextNumber++;
    const id = this.nextIssueId++;
    const pr: Pull = {
      id, number, title, body: body ?? '', state: 'open', headRef: head, baseRef: base,
      headSha: headTip, baseSha: baseTip, merged: false, mergeCommitSha: null, mergedAt: null,
      seq: number, created_at: isoOf(EPOCH + 100000 + id),
    };
    repo.pulls.set(number, pr);
    return pr;
  }
  /** File diff between two trees: path -> {status, oldData?, newData?}. */
  diffTrees(repo: Repo, oldTree: string | undefined, newTree: string | undefined): Map<string, { status: 'added' | 'modified' | 'removed'; oldData: Buffer | null; newData: Buffer | null }> {
    const oldF = oldTree ? flattenTree(repo.objects, oldTree) : new Map();
    const newF = newTree ? flattenTree(repo.objects, newTree) : new Map();
    const out = new Map<string, { status: 'added' | 'modified' | 'removed'; oldData: Buffer | null; newData: Buffer | null }>();
    const all = new Set([...oldF.keys(), ...newF.keys()]);
    for (const p of all) {
      const o = oldF.get(p); const n = newF.get(p);
      if (o && !n) out.set(p, { status: 'removed', oldData: repo.objects.get(o.oid)?.data ?? null, newData: null });
      else if (!o && n) out.set(p, { status: 'added', oldData: null, newData: repo.objects.get(n.oid)?.data ?? null });
      else if (o && n && o.oid !== n.oid) out.set(p, { status: 'modified', oldData: repo.objects.get(o.oid)?.data ?? null, newData: repo.objects.get(n.oid)?.data ?? null });
    }
    return out;
  }
  pullFiles(repo: Repo, pr: Pull): Array<{ filename: string; status: string; additions: number; deletions: number; changes: number }> {
    // Diff the CURRENT branch tips (the fixture pushes after PR creation).
    const baseTip = this.branchTip(repo, pr.baseRef) ?? pr.baseSha;
    const headTip = this.branchTip(repo, pr.headRef) ?? pr.headSha;
    const baseTree = this.treeOf(repo, this.peelToCommit(repo, baseTip) ?? baseTip);
    const headTree = this.treeOf(repo, this.peelToCommit(repo, headTip) ?? headTip);
    const diff = this.diffTrees(repo, baseTree, headTree);
    const out: Array<{ filename: string; status: string; additions: number; deletions: number; changes: number }> = [];
    for (const p of [...diff.keys()].sort()) {
      const d = diff.get(p)!;
      const [a, del] = lineDiff(d.oldData, d.newData);
      out.push({ filename: p, status: d.status, additions: a, deletions: del, changes: a + del });
    }
    return out;
  }
  /** Is this PR mergeable right now (3-way, file level)? */
  pullConflict(repo: Repo, pr: Pull): boolean {
    const baseTip = this.branchTip(repo, pr.baseRef);
    const headTip = this.branchTip(repo, pr.headRef);
    if (baseTip === undefined || headTip === undefined) return true;
    const baseCommit = this.peelToCommit(repo, baseTip);
    const headCommit = this.peelToCommit(repo, headTip);
    if (!baseCommit || !headCommit) return true;
    const mb = mergeBase(repo.objects, baseCommit, headCommit);
    if (mb === null) return true;
    const mbTree = this.treeOf(repo, mb);
    const baseTree = this.treeOf(repo, baseCommit);
    const headTree = this.treeOf(repo, headCommit);
    const dBase = this.diffTrees(repo, mbTree, baseTree);
    const dHead = this.diffTrees(repo, mbTree, headTree);
    for (const [p, dh] of dHead) {
      const db = dBase.get(p);
      if (!db) continue;
      const headBlob = dh.newData; const baseBlob = db.newData;
      const h = headBlob ? hashObject('blob', headBlob) : null;
      const b = baseBlob ? hashObject('blob', baseBlob) : null;
      if (h !== b) return true;
    }
    return false;
  }
  mergePull(repo: Repo, pr: Pull, method: string): { sha: string; message: string } | { error: string; status: number } {
    if (pr.state !== 'open') return { error: 'Pull request is already closed', status: 405 };
    if (this.pullConflict(repo, pr)) return { error: 'Merge conflict', status: 405 };
    const baseTip = this.branchTip(repo, pr.baseRef)!;
    const headTip = this.branchTip(repo, pr.headRef)!;
    const headCommit = this.peelToCommit(repo, headTip)!;
    const headTree = this.treeOf(repo, headCommit)!;
    const parents = method === 'merge' ? [baseTip, headCommit] : [baseTip];
    const message = `Merge pull request #${pr.number} from ${pr.headRef}\n\n${pr.title}`;
    const commit = this.makeCommit(repo, headTree, parents, message);
    repo.refs.set(`refs/heads/${pr.baseRef}`, commit);
    repo.packVersion++;
    pr.state = 'closed';
    pr.merged = true;
    pr.mergeCommitSha = commit;
    pr.mergedAt = isoOf(nextTs());
    return { sha: commit, message: 'Pull request successfully merged' };
  }
}

// ---------------------------------------------------------------- ids / misc
export function gid(kind: string, key: string): string {
  return Buffer.from(`${kind}:${key}`, 'utf8').toString('base64');
}
export function decodeGid(id: string): { kind: string; key: string } | null {
  try {
    const s = Buffer.from(id, 'base64').toString('utf8');
    const i = s.indexOf(':');
    if (i < 0) return null;
    return { kind: s.slice(0, i), key: s.slice(i + 1) };
  } catch { return null; }
}
/** Naive line diff counts via LCS on lines (files here are small). */
export function lineDiff(oldData: Buffer | null, newData: Buffer | null): [number, number] {
  const oldLines = oldData ? oldData.toString('utf8').split('\n') : [];
  const newLines = newData ? newData.toString('utf8').split('\n') : [];
  if (oldLines.length && oldLines[oldLines.length - 1] === '') oldLines.pop();
  if (newLines.length && newLines[newLines.length - 1] === '') newLines.pop();
  const n = oldLines.length; const m = newLines.length;
  if (n === 0) return [m, 0];
  if (m === 0) return [0, n];
  // LCS with rolling rows; cap work for big files.
  if (n * m > 400000) {
    let common = 0;
    const set = new Set(oldLines);
    for (const l of newLines) if (set.has(l)) common++;
    return [m - common, n - common];
  }
  let prev = new Array<number>(m + 1).fill(0);
  let cur = new Array<number>(m + 1).fill(0);
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      cur[j] = oldLines[i - 1] === newLines[j - 1] ? prev[j - 1] + 1 : Math.max(prev[j], cur[j - 1]);
    }
    [prev, cur] = [cur, prev];
  }
  const lcs = prev[m];
  return [m - lcs, n - lcs];
}
export type { GitObj, ObjType };
export { ZERO_OID };
