// Minimal GraphQL executor — port of graphql.ts (Node entry).
use crate::store::*;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

// ---------------------------------------------------------------- parser
#[derive(Clone, Debug)]
pub enum ArgValue { Var(String), Str(String), Int(i64), Float(f64), Bool(bool), Null, Enum(String), List(Vec<ArgValue>), Object(HashMap<String, ArgValue>) }
#[derive(Clone, Debug)]
pub struct Field { pub alias: Option<String>, pub name: String, pub args: HashMap<String, ArgValue>, pub selection: Option<Vec<Field>> }
#[derive(Clone, Debug)]
pub struct Operation { pub kind: String, pub fields: Vec<Field> }

pub struct Parser<'a> { src: &'a [u8], pos: usize }
impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self { Parser { src: src.as_bytes(), pos: 0 } }
    fn skip_ws(&mut self) {
        loop {
            while self.pos < self.src.len() && matches!(self.src[self.pos], b' ' | b'\t' | b'\n' | b'\r' | b',') { self.pos += 1; }
            if self.pos < self.src.len() && self.src[self.pos] == b'#' {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' { self.pos += 1; }
                continue;
            }
            break;
        }
    }
    fn peek(&mut self) -> u8 { self.skip_ws(); if self.pos < self.src.len() { self.src[self.pos] } else { 0 } }
    fn ident(&mut self) -> Result<String, String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.src.len() && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_') { self.pos += 1; }
        if start == self.pos { return Err(format!("expected identifier at {}", self.pos)); }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
    }
    fn expect(&mut self, ch: u8) -> Result<(), String> {
        self.skip_ws();
        if self.pos >= self.src.len() || self.src[self.pos] != ch { return Err(format!("expected {} at {}", ch as char, self.pos)); }
        self.pos += 1; Ok(())
    }
    fn parse_value(&mut self) -> Result<ArgValue, String> {
        self.skip_ws();
        let c = *self.src.get(self.pos).unwrap_or(&0);
        match c {
            b'$' => { self.pos += 1; Ok(ArgValue::Var(self.ident()?)) }
            b'"' => {
                self.pos += 1;
                let mut out = String::new();
                while self.pos < self.src.len() && self.src[self.pos] != b'"' {
                    if self.src[self.pos] == b'\\' {
                        self.pos += 1;
                        let e = self.src[self.pos];
                        out.push(match e { b'n' => '\n', b't' => '\t', b'r' => '\r', _ => e as char });
                        self.pos += 1;
                    } else { out.push(self.src[self.pos] as char); self.pos += 1; }
                }
                self.pos += 1;
                Ok(ArgValue::Str(out))
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != b']' { items.push(self.parse_value()?); }
                self.expect(b']')?;
                Ok(ArgValue::List(items))
            }
            b'{' => {
                self.pos += 1;
                let mut obj = HashMap::new();
                while self.peek() != b'}' {
                    let k = self.ident()?;
                    self.expect(b':')?;
                    obj.insert(k, self.parse_value()?);
                }
                self.expect(b'}')?;
                Ok(ArgValue::Object(obj))
            }
            b'0'..=b'9' | b'-' => {
                let start = self.pos;
                while self.pos < self.src.len() && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'.' || self.src[self.pos] == b'-') { self.pos += 1; }
                let s = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                if s.contains('.') { Ok(ArgValue::Float(s.parse().unwrap_or(0.0))) } else { Ok(ArgValue::Int(s.parse().unwrap_or(0))) }
            }
            _ => {
                let id = self.ident()?;
                match id.as_str() {
                    "true" => Ok(ArgValue::Bool(true)),
                    "false" => Ok(ArgValue::Bool(false)),
                    "null" => Ok(ArgValue::Null),
                    _ => Ok(ArgValue::Enum(id)),
                }
            }
        }
    }
    fn parse_args(&mut self) -> Result<HashMap<String, ArgValue>, String> {
        let mut args = HashMap::new();
        self.skip_ws();
        if self.pos >= self.src.len() || self.src[self.pos] != b'(' { return Ok(args); }
        self.pos += 1;
        while self.peek() != b')' {
            let k = self.ident()?;
            self.expect(b':')?;
            args.insert(k, self.parse_value()?);
        }
        self.expect(b')')?;
        Ok(args)
    }
    fn parse_selection(&mut self) -> Result<Vec<Field>, String> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        while self.peek() != b'}' {
            if self.src[self.pos..].starts_with(b"...") {
                self.pos += 3;
                self.skip_ws();
                if self.src[self.pos..].starts_with(b"on") { self.pos += 2; let _ = self.ident()?; }
                else { let _ = self.ident()?; }
                let sub = self.parse_selection()?;
                fields.extend(sub);
                continue;
            }
            let mut name = self.ident()?;
            let mut alias = None;
            self.skip_ws();
            if self.pos < self.src.len() && self.src[self.pos] == b':' { alias = Some(name); self.pos += 1; name = self.ident()?; }
            let args = self.parse_args()?;
            let mut selection = None;
            self.skip_ws();
            if self.pos < self.src.len() && self.src[self.pos] == b'{' { selection = Some(self.parse_selection()?); }
            fields.push(Field { alias, name, args, selection });
        }
        self.expect(b'}')?;
        Ok(fields)
    }
    pub fn parse_operation(&mut self) -> Result<Operation, String> {
        self.skip_ws();
        let mut kind = "query".to_string();
        if self.src[self.pos..].starts_with(b"query") && !self.src.get(self.pos + 5).map(|c| c.is_ascii_alphanumeric() || *c == b'_').unwrap_or(false) { self.pos += 5; kind = "query".to_string(); }
        else if self.src[self.pos..].starts_with(b"mutation") && !self.src.get(self.pos + 8).map(|c| c.is_ascii_alphanumeric() || *c == b'_').unwrap_or(false) { self.pos += 8; kind = "mutation".to_string(); }
        self.skip_ws();
        if self.pos < self.src.len() && (self.src[self.pos].is_ascii_alphabetic() || self.src[self.pos] == b'_') { let _ = self.ident()?; }
        self.skip_ws();
        if self.pos < self.src.len() && self.src[self.pos] == b'(' {
            let mut depth = 0;
            while self.pos < self.src.len() {
                let c = self.src[self.pos]; self.pos += 1;
                if c == b'(' { depth += 1; } else if c == b')' { depth -= 1; if depth == 0 { break; } }
            }
        }
        let fields = self.parse_selection()?;
        Ok(Operation { kind, fields })
    }
}

