// Minimal GraphQL executor for the exact documents gh / github-mcp-server send.
// Parses a query/mutation document (fields, aliases, arguments, variables,
// inline fragments, __type introspection) and projects the store's data.
// Unknown fields produce a 200 with an errors array naming the field.
import { decodeGid, gid, VIEWER, type Issue, type Pull, type Repo, type Store } from './store.ts';

// ---------------------------------------------------------------- parser
interface ArgValue { kind: 'var' | 'string' | 'int' | 'float' | 'bool' | 'null' | 'enum' | 'list' | 'object'; value: unknown }
interface Field {
  alias: string | null; name: string;
  args: Record<string, ArgValue>;
  selection: Field[] | null;
}
interface Operation { kind: 'query' | 'mutation'; name: string | null; fields: Field[] }

class Parser {
  private pos = 0;
  private readonly src: string;
  constructor(src: string) { this.src = src; }
  private skipWs(): void {
    for (;;) {
      while (this.pos < this.src.length && /[\s,]/.test(this.src[this.pos])) this.pos++;
      if (this.src[this.pos] === '#') { while (this.pos < this.src.length && this.src[this.pos] !== '\n') this.pos++; continue; }
      break;
    }
  }
  private peek(): string { this.skipWs(); return this.src[this.pos] ?? ''; }
  private ident(): string {
    this.skipWs();
    const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(this.src.slice(this.pos));
    if (!m) throw new Error(`expected identifier at ${this.pos}`);
    this.pos += m[0].length;
    return m[0];
  }
  private expect(ch: string): void {
    this.skipWs();
    if (this.src[this.pos] !== ch) throw new Error(`expected ${ch} at ${this.pos}, got ${this.src[this.pos] ?? 'EOF'}`);
    this.pos++;
  }
  private parseValue(): ArgValue {
    this.skipWs();
    const c = this.src[this.pos];
    if (c === '$') { this.pos++; return { kind: 'var', value: this.ident() }; }
    if (c === '"') {
      this.pos++;
      let out = '';
      while (this.pos < this.src.length && this.src[this.pos] !== '"') {
        if (this.src[this.pos] === '\\') { this.pos++; const e = this.src[this.pos]; out += e === 'n' ? '\n' : e === 't' ? '\t' : e === 'r' ? '\r' : e; this.pos++; }
        else out += this.src[this.pos++];
      }
      this.pos++;
      return { kind: 'string', value: out };
    }
    if (c === '[') {
      this.pos++;
      const items: ArgValue[] = [];
      while (this.peek() !== ']') items.push(this.parseValue());
      this.expect(']');
      return { kind: 'list', value: items };
    }
    if (c === '{') {
      this.pos++;
      const obj: Record<string, ArgValue> = {};
      while (this.peek() !== '}') {
        const k = this.ident();
        this.expect(':');
        obj[k] = this.parseValue();
      }
      this.expect('}');
      return { kind: 'object', value: obj };
    }
    if (/[0-9-]/.test(c)) {
      const m = /^-?\d+(\.\d+)?/.exec(this.src.slice(this.pos));
      this.pos += m![0].length;
      return m![0].includes('.') ? { kind: 'float', value: Number(m![0]) } : { kind: 'int', value: Number(m![0]) };
    }
    const id = this.ident();
    if (id === 'true') return { kind: 'bool', value: true };
    if (id === 'false') return { kind: 'bool', value: false };
    if (id === 'null') return { kind: 'null', value: null };
    return { kind: 'enum', value: id };
  }
  private parseArgs(): Record<string, ArgValue> {
    const args: Record<string, ArgValue> = {};
    this.skipWs();
    if (this.src[this.pos] !== '(') return args;
    this.pos++;
    while (this.peek() !== ')') {
      const k = this.ident();
      this.expect(':');
      args[k] = this.parseValue();
    }
    this.expect(')');
    return args;
  }
  private parseSelection(): Field[] {
    this.expect('{');
    const fields: Field[] = [];
    while (this.peek() !== '}') {
      if (this.src.slice(this.pos, this.pos + 3) === '...') {
        this.pos += 3;
        this.skipWs();
        if (this.src.slice(this.pos, this.pos + 2) === 'on') {
          this.pos += 2;
          this.ident(); // type condition; fields apply to the parent type
        } else {
          this.ident(); // fragment name (not used by the suite's documents)
        }
        const sub = this.parseSelection();
        fields.push(...sub);
        continue;
      }
      let name = this.ident();
      let alias: string | null = null;
      this.skipWs();
      if (this.src[this.pos] === ':') { alias = name; this.pos++; name = this.ident(); }
      const args = this.parseArgs();
      let selection: Field[] | null = null;
      this.skipWs();
      if (this.src[this.pos] === '{') selection = this.parseSelection();
      fields.push({ alias, name, args, selection });
    }
    this.expect('}');
    return fields;
  }
  parseOperation(): Operation {
    this.skipWs();
    let kind: 'query' | 'mutation' = 'query';
    if (this.src.slice(this.pos, this.pos + 5) === 'query' && !/[A-Za-z0-9_]/.test(this.src[this.pos + 5] ?? '')) { this.pos += 5; kind = 'query'; }
    else if (this.src.slice(this.pos, this.pos + 8) === 'mutation' && !/[A-Za-z0-9_]/.test(this.src[this.pos + 8] ?? '')) { this.pos += 8; kind = 'mutation'; }
    this.skipWs();
    let name: string | null = null;
    if (/[A-Za-z_]/.test(this.src[this.pos] ?? '')) name = this.ident();
    // skip variable definitions (...) if present
    this.skipWs();
    if (this.src[this.pos] === '(') {
      let depth = 0;
      while (this.pos < this.src.length) {
        const c = this.src[this.pos++];
        if (c === '(') depth++;
        else if (c === ')') { depth--; if (depth === 0) break; }
      }
    }
    const fields = this.parseSelection();
    return { kind, name, fields };
  }
}