pub fn resolve_arg(v: &ArgValue, vars: &HashMap<String, Value>) -> Value {
    match v {
        ArgValue::Var(n) => vars.get(n).cloned().unwrap_or(Value::Null),
        ArgValue::Str(s) => Value::String(s.clone()),
        ArgValue::Int(i) => json!(*i),
        ArgValue::Float(f) => json!(*f),
        ArgValue::Bool(b) => json!(*b),
        ArgValue::Null => Value::Null,
        ArgValue::Enum(s) => Value::String(s.clone()),
        ArgValue::List(items) => Value::Array(items.iter().map(|x| resolve_arg(x, vars)).collect()),
        ArgValue::Object(o) => { let mut m = Map::new(); for (k, x) in o { m.insert(k.clone(), resolve_arg(x, vars)); } Value::Object(m) }
    }
}
fn arg_of(f: &Field, name: &str, vars: &HashMap<String, Value>) -> Option<Value> {
    f.args.get(name).map(|a| resolve_arg(a, vars))
}

// ---------------------------------------------------------------- schema
fn type_fields(name: &str) -> Vec<&'static str> {
    match name {
        "Issue" => vec!["id", "number", "title", "state", "body", "createdAt", "updatedAt", "url", "author", "comments", "labels", "closed", "repository"],
        "PullRequest" => vec!["id", "number", "title", "state", "merged", "headRefName", "baseRefName", "mergeable", "files", "body", "createdAt", "updatedAt", "url", "author", "headRef", "baseRef", "mergeCommit", "repository"],
        "Repository" => vec!["id", "databaseId", "name", "nameWithOwner", "owner", "defaultBranchRef", "viewerPermission", "hasIssuesEnabled", "issue", "issues", "pullRequests", "pullRequest", "description", "url", "createdAt", "updatedAt", "isPrivate", "fork"],
        "User" => vec!["id", "login", "name", "email", "url"],
        _ => vec![],
    }
}
const SEARCH_ENUM: [&str; 8] = ["ISSUE", "PULL_REQUEST", "REPOSITORY", "USER", "DISCUSSION", "COMMIT", "MILESTONE", "LABEL"];

// ---------------------------------------------------------------- executor
pub struct Ctx<'a> { pub store: &'a mut Store, pub vars: HashMap<String, Value>, pub errors: Vec<String> }

fn issue_gid(repo: &Repo, issue: &Issue) -> String { gid("Issue", &format!("{}#{}", format!("{}/{}", repo.owner, repo.name), issue.number)) }
fn repo_gid(repo: &Repo) -> String { gid("Repository", &format!("{}/{}", repo.owner, repo.name)) }
fn pull_gid(repo: &Repo, pr: &Pull) -> String { gid("PullRequest", &format!("{}#{}", format!("{}/{}", repo.owner, repo.name), pr.number)) }

fn project_user(ctx: &mut Ctx, sel: Option<&Vec<Field>>) -> Value {
    let mut out = Map::new();
    for f in sel.unwrap_or(&vec![]) {
        let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
        match f.name.as_str() {
            "login" => { out.insert(key, json!(VIEWER_LOGIN)); }
            "name" => { out.insert(key, json!(VIEWER_NAME)); }
            "id" => { out.insert(key, json!(gid("User", VIEWER_LOGIN))); }
            "email" => { out.insert(key, json!(VIEWER_EMAIL)); }
            "url" => { out.insert(key, json!(format!("https://example.invalid/{}", VIEWER_LOGIN))); }
            "__typename" => { out.insert(key, json!("User")); }
            _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"User\".", f.name)); out.insert(key, Value::Null); }
        }
    }
    Value::Object(out)
}
fn project_issue(ctx: &mut Ctx, repo: &Repo, issue: &Issue, sel: Option<&Vec<Field>>) -> Value {
    let mut out = Map::new();
    for f in sel.unwrap_or(&vec![]) {
        let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
        match f.name.as_str() {
            "id" => { out.insert(key, json!(issue_gid(repo, issue))); }
            "number" => { out.insert(key, json!(issue.number)); }
            "title" => { out.insert(key, json!(issue.title)); }
            "body" => { out.insert(key, json!(issue.body)); }
            "state" => { out.insert(key, json!(if issue.state == "open" { "OPEN" } else { "CLOSED" })); }
            "createdAt" => { out.insert(key, json!(issue.created_at)); }
            "updatedAt" => { out.insert(key, json!(issue.updated_at)); }
            "url" => { out.insert(key, json!(format!("https://example.invalid/{}/{}/issues/{}", repo.owner, repo.name, issue.number))); }
            "author" => { let v = project_user(ctx, f.selection.as_ref()); out.insert(key, v); }
            "comments" => { out.insert(key, json!({"totalCount": issue.comments.len(), "nodes": issue.comments.iter().map(|c| json!({"id": c.gid, "body": c.body})).collect::<Vec<_>>() })); }
            "labels" => { out.insert(key, json!({"nodes": issue.labels.iter().map(|l| json!({"name": l.name})).collect::<Vec<_>>() })); }
            "closed" => { out.insert(key, json!(issue.state == "closed")); }
            "repository" => { let v = project_repo(ctx, repo, f.selection.as_ref()); out.insert(key, v); }
            "__typename" => { out.insert(key, json!("Issue")); }
            _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"Issue\".", f.name)); }
        }
    }
    Value::Object(out)
}
fn project_repo(ctx: &mut Ctx, repo: &Repo, sel: Option<&Vec<Field>>) -> Value {
    let mut out = Map::new();
    for f in sel.unwrap_or(&vec![]) {
        let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
        match f.name.as_str() {
            "id" => { out.insert(key, json!(repo_gid(repo))); }
            "databaseId" => { out.insert(key, json!(repo.id)); }
            "name" => { out.insert(key, json!(repo.name)); }
            "nameWithOwner" => { out.insert(key, json!(format!("{}/{}", repo.owner, repo.name))); }
            "owner" => { let v = project_user(ctx, f.selection.as_ref()); out.insert(key, v); }
            "description" => { out.insert(key, json!(repo.description)); }
            "isPrivate" => { out.insert(key, json!(repo.priv_)); }
            "url" => { out.insert(key, json!(format!("https://example.invalid/{}/{}", repo.owner, repo.name))); }
            "createdAt" => { out.insert(key, json!(repo.created_at)); }
            "viewerPermission" => { out.insert(key, json!("ADMIN")); }
            "hasIssuesEnabled" => { out.insert(key, json!(true)); }
            "defaultBranchRef" => {
                let tip = repo.ref_tip(&format!("refs/heads/{}", repo.default_branch));
                out.insert(key, match tip { Some(_) => json!({"name": repo.default_branch}), None => Value::Null });
            }
            "issue" => {
                let n = arg_of(f, "number", &ctx.vars).and_then(|v| v.as_i64()).unwrap_or(0);
                let issue = repo.issues.get(&n).cloned();
                out.insert(key, match issue { Some(i) => project_issue(ctx, repo, &i, f.selection.as_ref()), None => Value::Null });
            }
            "issues" => { let v = project_issues(ctx, repo, f); out.insert(key, v); }
            "pullRequests" => { let v = project_pulls(ctx, repo, f); out.insert(key, v); }
            "pullRequest" => {
                let n = arg_of(f, "number", &ctx.vars).and_then(|v| v.as_i64()).unwrap_or(0);
                let pr = repo.pulls.get(&n).cloned();
                out.insert(key, match pr { Some(p) => project_pull(ctx, repo, &p, f.selection.as_ref()), None => Value::Null });
            }
            "__typename" => { out.insert(key, json!("Repository")); }
            _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"Repository\".", f.name)); }
        }
    }
    Value::Object(out)
}
fn project_issues(ctx: &mut Ctx, repo: &Repo, f: &Field) -> Value {
    let mut list: Vec<Issue> = repo.issues.values().cloned().collect();
    if let Some(states) = arg_of(f, "states", &ctx.vars) {
        if let Some(arr) = states.as_array() {
            let want: std::collections::HashSet<String> = arr.iter().filter_map(|s| s.as_str().map(|x| x.to_lowercase())).collect();
            list.retain(|i| want.contains(&i.state));
        }
    }
    let dir = arg_of(f, "orderBy", &ctx.vars).and_then(|v| v.get("direction").and_then(|d| d.as_str()).map(|s| s.to_string())).unwrap_or_else(|| "DESC".to_string());
    let d: i64 = if dir == "ASC" { 1 } else { -1 };
    list.sort_by(|a, b| if d == 1 { a.number.cmp(&b.number) } else { b.number.cmp(&a.number) });
    let first = arg_of(f, "first", &ctx.vars).and_then(|v| v.as_i64());
    let after = arg_of(f, "after", &ctx.vars).and_then(|v| v.as_str().map(|s| s.to_string()));
    let mut start = 0usize;
    if let Some(a) = after {
        if let Some((_, key)) = decode_gid(&a) {
            if let Some(nstr) = key.split(':').nth(1) {
                if let Ok(n) = nstr.parse::<i64>() {
                    if let Some(idx) = list.iter().position(|i| i.number == n) { start = idx + 1; }
                }
            }
        }
    }
    let total = list.len();
    let page: Vec<Issue> = match first { Some(n) => list.iter().skip(start).take(n as usize).cloned().collect(), None => list.iter().skip(start).cloned().collect() };
    let end_cursor = page.last().map(|i| gid("cursor", &format!("issue:{}", i.number)));
    let node_sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "nodes")).and_then(|n| n.selection.as_ref());
    let mut nodes = Vec::new();
    for issue in &page { nodes.push(project_issue(ctx, repo, issue, node_sel)); }
    let mut out = Map::new();
    out.insert("nodes".to_string(), Value::Array(nodes));
    out.insert("totalCount".to_string(), json!(total));
    if let Some(edges_sel) = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "edges")) {
        let node_sel2 = edges_sel.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "node")).and_then(|n| n.selection.as_ref());
        let edges: Vec<Value> = page.iter().map(|issue| json!({"node": project_issue(ctx, repo, issue, node_sel2), "cursor": gid("cursor", &format!("issue:{}", issue.number))})).collect();
        out.insert("edges".to_string(), Value::Array(edges));
    }
    if let Some(pi_sel) = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "pageInfo")) {
        let mut pi = Map::new();
        for pf in pi_sel.selection.as_ref().unwrap_or(&vec![]) {
            let k = pf.alias.clone().unwrap_or_else(|| pf.name.clone());
            match pf.name.as_str() {
                "hasNextPage" => { pi.insert(k, json!(start + page.len() < total)); }
                "hasPreviousPage" => { pi.insert(k, json!(start > 0)); }
                "endCursor" => { pi.insert(k, end_cursor.clone().map(Value::String).unwrap_or(Value::Null)); }
                "startCursor" => { pi.insert(k, page.first().map(|i| Value::String(gid("cursor", &format!("issue:{}", i.number)))).unwrap_or(Value::Null)); }
                _ => {}
            }
        }
        out.insert("pageInfo".to_string(), Value::Object(pi));
    }
    Value::Object(out)
}
fn project_pulls(ctx: &mut Ctx, repo: &Repo, f: &Field) -> Value {
    let mut list: Vec<Pull> = repo.pulls.values().cloned().collect();
    if let Some(states) = arg_of(f, "states", &ctx.vars) {
        if let Some(arr) = states.as_array() {
            let want: std::collections::HashSet<String> = arr.iter().filter_map(|s| s.as_str().map(|x| x.to_uppercase())).collect();
            list.retain(|p| { let st = if p.merged { "MERGED".to_string() } else { p.state.to_uppercase() }; want.contains(&st) });
        }
    }
    list.sort_by(|a, b| b.number.cmp(&a.number));
    let first = arg_of(f, "first", &ctx.vars).and_then(|v| v.as_i64());
    let total = list.len();
    let page: Vec<Pull> = match first { Some(n) => list.iter().take(n as usize).cloned().collect(), None => list };
    let node_sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "nodes")).and_then(|n| n.selection.as_ref());
    let nodes: Vec<Value> = page.iter().map(|p| project_pull(ctx, repo, p, node_sel)).collect();
    json!({"nodes": nodes, "totalCount": total})
}
fn project_pull(ctx: &mut Ctx, repo: &Repo, pr: &Pull, sel: Option<&Vec<Field>>) -> Value {
    let mut out = Map::new();
    for f in sel.unwrap_or(&vec![]) {
        let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
        match f.name.as_str() {
            "id" => { out.insert(key, json!(pull_gid(repo, pr))); }
            "number" => { out.insert(key, json!(pr.number)); }
            "title" => { out.insert(key, json!(pr.title)); }
            "body" => { out.insert(key, json!(pr.body)); }
            "state" => { let st = if pr.merged { "MERGED".to_string() } else { pr.state.to_uppercase() }; out.insert(key, json!(st)); }
            "merged" => { out.insert(key, json!(pr.merged)); }
            "headRefName" => { out.insert(key, json!(pr.head_ref)); }
            "baseRefName" => { out.insert(key, json!(pr.base_ref)); }
            "mergeable" => { out.insert(key, json!(if pr.state == "open" { if Store::pull_conflict(repo, pr) { "CONFLICTING" } else { "MERGEABLE" } } else { "UNKNOWN" })); }
            "createdAt" => { out.insert(key, json!(pr.created_at)); }
            "url" => { out.insert(key, json!(format!("https://example.invalid/{}/{}/pull/{}", repo.owner, repo.name, pr.number))); }
            "author" => { let v = project_user(ctx, f.selection.as_ref()); out.insert(key, v); }
            "headRef" => { out.insert(key, json!({"name": pr.head_ref})); }
            "baseRef" => { out.insert(key, json!({"name": pr.base_ref})); }
            "files" => {
                let files = Store::pull_files(repo, pr);
                let first = arg_of(f, "first", &ctx.vars).and_then(|v| v.as_i64());
                let page: Vec<_> = match first { Some(n) => files.iter().take(n as usize).collect(), None => files.iter().collect() };
                let node_sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "nodes")).and_then(|n| n.selection.as_ref());
                let nodes: Vec<Value> = page.iter().map(|(filename, status, additions, deletions)| {
                    let mut o = Map::new();
                    for nf in node_sel.unwrap_or(&vec![]) {
                        let k = nf.alias.clone().unwrap_or_else(|| nf.name.clone());
                        match nf.name.as_str() {
                            "path" => { o.insert(k, json!(filename)); }
                            "additions" => { o.insert(k, json!(additions)); }
                            "deletions" => { o.insert(k, json!(deletions)); }
                            "changeType" => { o.insert(k, json!(status.to_uppercase())); }
                            _ => {}
                        }
                    }
                    Value::Object(o)
                }).collect();
                out.insert(key, json!({"nodes": nodes, "totalCount": files.len()}));
            }
            "mergeCommit" => { out.insert(key, match &pr.merge_commit_sha { Some(s) => json!({"oid": s}), None => Value::Null }); }
            "repository" => { let v = project_repo(ctx, repo, f.selection.as_ref()); out.insert(key, v); }
            "__typename" => { out.insert(key, json!("PullRequest")); }
            _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"PullRequest\".", f.name)); }
        }
    }
    Value::Object(out)
}
fn project_type(ctx: &mut Ctx, name: &str, sel: Option<&Vec<Field>>) -> Value {
    let mut out = Map::new();
    out.insert("name".to_string(), json!(name));
    for f in sel.unwrap_or(&vec![]) {
        let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
        match f.name.as_str() {
            "fields" => { out.insert(key, json!(type_fields(name).iter().map(|n| json!({"name": n})).collect::<Vec<_>>())); }
            "enumValues" => { out.insert(key, if name == "SearchType" { json!(SEARCH_ENUM.iter().map(|n| json!({"name": n})).collect::<Vec<_>>()) } else { Value::Null }); }
            "name" => { out.insert(key, json!(name)); }
            "kind" => { out.insert(key, json!(if name == "SearchType" { "ENUM" } else { "OBJECT" })); }
            _ => { out.insert(key, Value::Null); }
        }
    }
    let _ = ctx;
    Value::Object(out)
}
fn find_repo_by_gid(store: &Store, id: &str) -> Option<String> {
    let (kind, key) = decode_gid(id)?;
    if kind != "Repository" { return None; }
    let mut parts = key.splitn(2, '/');
    let owner = parts.next()?; let name = parts.next()?;
    if store.get_repo(owner, name).is_some() { Some(Store::key(owner, name)) } else { None }
}