function resolveArg(v: ArgValue, vars: Record<string, unknown>): unknown {
  switch (v.kind) {
    case 'var': return vars[v.value as string];
    case 'list': return (v.value as ArgValue[]).map((x) => resolveArg(x, vars));
    case 'object': {
      const o: Record<string, unknown> = {};
      for (const [k, x] of Object.entries(v.value as Record<string, ArgValue>)) o[k] = resolveArg(x, vars);
      return o;
    }
    default: return v.value;
  }
}
function argOf(f: Field, name: string, vars: Record<string, unknown>): unknown {
  const a = f.args[name];
  return a === undefined ? undefined : resolveArg(a, vars);
}

// ---------------------------------------------------------------- schema introspection
const TYPE_FIELDS: Record<string, string[]> = {
  Issue: ['id', 'number', 'title', 'state', 'body', 'createdAt', 'updatedAt', 'url', 'author', 'comments', 'labels', 'closed', 'repository'],
  PullRequest: ['id', 'number', 'title', 'state', 'merged', 'headRefName', 'baseRefName', 'mergeable', 'files', 'body', 'createdAt', 'updatedAt', 'url', 'author', 'headRef', 'baseRef', 'mergeCommit', 'repository'],
  Repository: ['id', 'databaseId', 'name', 'nameWithOwner', 'owner', 'defaultBranchRef', 'viewerPermission', 'hasIssuesEnabled', 'issue', 'issues', 'pullRequests', 'pullRequest', 'description', 'url', 'createdAt', 'updatedAt', 'isPrivate', 'fork'],
  User: ['id', 'login', 'name', 'email', 'url'],
  SearchType: [],
};
const SEARCH_ENUM = ['ISSUE', 'PULL_REQUEST', 'REPOSITORY', 'USER', 'DISCUSSION', 'COMMIT', 'MILESTONE', 'LABEL'];

// ---------------------------------------------------------------- executor
interface Ctx { store: Store; vars: Record<string, unknown>; errors: Array<{ message: string }> }

function issueGid(repo: Repo, issue: Issue): string { return gid('Issue', `${repo.owner}/${repo.name}#${issue.number}`); }
function repoGid(repo: Repo): string { return gid('Repository', `${repo.owner}/${repo.name}`); }
function pullGid(repo: Repo, pr: Pull): string { return gid('PullRequest', `${repo.owner}/${repo.name}#${pr.number}`); }

function projectIssue(ctx: Ctx, repo: Repo, issue: Issue, sel: Field[] | null): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of sel ?? []) {
    const key = f.alias ?? f.name;
    switch (f.name) {
      case 'id': out[key] = issueGid(repo, issue); break;
      case 'number': out[key] = issue.number; break;
      case 'title': out[key] = issue.title; break;
      case 'body': out[key] = issue.body; break;
      case 'state': out[key] = issue.state === 'open' ? 'OPEN' : 'CLOSED'; break;
      case 'createdAt': out[key] = issue.created_at; break;
      case 'updatedAt': out[key] = issue.updated_at; break;
      case 'url': out[key] = `https://example.invalid/${repo.owner}/${repo.name}/issues/${issue.number}`; break;
      case 'author': out[key] = projectUser(ctx, f.selection); break;
      case 'comments': out[key] = { totalCount: issue.comments.length, nodes: issue.comments.map((c) => ({ id: c._gid, body: c.body })) }; break;
      case 'labels': out[key] = { nodes: issue.labels.map((l) => ({ name: l.name })) }; break;
      case 'closed': out[key] = issue.state === 'closed'; break;
      case 'repository': out[key] = projectRepo(ctx, repo, f.selection); break;
      case '__typename': out[key] = 'Issue'; break;
      default: ctx.errors.push({ message: `Cannot query field "${f.name}" on type "Issue".` });
    }
  }
  return out;
}
function projectUser(ctx: Ctx, sel: Field[] | null): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of sel ?? []) {
    const key = f.alias ?? f.name;
    if (f.name === 'login') out[key] = VIEWER.login;
    else if (f.name === 'name') out[key] = VIEWER.name;
    else if (f.name === 'id') out[key] = gid('User', VIEWER.login);
    else if (f.name === 'email') out[key] = VIEWER.email;
    else if (f.name === 'url') out[key] = `https://example.invalid/${VIEWER.login}`;
    else if (f.name === '__typename') out[key] = 'User';
    else { ctx.errors.push({ message: `Cannot query field "${f.name}" on type "User".` }); out[key] = null; }
  }
  return out;
}
function projectRepo(ctx: Ctx, repo: Repo, sel: Field[] | null): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of sel ?? []) {
    const key = f.alias ?? f.name;
    switch (f.name) {
      case 'id': out[key] = repoGid(repo); break;
      case 'databaseId': out[key] = repo.id; break;
      case 'name': out[key] = repo.name; break;
      case 'nameWithOwner': out[key] = `${repo.owner}/${repo.name}`; break;
      case 'owner': out[key] = projectUser(ctx, f.selection); break;
      case 'description': out[key] = repo.description; break;
      case 'isPrivate': out[key] = repo.priv; break;
      case 'url': out[key] = `https://example.invalid/${repo.owner}/${repo.name}`; break;
      case 'createdAt': out[key] = repo.created_at; break;
      case 'viewerPermission': out[key] = 'ADMIN'; break;
      case 'hasIssuesEnabled': out[key] = true; break;
      case 'defaultBranchRef': {
        const tip = repo.refs.get(`refs/heads/${repo.defaultBranch}`);
        out[key] = tip ? { name: repo.defaultBranch } : null;
        break;
      }
      case 'issue': {
        const n = argOf(f, 'number', ctx.vars) as number;
        const issue = repo.issues.get(n);
        out[key] = issue ? projectIssue(ctx, repo, issue, f.selection) : null;
        break;
      }
      case 'issues': out[key] = projectIssues(ctx, repo, f); break;
      case 'pullRequests': out[key] = projectPulls(ctx, repo, f); break;
      case 'pullRequest': {
        const n = argOf(f, 'number', ctx.vars) as number;
        const pr = repo.pulls.get(n);
        out[key] = pr ? projectPull(ctx, repo, pr, f.selection) : null;
        break;
      }
      case '__typename': out[key] = 'Repository'; break;
      default: ctx.errors.push({ message: `Cannot query field "${f.name}" on type "Repository".` });
    }
  }
  return out;
}
function projectIssues(ctx: Ctx, repo: Repo, f: Field): Record<string, unknown> {
  let list = [...repo.issues.values()];
  const states = argOf(f, 'states', ctx.vars) as string[] | undefined;
  if (states) {
    const want = new Set(states.map((s) => String(s).toLowerCase()));
    list = list.filter((i) => want.has(i.state));
  }
  const orderBy = argOf(f, 'orderBy', ctx.vars) as { field?: string; direction?: string } | undefined;
  const dir = orderBy?.direction === 'ASC' ? 1 : -1; // default DESC (creation desc)
  list.sort((a, b) => (a.number - b.number) * dir);
  const first = argOf(f, 'first', ctx.vars) as number | undefined;
  const after = argOf(f, 'after', ctx.vars) as string | undefined;
  let start = 0;
  if (after) {
    const dec = decodeGid(after);
    const n = dec ? Number(dec.key.split(':')[1]) : NaN;
    if (Number.isFinite(n)) {
      const idx = list.findIndex((i) => i.number === n);
      if (idx >= 0) start = idx + 1;
    }
  }
  const page = first !== undefined ? list.slice(start, start + first) : list.slice(start);
  const endCursor = page.length ? gid('cursor', `issue:${page[page.length - 1].number}`) : null;
  const nodes: unknown[] = [];
  const nodeSel = f.selection?.find((s) => s.name === 'nodes')?.selection ?? null;
  const edgesSel = f.selection?.find((s) => s.name === 'edges');
  for (const issue of page) nodes.push(projectIssue(ctx, repo, issue, nodeSel));
  const out: Record<string, unknown> = { nodes, totalCount: list.length };
  if (edgesSel) out.edges = page.map((issue) => ({ node: projectIssue(ctx, repo, issue, edgesSel.selection?.find((s) => s.name === 'node')?.selection ?? null), cursor: gid('cursor', `issue:${issue.number}`) }));
  const pageInfoSel = f.selection?.find((s) => s.name === 'pageInfo');
  if (pageInfoSel) {
    const pi: Record<string, unknown> = {};
    for (const pf of pageInfoSel.selection ?? []) {
      const k = pf.alias ?? pf.name;
      if (pf.name === 'hasNextPage') pi[k] = start + page.length < list.length;
      else if (pf.name === 'hasPreviousPage') pi[k] = start > 0;
      else if (pf.name === 'endCursor') pi[k] = endCursor;
      else if (pf.name === 'startCursor') pi[k] = page.length ? gid('cursor', `issue:${page[0].number}`) : null;
    }
    out.pageInfo = pi;
  }
  return out;
}
function projectPulls(ctx: Ctx, repo: Repo, f: Field): Record<string, unknown> {
  let list = [...repo.pulls.values()];
  const states = argOf(f, 'states', ctx.vars) as string[] | undefined;
  if (states) {
    const want = new Set(states.map((s) => String(s).toUpperCase()));
    list = list.filter((p) => want.has(p.merged ? 'MERGED' : p.state.toUpperCase()));
  }
  list.sort((a, b) => b.number - a.number);
  const first = argOf(f, 'first', ctx.vars) as number | undefined;
  const page = first !== undefined ? list.slice(0, first) : list;
  const nodeSel = f.selection?.find((s) => s.name === 'nodes')?.selection ?? null;
  return { nodes: page.map((p) => projectPull(ctx, repo, p, nodeSel)), totalCount: list.length };
}
function projectPull(ctx: Ctx, repo: Repo, pr: Pull, sel: Field[] | null): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of sel ?? []) {
    const key = f.alias ?? f.name;
    switch (f.name) {
      case 'id': out[key] = pullGid(repo, pr); break;
      case 'number': out[key] = pr.number; break;
      case 'title': out[key] = pr.title; break;
      case 'body': out[key] = pr.body; break;
      case 'state': out[key] = pr.merged ? 'MERGED' : pr.state.toUpperCase(); break;
      case 'merged': out[key] = pr.merged; break;
      case 'headRefName': out[key] = pr.headRef; break;
      case 'baseRefName': out[key] = pr.baseRef; break;
      case 'mergeable': out[key] = pr.state === 'open' ? (ctx.store.pullConflict(repo, pr) ? 'CONFLICTING' : 'MERGEABLE') : 'UNKNOWN'; break;
      case 'createdAt': out[key] = pr.created_at; break;
      case 'url': out[key] = `https://example.invalid/${repo.owner}/${repo.name}/pull/${pr.number}`; break;
      case 'author': out[key] = projectUser(ctx, f.selection); break;
      case 'headRef': out[key] = { name: pr.headRef }; break;
      case 'baseRef': out[key] = { name: pr.baseRef }; break;
      case 'files': {
        const files = ctx.store.pullFiles(repo, pr);
        const first = argOf(f, 'first', ctx.vars) as number | undefined;
        const page = first !== undefined ? files.slice(0, first) : files;
        const nodeSel = f.selection?.find((s) => s.name === 'nodes')?.selection ?? null;
        const nodes = page.map((fl) => {
          const o: Record<string, unknown> = {};
          for (const nf of nodeSel ?? []) {
            const k = nf.alias ?? nf.name;
            if (nf.name === 'path') o[k] = fl.filename;
            else if (nf.name === 'additions') o[k] = fl.additions;
            else if (nf.name === 'deletions') o[k] = fl.deletions;
            else if (nf.name === 'changeType') o[k] = fl.status.toUpperCase();
          }
          return o;
        });
        out[key] = { nodes, totalCount: files.length };
        break;
      }
      case 'mergeCommit': out[key] = pr.mergeCommitSha ? { oid: pr.mergeCommitSha } : null; break;
      case 'repository': out[key] = projectRepo(ctx, repo, f.selection); break;
      case '__typename': out[key] = 'PullRequest'; break;
      default: ctx.errors.push({ message: `Cannot query field "${f.name}" on type "PullRequest".` });
    }
  }
  return out;
}