pub fn execute_graphql(store: &mut Store, query: &str, variables: HashMap<String, Value>) -> Value {
    let mut ctx = Ctx { store, vars: variables, errors: Vec::new() };
    let op = match Parser::new(query).parse_operation() {
        Ok(o) => o,
        Err(e) => return json!({"data": null, "errors": [{"message": format!("GraphQL parse error: {}", e)}]}),
    };
    let mut data = Map::new();
    if op.kind == "mutation" {
        for f in &op.fields {
            let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
            let input = arg_of(f, "input", &ctx.vars).and_then(|v| if let Value::Object(m) = v { Some(m) } else { None }).unwrap_or_default();
            let get_s = |k: &str| -> String { input.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string() };
            match f.name.as_str() {
                "createIssue" => {
                    let repo_key = get_s("repositoryId");
                    let repo_key = find_repo_by_gid(ctx.store, &repo_key);
                    match repo_key {
                        None => { ctx.errors.push("Could not resolve to a Repository with the id provided.".into()); data.insert(key, Value::Null); }
                        Some(rk) => {
                            let title = get_s("title"); let body = get_s("body");
                            match ctx.store.create_issue(&rk, title, body, &[]) {
                                Err(e) => { ctx.errors.push(e); data.insert(key, Value::Null); }
                                Ok(num) => {
                                    let (repo_c, issue_c) = { let r = ctx.store.get_repo(&rk.split('/').next().unwrap().to_string(), &rk.split('/').nth(1).unwrap().to_string()).unwrap(); (r.clone(), r.issues.get(&num).unwrap().clone()) };
                                    let sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "issue")).and_then(|n| n.selection.as_ref());
                                    let v = project_issue(&mut ctx, &repo_c, &issue_c, sel);
                                    data.insert(key, json!({"issue": v}));
                                }
                            }
                        }
                    }
                }
                "closeIssue" => {
                    let id = get_s("issueId");
                    let dec = decode_gid(&id);
                    let mut found: Option<(String, i64)> = None;
                    if let Some((kind, k)) = dec {
                        if kind == "Issue" {
                            if let Some(cap) = parse_issue_key(&k) { found = Some(cap); }
                        }
                    }
                    match found {
                        None => { ctx.errors.push("Could not resolve to an Issue with the id provided.".into()); data.insert(key, Value::Null); }
                        Some((rk, num)) => {
                            let (owner, name) = split_key(&rk);
                            let repo_c = ctx.store.get_repo(&owner, &name).unwrap().clone();
                            let issue_c = repo_c.issues.get(&num).unwrap().clone();
                            // mutate
                            { let r = ctx.store.get_repo_mut(&owner, &name).unwrap(); let iss = r.issues.get_mut(&num).unwrap(); iss.state = "closed".to_string(); iss.json_cache = None; }
                            let sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "issue")).and_then(|n| n.selection.as_ref());
                            let v = project_issue(&mut ctx, &repo_c, &issue_c, sel);
                            // project_issue used the pre-close clone; Node projects after close. Re-project with closed state:
                            let mut issue_closed = issue_c.clone(); issue_closed.state = "closed".to_string();
                            let v = project_issue(&mut ctx, &repo_c, &issue_closed, sel);
                            let _ = v; let _ = v;
                            data.insert(key, json!({"issue": project_issue(&mut ctx, &repo_c, &issue_closed, sel)}));
                        }
                    }
                }
                "createPullRequest" => {
                    let repo_key = find_repo_by_gid(ctx.store, &get_s("repositoryId"));
                    match repo_key {
                        None => { ctx.errors.push("Could not resolve to a Repository with the id provided.".into()); data.insert(key, Value::Null); }
                        Some(rk) => {
                            let title = get_s("title"); let body = get_s("body");
                            let head = input.get("headRefName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let base = input.get("baseRefName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            match ctx.store.create_pull(&rk, title, body, &head, &base) {
                                Err(e) => { ctx.errors.push(e); data.insert(key, Value::Null); }
                                Ok(num) => {
                                    let (owner, name) = split_key(&rk);
                                    let repo_c = ctx.store.get_repo(&owner, &name).unwrap().clone();
                                    let pr_c = repo_c.pulls.get(&num).unwrap().clone();
                                    let sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "pullRequest")).and_then(|n| n.selection.as_ref());
                                    let v = project_pull(&mut ctx, &repo_c, &pr_c, sel);
                                    data.insert(key, json!({"pullRequest": v}));
                                }
                            }
                        }
                    }
                }
                "mergePullRequest" => {
                    let id = get_s("pullRequestId");
                    let dec = decode_gid(&id);
                    let mut found: Option<(String, i64)> = None;
                    if let Some((kind, k)) = dec { if kind == "PullRequest" { found = parse_issue_key(&k); } }
                    match found {
                        None => { ctx.errors.push("Could not resolve to a PullRequest with the id provided.".into()); data.insert(key, Value::Null); }
                        Some((rk, num)) => {
                            let method = get_s("mergeMethod");
                            let m = if method.to_lowercase() == "merge" { "merge" } else { "squash" };
                            match ctx.store.merge_pull(&rk, num, m) {
                                Err((e, _)) => { ctx.errors.push(e); data.insert(key, Value::Null); }
                                Ok(_) => {
                                    let (owner, name) = split_key(&rk);
                                    let repo_c = ctx.store.get_repo(&owner, &name).unwrap().clone();
                                    let pr_c = repo_c.pulls.get(&num).unwrap().clone();
                                    let sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "pullRequest")).and_then(|n| n.selection.as_ref());
                                    let v = project_pull(&mut ctx, &repo_c, &pr_c, sel);
                                    data.insert(key, json!({"pullRequest": v}));
                                }
                            }
                        }
                    }
                }
                "addComment" => {
                    let id = get_s("subjectId");
                    let dec = decode_gid(&id);
                    let mut found: Option<(String, i64)> = None;
                    if let Some((kind, k)) = dec { if kind == "Issue" { found = parse_issue_key(&k); } }
                    match found {
                        None => { ctx.errors.push("Could not resolve to a subject with the id provided.".into()); data.insert(key, Value::Null); }
                        Some((rk, num)) => {
                            let body = get_s("body");
                            let (owner, name) = split_key(&rk);
                            let comment = {
                                let repo = ctx.store.get_repo_mut(&owner, &name).unwrap();
                                let issue = repo.issues.get_mut(&num).unwrap();
                                let cid = repo.next_comment_id; repo.next_comment_id += 1;
                                let c = Comment { id: cid, body: body.clone(), user: VIEWER_LOGIN.to_string(), created_at: iso_of(EPOCH + 200000 + cid), gid: gid("Comment", &format!("{}#{}", rk, format!("/c{}", cid)).replace(&format!("{}#", rk), &format!("{}#", rk))) };
                                // Node gid key: `${owner}/${name}#${issue.number}/c${id}`
                                let c = Comment { gid: gid("Comment", &format!("{}#{}", format!("{}/{}", owner, name), format!("{}/c{}", num, cid))), ..c };
                                issue.comments.push(c.clone());
                                c
                            };
                            let node_sel = f.selection.as_ref().and_then(|s| s.iter().find(|x| x.name == "commentEdge")).and_then(|n| n.selection.as_ref()).and_then(|s| s.iter().find(|x| x.name == "node")).and_then(|n| n.selection.as_ref());
                            let mut node = Map::new();
                            for nf in node_sel.unwrap_or(&vec![]) {
                                let k = nf.alias.clone().unwrap_or_else(|| nf.name.clone());
                                match nf.name.as_str() {
                                    "id" => { node.insert(k, json!(comment.gid)); }
                                    "body" => { node.insert(k, json!(comment.body)); }
                                    _ => {}
                                }
                            }
                            data.insert(key, json!({"commentEdge": {"node": node}}));
                        }
                    }
                }
                _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"Mutation\".", f.name)); data.insert(key, Value::Null); }
            }
        }
    } else {
        for f in &op.fields {
            let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
            match f.name.as_str() {
                "viewer" => { let v = project_user(&mut ctx, f.selection.as_ref()); data.insert(key, v); }
                "repository" => {
                    let owner = arg_of(f, "owner", &ctx.vars).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    let name = arg_of(f, "name", &ctx.vars).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    let repo = if owner == VIEWER_LOGIN { ctx.store.get_repo(&owner, &name).cloned() } else { None };
                    data.insert(key, match repo { Some(r) => project_repo(&mut ctx, &r, f.selection.as_ref()), None => Value::Null });
                }
                "node" => {
                    let id = arg_of(f, "id", &ctx.vars).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    let dec = decode_gid(&id);
                    match dec {
                        None => { data.insert(key, Value::Null); }
                        Some((kind, k)) => {
                            if kind == "Repository" {
                                let (owner, name) = split_key(&k);
                                let repo = ctx.store.get_repo(&owner, &name).cloned();
                                match repo {
                                    None => { data.insert(key, Value::Null); }
                                    Some(r) => {
                                        let mut node = Map::new(); node.insert("id".to_string(), json!(id));
                                        for nf in f.selection.as_ref().unwrap_or(&vec![]) {
                                            let kk = nf.alias.clone().unwrap_or_else(|| nf.name.clone());
                                            match nf.name.as_str() {
                                                "id" => { node.insert(kk, json!(id)); }
                                                "name" => { node.insert(kk, json!(r.name)); }
                                                "nameWithOwner" => { node.insert(kk, json!(format!("{}/{}", r.owner, r.name))); }
                                                "__typename" => { node.insert(kk, json!("Repository")); }
                                                _ => {}
                                            }
                                        }
                                        data.insert(key, Value::Object(node));
                                    }
                                }
                            } else if kind == "Issue" {
                                let parsed = parse_issue_key(&k);
                                match parsed {
                                    None => { data.insert(key, Value::Null); }
                                    Some((rk, num)) => {
                                        let (owner, name) = split_key(&rk);
                                        let repo = ctx.store.get_repo(&owner, &name).cloned();
                                        let issue = repo.as_ref().and_then(|r| r.issues.get(&num).cloned());
                                        match (repo, issue) { (Some(r), Some(i)) => { let v = project_issue(&mut ctx, &r, &i, f.selection.as_ref()); data.insert(key, v); } _ => { data.insert(key, Value::Null); } }
                                    }
                                }
                            } else if kind == "PullRequest" {
                                let parsed = parse_issue_key(&k);
                                match parsed {
                                    None => { data.insert(key, Value::Null); }
                                    Some((rk, num)) => {
                                        let (owner, name) = split_key(&rk);
                                        let repo = ctx.store.get_repo(&owner, &name).cloned();
                                        let pr = repo.as_ref().and_then(|r| r.pulls.get(&num).cloned());
                                        match (repo, pr) { (Some(r), Some(p)) => { let v = project_pull(&mut ctx, &r, &p, f.selection.as_ref()); data.insert(key, v); } _ => { data.insert(key, Value::Null); } }
                                    }
                                }
                            } else { data.insert(key, Value::Null); }
                        }
                    }
                }
                "__type" => {
                    let name = arg_of(f, "name", &ctx.vars).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
                    let known = type_fields(&name).len() > 0 || name == "SearchType" || name == "Query" || name == "Mutation";
                    data.insert(key, if known { project_type(&mut ctx, &name, f.selection.as_ref()) } else { Value::Null });
                }
                "__schema" => {
                    let types: Vec<Value> = ["Issue", "PullRequest", "Repository", "User", "SearchType"].iter().map(|n| json!({"name": n})).collect();
                    data.insert(key, json!({"types": types}));
                }
                _ => { ctx.errors.push(format!("Cannot query field \"{}\" on type \"Query\".", f.name)); data.insert(key, Value::Null); }
            }
        }
    }
    let mut out = Map::new();
    out.insert("data".to_string(), Value::Object(data));
    if !ctx.errors.is_empty() {
        out.insert("errors".to_string(), json!(ctx.errors.iter().map(|m| json!({"message": m})).collect::<Vec<_>>()));
    }
    Value::Object(out)
}

fn split_key(k: &str) -> (String, String) {
    let mut p = k.splitn(2, '/');
    (p.next().unwrap_or("").to_string(), p.next().unwrap_or("").to_string())
}
fn parse_issue_key(k: &str) -> Option<(String, i64)> {
    // key like "owner/name#123" or "owner/name#123/c4"
    let hash = k.rfind('#')?;
    let rk = &k[..hash];
    let rest = &k[hash + 1..];
    let num_str = rest.split('/').next()?;
    let num: i64 = num_str.parse().ok()?;
    Some((rk.to_string(), num))
}