function projectType(ctx: Ctx, name: string, sel: Field[] | null): Record<string, unknown> | null {
  if (!(name in TYPE_FIELDS) && name !== 'SearchType' && name !== 'Query' && name !== 'Mutation') return null;
  const out: Record<string, unknown> = { name };
  for (const f of sel ?? []) {
    const key = f.alias ?? f.name;
    if (f.name === 'fields') {
      const fields = TYPE_FIELDS[name] ?? [];
      out[key] = fields.map((n) => ({ name: n }));
    } else if (f.name === 'enumValues') {
      out[key] = name === 'SearchType' ? SEARCH_ENUM.map((n) => ({ name: n })) : null;
    } else if (f.name === 'name') out[key] = name;
    else if (f.name === 'kind') out[key] = name === 'SearchType' ? 'ENUM' : 'OBJECT';
    else out[key] = null;
  }
  return out;
}

function findRepoByGid(store: Store, id: string): Repo | undefined {
  const dec = decodeGid(id);
  if (!dec || dec.kind !== 'Repository') return undefined;
  const [owner, name] = dec.key.split('/');
  return store.getRepo(owner, name);
}

export function executeGraphQL(store: Store, query: string, variables: Record<string, unknown>): { data: unknown; errors?: Array<{ message: string }> } {
  const ctx: Ctx = { store, vars: variables ?? {}, errors: [] };
  let op: Operation;
  try {
    op = new Parser(query).parseOperation();
  } catch (e) {
    return { data: null, errors: [{ message: `GraphQL parse error: ${e instanceof Error ? e.message : String(e)}` }] };
  }
  const data: Record<string, unknown> = {};
  if (op.kind === 'mutation') {
    for (const f of op.fields) {
      const key = f.alias ?? f.name;
      const input = (argOf(f, 'input', ctx.vars) ?? {}) as Record<string, unknown>;
      if (f.name === 'createIssue') {
        const repo = typeof input.repositoryId === 'string' ? findRepoByGid(store, input.repositoryId) : undefined;
        if (!repo) { ctx.errors.push({ message: 'Could not resolve to a Repository with the id provided.' }); data[key] = null; continue; }
        const created = store.createIssue(repo, String(input.title ?? ''), String(input.body ?? ''), []);
        if ('error' in created) { ctx.errors.push({ message: created.error }); data[key] = null; continue; }
        const sel = f.selection?.find((s) => s.name === 'issue')?.selection ?? null;
        data[key] = { issue: projectIssue(ctx, repo, created, sel) };
      } else if (f.name === 'closeIssue') {
        const dec = typeof input.issueId === 'string' ? decodeGid(input.issueId) : null;
        let issue: Issue | undefined; let repo: Repo | undefined;
        if (dec?.kind === 'Issue') {
          const m = /^(.+)\/(.+)#(\d+)$/.exec(dec.key);
          if (m) { repo = store.getRepo(m[1], m[2]); issue = repo?.issues.get(Number(m[3])); }
        }
        if (!issue || !repo) { ctx.errors.push({ message: 'Could not resolve to an Issue with the id provided.' }); data[key] = null; continue; }
        issue.state = 'closed'; issue._json = null;
        const sel = f.selection?.find((s) => s.name === 'issue')?.selection ?? null;
        data[key] = { issue: projectIssue(ctx, repo, issue, sel) };
      } else if (f.name === 'createPullRequest') {
        const repo = typeof input.repositoryId === 'string' ? findRepoByGid(store, input.repositoryId) : undefined;
        if (!repo) { ctx.errors.push({ message: 'Could not resolve to a Repository with the id provided.' }); data[key] = null; continue; }
        const created = store.createPull(repo, String(input.title ?? ''), String(input.body ?? ''), String(input.headRefName ?? ''), String(input.baseRefName ?? ''));
        if ('error' in created) { ctx.errors.push({ message: created.error }); data[key] = null; continue; }
        const sel = f.selection?.find((s) => s.name === 'pullRequest')?.selection ?? null;
        data[key] = { pullRequest: projectPull(ctx, repo, created, sel) };
      } else if (f.name === 'mergePullRequest') {
        const dec = typeof input.pullRequestId === 'string' ? decodeGid(input.pullRequestId) : null;
        let pr: Pull | undefined; let repo: Repo | undefined;
        if (dec?.kind === 'PullRequest') {
          const m = /^(.+)\/(.+)#(\d+)$/.exec(dec.key);
          if (m) { repo = store.getRepo(m[1], m[2]); pr = repo?.pulls.get(Number(m[3])); }
        }
        if (!pr || !repo) { ctx.errors.push({ message: 'Could not resolve to a PullRequest with the id provided.' }); data[key] = null; continue; }
        const method = String(input.mergeMethod ?? 'SQUASH');
        const merged = store.mergePull(repo, pr, method.toLowerCase() === 'merge' ? 'merge' : 'squash');
        if ('error' in merged) { ctx.errors.push({ message: merged.error }); data[key] = null; continue; }
        const sel = f.selection?.find((s) => s.name === 'pullRequest')?.selection ?? null;
        data[key] = { pullRequest: projectPull(ctx, repo, pr, sel) };
      } else if (f.name === 'addComment') {
        const dec = typeof input.subjectId === 'string' ? decodeGid(input.subjectId) : null;
        let issue: Issue | undefined; let repo: Repo | undefined;
        if (dec?.kind === 'Issue') {
          const m = /^(.+)\/(.+)#(\d+)$/.exec(dec.key);
          if (m) { repo = store.getRepo(m[1], m[2]); issue = repo?.issues.get(Number(m[3])); }
        }
        if (!issue || !repo) { ctx.errors.push({ message: 'Could not resolve to a subject with the id provided.' }); data[key] = null; continue; }
        const comment = store.addComment(repo, issue, String(input.body ?? ''));
        const nodeSel = f.selection?.find((s) => s.name === 'commentEdge')?.selection?.find((s) => s.name === 'node')?.selection ?? null;
        const node: Record<string, unknown> = {};
        for (const nf of nodeSel ?? []) {
          const k = nf.alias ?? nf.name;
          if (nf.name === 'id') node[k] = comment._gid;
          else if (nf.name === 'body') node[k] = comment.body;
        }
        data[key] = { commentEdge: { node } };
      } else {
        ctx.errors.push({ message: `Cannot query field "${f.name}" on type "Mutation".` });
        data[key] = null;
      }
    }
  } else {
    for (const f of op.fields) {
      const key = f.alias ?? f.name;
      if (f.name === 'viewer') {
        data[key] = projectUser(ctx, f.selection);
      } else if (f.name === 'repository') {
        const owner = String(argOf(f, 'owner', ctx.vars) ?? '');
        const name = String(argOf(f, 'name', ctx.vars) ?? '');
        const repo = owner === VIEWER.login ? store.getRepo(owner, name) : undefined;
        data[key] = repo ? projectRepo(ctx, repo, f.selection) : null;
      } else if (f.name === 'node') {
        const id = String(argOf(f, 'id', ctx.vars) ?? '');
        const dec = decodeGid(id);
        if (!dec) { data[key] = null; continue; }
        if (dec.kind === 'Repository') {
          const [owner, name] = dec.key.split('/');
          const repo = store.getRepo(owner, name);
          if (!repo) { data[key] = null; continue; }
          const node: Record<string, unknown> = { id };
          for (const nf of f.selection ?? []) {
            const k = nf.alias ?? nf.name;
            if (nf.name === 'id') node[k] = id;
            else if (nf.name === 'name') node[k] = repo.name;
            else if (nf.name === 'nameWithOwner') node[k] = `${repo.owner}/${repo.name}`;
            else if (nf.name === '__typename') node[k] = 'Repository';
          }
          data[key] = node;
        } else if (dec.kind === 'Issue') {
          const m = /^(.+)\/(.+)#(\d+)$/.exec(dec.key);
          const repo = m ? store.getRepo(m[1], m[2]) : undefined;
          const issue = repo && m ? repo.issues.get(Number(m[3])) : undefined;
          data[key] = issue && repo ? projectIssue(ctx, repo, issue, f.selection) : null;
        } else if (dec.kind === 'PullRequest') {
          const m = /^(.+)\/(.+)#(\d+)$/.exec(dec.key);
          const repo = m ? store.getRepo(m[1], m[2]) : undefined;
          const pr = repo && m ? repo.pulls.get(Number(m[3])) : undefined;
          data[key] = pr && repo ? projectPull(ctx, repo, pr, f.selection) : null;
        } else data[key] = null;
      } else if (f.name === '__type') {
        const name = String(argOf(f, 'name', ctx.vars) ?? '');
        data[key] = projectType(ctx, name, f.selection);
      } else if (f.name === '__schema') {
        data[key] = { types: Object.keys(TYPE_FIELDS).map((n) => ({ name: n })) };
      } else {
        ctx.errors.push({ message: `Cannot query field "${f.name}" on type "Query".` });
        data[key] = null;
      }
    }
  }
  if (ctx.errors.length) return { data, errors: ctx.errors };
  return { data };
}
