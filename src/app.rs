// GitHub forge HTTP handlers — port of app.ts (Node entry).
use crate::git::*;
use crate::graphql::execute_graphql;
use crate::store::*;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(status: u16, body: &str, extra: &[(&str, &str)]) -> Response {
        let mut h = vec![
            ("Content-Type".to_string(), "application/json; charset=utf-8".to_string()),
            ("X-GitHub-Request-Id".to_string(), req_id()),
            ("Content-Length".to_string(), body.len().to_string()),
        ];
        for (k, v) in extra { h.push((k.to_string(), v.to_string())); }
        Response { status, headers: h, body: body.as_bytes().to_vec() }
    }
    pub fn json_value(status: u16, v: &Value, extra: &[(&str, &str)]) -> Response {
        Self::json(status, &serde_json::to_string(v).unwrap(), extra)
    }
    pub fn raw(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response { status, headers: vec![
            ("Content-Type".to_string(), content_type.to_string()),
            ("X-GitHub-Request-Id".to_string(), req_id()),
            ("Content-Length".to_string(), body.len().to_string()),
        ], body }
    }
    pub fn empty(status: u16) -> Response {
        Response { status, headers: vec![("X-GitHub-Request-Id".to_string(), req_id()), ("Content-Length".to_string(), "0".to_string())], body: Vec::new() }
    }
}
static mut REQ_SEQ: u64 = 0;
pub fn req_id() -> String {
    unsafe { REQ_SEQ += 1; format!("req_{:08x}", REQ_SEQ) }
}
fn not_found() -> Response { Response::json_value(404, &json!({"message": "Not Found"}), &[]) }
fn bad_request(msg: &str) -> Response { Response::json_value(400, &json!({"message": msg}), &[]) }
fn unauthorized() -> Response { Response::json_value(401, &json!({"message": "Requires authentication"}), &[]) }
fn unprocessable(msg: &str) -> Response { Response::json_value(422, &json!({"message": msg}), &[]) }

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| -> Option<u8> { match c { b'0'..=b'9' => Some(c - b'0'), b'a'..=b'f' => Some(c - b'a' + 10), b'A'..=b'F' => Some(c - b'A' + 10), _ => None } };
            if let (Some(a), Some(c)) = (h(b[i + 1]), h(b[i + 2])) { out.push((a << 4) | c); i += 3; continue; }
        }
        if b[i] == b'+' { out.push(b' '); i += 1; continue; }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
pub fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() { continue; }
        let (k, v) = match pair.find('=') { Some(i) => (&pair[..i], &pair[i + 1..]), None => (pair, "") };
        m.insert(percent_decode(k), percent_decode(v));
    }
    m
}

fn auth_ok(req: &Request) -> bool {
    match req.headers.get("authorization") {
        Some(h) => { let h = h.trim(); h.starts_with("Bearer ") && h[7..].trim().len() > 0 }
        None => false,
    }
}
fn is_json_body(req: &Request) -> bool {
    let ct = req.headers.get("content-type").map(|s| s.as_str()).unwrap_or("");
    ct.is_empty() || ct.contains("application/json") || ct.contains("text/plain")
}
fn read_body_json(req: &Request) -> Result<Map<String, Value>, Response> {
    if req.body.is_empty() { return Ok(Map::new()); }
    if !is_json_body(req) { return Err(bad_request("Invalid JSON")); }
    match serde_json::from_slice::<Value>(&req.body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Ok(Map::new()),
        Err(_) => Err(bad_request("Invalid JSON")),
    }
}
fn json_str(v: &Value) -> String { serde_json::to_string(v).unwrap() }
fn get_s(m: &Map<String, Value>, k: &str) -> String { m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string() }
fn get_bool(m: &Map<String, Value>, k: &str) -> bool { m.get(k).and_then(|v| v.as_bool()).unwrap_or(false) }

const LABEL_COLORS: &[(&str, &str)] = &[
    ("bug", "d73a4a"), ("enhancement", "a2eeef"), ("documentation", "0075ca"), ("question", "d876e3"),
    ("good first issue", "7057ff"), ("help wanted", "008672"),
];
fn label_color(name: &str) -> String {
    LABEL_COLORS.iter().find(|(n, _)| *n == name).map(|(_, c)| c.to_string()).unwrap_or_else(|| "ededed".to_string())
}

// ---------------------------------------------------------------- repo/issue JSON (cached, like Node)
pub fn repo_json(repo: &Repo) -> String {
    if let Some(s) = &repo.json_cache { return s.clone(); }
    let open = repo.issues.values().filter(|i| i.state == "open").count();
    json!({
        "id": repo.id, "node_id": gid("Repository", &format!("{}/{}", repo.owner, repo.name)),
        "name": repo.name, "full_name": format!("{}/{}", repo.owner, repo.name),
        "owner": {"login": repo.owner, "id": VIEWER_ID, "type": "User"},
        "private": repo.priv_, "description": repo.description, "fork": false,
        "html_url": format!("https://example.invalid/{}/{}", repo.owner, repo.name),
        "url": format!("https://api.example.invalid/repos/{}/{}", repo.owner, repo.name),
        "default_branch": repo.default_branch, "has_issues": true, "has_projects": true,
        "has_wiki": true, "created_at": repo.created_at, "updated_at": repo.created_at,
        "pushed_at": repo.created_at, "size": 0,
        "open_issues_count": open,
    }).to_string()
}
pub fn issue_json(repo: &Repo, issue: &Issue) -> String {
    if let Some(s) = &issue.json_cache { return s.clone(); }
    json!({
        "id": issue.id, "node_id": gid("Issue", &format!("{}#{}", format!("{}/{}", repo.owner, repo.name), issue.number)),
        "number": issue.number, "title": issue.title, "body": issue.body, "state": issue.state,
        "user": {"login": VIEWER_LOGIN, "id": VIEWER_ID, "type": "User"},
        "labels": issue.labels.iter().map(|l| json!({"name": l.name, "color": l.color, "description": l.description})).collect::<Vec<_>>(),
        "assignees": [], "locked": false, "comments": issue.comments.len(),
        "created_at": issue.created_at, "updated_at": issue.updated_at,
        "closed_at": if issue.state == "closed" { Value::String(issue.updated_at.clone()) } else { Value::Null },
        "repository_url": format!("https://api.example.invalid/repos/{}/{}", repo.owner, repo.name),
        "html_url": format!("https://example.invalid/{}/{}/issues/{}", repo.owner, repo.name, issue.number),
    }).to_string()
}
pub fn pull_json(store: &Store, repo: &Repo, pr: &Pull) -> Value {
    let head_sha = Store::branch_tip(repo, &pr.head_ref).unwrap_or_else(|| pr.head_sha.clone());
    let base_sha = Store::branch_tip(repo, &pr.base_ref).unwrap_or_else(|| pr.base_sha.clone());
    json!({
        "id": pr.id, "node_id": gid("PullRequest", &format!("{}#{}", format!("{}/{}", repo.owner, repo.name), pr.number)),
        "number": pr.number, "title": pr.title, "body": pr.body,
        "state": if pr.merged { "closed" } else { &pr.state },
        "merged": pr.merged, "merged_at": pr.merged_at, "merge_commit_sha": pr.merge_commit_sha,
        "user": {"login": VIEWER_LOGIN, "id": VIEWER_ID, "type": "User"},
        "head": {"ref": pr.head_ref, "sha": head_sha, "label": format!("{}:{}", repo.owner, pr.head_ref)},
        "base": {"ref": pr.base_ref, "sha": base_sha, "label": format!("{}:{}", repo.owner, pr.base_ref)},
        "created_at": pr.created_at, "updated_at": pr.created_at,
        "closed_at": if pr.state == "closed" { Value::String(pr.created_at.clone()) } else { Value::Null },
        "html_url": format!("https://example.invalid/{}/{}/pull/{}", repo.owner, repo.name, pr.number),
    })
}
fn comment_json(repo: &Repo, issue: &Issue, c: &Comment) -> Value {
    json!({
        "id": c.id, "node_id": c.gid, "body": c.body,
        "user": {"login": c.user, "id": VIEWER_ID, "type": "User"},
        "created_at": c.created_at, "updated_at": c.created_at,
        "html_url": format!("https://example.invalid/{}/{}/issues/{}#issuecomment-{}", repo.owner, repo.name, issue.number, c.id),
    })
}

// ---------------------------------------------------------------- git smart HTTP
const V0_UPLOAD_CAPS: &str = "multi_ack_detailed multi_ack ofs-delta side-band-64k thin-pack no-progress include-tag agent=forge/1.0";
const V0_RECEIVE_CAPS: &str = "report-status side-band-64k delete-refs ofs-delta agent=forge/1.0";

fn refs_list(repo: &Repo) -> Vec<(String, String, Option<String>)> {
    // (oid, name, peeled_target)
    let mut out = Vec::new();
    for (name, oid) in &repo.refs {
        out.push((oid.clone(), name.clone(), None));
        if name.starts_with("refs/tags/") {
            if let Some(o) = repo.objects.get(oid) {
                if o.typ == ObjType::Tag {
                    let text = String::from_utf8_lossy(&o.data);
                    for line in text.lines() {
                        if let Some(t) = line.strip_prefix("object ") { out.push((t.to_string(), format!("{}^{{}}", name), Some(oid.clone()))); break; }
                    }
                }
            }
        }
    }
    out
}

fn handle_info_refs(req: &Request, repo: &Repo, service: &str) -> Response {
    let refs = refs_list(repo);
    let is_v2 = req.headers.get("git-protocol").map(|h| h.contains("version=2")).unwrap_or(false);
    if service == "git-upload-pack" {
        if is_v2 {
            let body = pkt_lines(&[PktItem::from("version 2"), PktItem::from("agent=forge/1.0"), PktItem::from("ls-refs=unborn"), PktItem::from("fetch=shallow filter"), PktItem::from("object-format=sha1"), PktItem::Flush]);
            return Response::raw(200, "application/x-git-upload-pack-advertisement", body);
        }
        let mut items: Vec<PktItem> = vec![PktItem::from("# service=git-upload-pack\n"), PktItem::Flush];
        if refs.is_empty() {
            items.push(PktItem::from(format!("{} capabilities^{{}}\0{}", ZERO_OID, V0_UPLOAD_CAPS)));
        } else {
            for (i, (oid, name, _)) in refs.iter().enumerate() {
                if i == 0 { items.push(PktItem::from(format!("{} {}\0{}", oid, name, V0_UPLOAD_CAPS))); }
                else { items.push(PktItem::from(format!("{} {}", oid, name))); }
            }
        }
        items.push(PktItem::Flush);
        return Response::raw(200, "application/x-git-upload-pack-advertisement", pkt_lines(&items));
    }
    let mut items: Vec<PktItem> = vec![PktItem::from("# service=git-receive-pack\n"), PktItem::Flush];
    if refs.is_empty() {
        items.push(PktItem::from(format!("{} capabilities^{{}}\0{}", ZERO_OID, V0_RECEIVE_CAPS)));
    } else {
        for (i, (oid, name, _)) in refs.iter().enumerate() {
            if i == 0 { items.push(PktItem::from(format!("{} {}\0{}", oid, name, V0_RECEIVE_CAPS))); }
            else { items.push(PktItem::from(format!("{} {}", oid, name))); }
        }
    }
    items.push(PktItem::Flush);
    Response::raw(200, "application/x-git-receive-pack-advertisement", pkt_lines(&items))
}

fn pack_from_plan(store: &mut Store, repo: &Repo, plan: &FetchPlan, key: &str) -> Vec<u8> {
    if let Some(hit) = store.pack_cache.get(key) { return hit.clone(); }
    let objs: Vec<&GitObj> = plan.send.iter().filter_map(|oid| repo.objects.get(oid)).collect();
    let pack = write_pack(&objs);
    if store.pack_cache.len() > 64 { store.pack_cache.clear(); }
    store.pack_cache.insert(key.to_string(), pack.clone());
    pack
}
fn plan_key(repo: &Repo, wants: &std::collections::HashSet<String>, haves: &std::collections::HashSet<String>, shallow: &std::collections::HashSet<String>, depth: Option<usize>) -> String {
    let mut w: Vec<_> = wants.iter().cloned().collect(); w.sort();
    let mut h: Vec<_> = haves.iter().cloned().collect(); h.sort();
    let mut s: Vec<_> = shallow.iter().cloned().collect(); s.sort();
    format!("{}:{}:{}:{}:{}:{}", repo.id, repo.pack_version, w.join(","), h.join(","), s.join(","), depth.map(|d| d.to_string()).unwrap_or_default())
}

fn handle_upload_pack(req: &Request, store: &mut Store, repo_key: &str) -> Response {
    let repo = store.get_repo(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap().clone();
    let body = req.body.clone();
    let is_v2 = req.headers.get("git-protocol").map(|h| h.contains("version=2")).unwrap_or(false);
    if is_v2 {
        let (packets, _) = decode_pkt_full(&body);
        let mut arg_lines: Vec<String> = Vec::new();
        for l in &packets {
            if matches!(l, Packet::Flush) { break; }
            if let Packet::Data(d) = l { arg_lines.push(String::from_utf8_lossy(d).trim().to_string()); }
        }
        let mut command = String::new();
        let mut prefixes: Vec<String> = Vec::new();
        let mut wants = std::collections::HashSet::new();
        let mut haves = std::collections::HashSet::new();
        let mut shallow_bounds = std::collections::HashSet::new();
        let mut depth: Option<usize> = None;
        if !arg_lines.is_empty() {
            let first = &arg_lines[0];
            command = if let Some(c) = first.strip_prefix("command=") { c.to_string() } else { first.clone() };
        }
        for s in arg_lines.iter().skip(1) {
            if let Some(v) = s.strip_prefix("want ") { wants.insert(v.trim().to_string()); }
            else if let Some(v) = s.strip_prefix("have ") { haves.insert(v.trim().to_string()); }
            else if let Some(v) = s.strip_prefix("shallow ") { shallow_bounds.insert(v.trim().to_string()); }
            else if let Some(v) = s.strip_prefix("deepen ") { depth = v.parse().ok(); }
            else if let Some(v) = s.strip_prefix("ref-prefix ") { prefixes.push(v.to_string()); }
            else if s == "done" { break; }
        }
        if command == "ls-refs" {
            let all_refs = refs_list(&repo);
            let mut peel_of: HashMap<String, String> = HashMap::new();
            for (oid, name, _) in &all_refs { if name.ends_with("^{}") { peel_of.insert(name[..name.len() - 3].to_string(), oid.clone()); } }
            let refs: Vec<_> = all_refs.iter().filter(|(_, n, _)| !n.ends_with("^{}")).collect();
            let keep = |name: &str| -> bool { prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p.as_str())) };
            let head_tip = repo.ref_tip(&format!("refs/heads/{}", repo.default_branch));
            let mut items: Vec<PktItem> = Vec::new();
            if keep("HEAD") {
                match head_tip {
                    Some(t) => items.push(PktItem::from(format!("{} HEAD symref-target:refs/heads/{}\n", t, repo.default_branch))),
                    None => items.push(PktItem::from(format!("unborn HEAD symref-target:refs/heads/{}\n", repo.default_branch))),
                }
            }
            for (oid, name, _) in refs {
                if !keep(name) { continue; }
                match peel_of.get(name) {
                    Some(p) => items.push(PktItem::from(format!("{} {} peeled:{}\n", oid, name, p))),
                    None => items.push(PktItem::from(format!("{} {}\n", oid, name))),
                }
            }
            items.push(PktItem::Flush);
            return Response::raw(200, "application/x-git-upload-pack-result", pkt_lines(&items));
        }
        // fetch
        let key = plan_key(&repo, &wants, &haves, &shallow_bounds, depth);
        let fkey = format!("v2:{}", key);
        if let Some(hit) = store.fetch_cache.get(&fkey) {
            return Response::raw(200, "application/x-git-upload-pack-result", hit.clone());
        }
        let plan = plan_fetch(&repo.objects, &wants.iter().cloned().collect::<Vec<_>>(), &haves.iter().cloned().collect::<Vec<_>>(), &shallow_bounds.iter().cloned().collect::<Vec<_>>(), depth);
        let pack = pack_from_plan(store, &repo, &plan, &key);
        let mut parts: Vec<u8> = Vec::new();
        if !haves.is_empty() { parts.extend_from_slice(&pkt_lines(&[PktItem::from("acknowledgments\n"), PktItem::from("NAK\n"), PktItem::Flush])); }
        if !plan.shallow.is_empty() || !plan.unshallow.is_empty() {
            let mut items: Vec<PktItem> = vec![PktItem::from("shallow-info\n")];
            for x in &plan.shallow { items.push(PktItem::from(format!("shallow {}\n", x))); }
            for x in &plan.unshallow { items.push(PktItem::from(format!("unshallow {}\n", x))); }
            items.push(PktItem::Flush);
            parts.extend_from_slice(&pkt_lines(&items));
        }
        parts.extend_from_slice(&pkt(b"packfile\n"));
        let mut off = 0;
        while off < pack.len() {
            let chunk = &pack[off..std::cmp::min(pack.len(), off + 65515)];
            let mut frame = vec![1u8]; frame.extend_from_slice(chunk);
            parts.extend_from_slice(&pkt(&frame));
            off += chunk.len();
        }
        parts.extend_from_slice(FLUSH);
        if store.fetch_cache.len() > 64 { store.fetch_cache.clear(); }
        store.fetch_cache.insert(fkey, parts.clone());
        return Response::raw(200, "application/x-git-upload-pack-result", parts);
    }
    // v0 fetch
    let (packets, _) = decode_pkt_full(&body);
    let mut wants = std::collections::HashSet::new();
    let mut haves = std::collections::HashSet::new();
    let mut shallow_bounds = std::collections::HashSet::new();
    let mut depth: Option<usize> = None;
    for l in &packets {
        if let Packet::Data(d) = l {
            let s = String::from_utf8_lossy(d).into_owned();
            if s.starts_with("want ") { wants.insert(s[5..std::cmp::min(45, s.len())].to_string()); }
            else if s.starts_with("have ") { haves.insert(s[5..std::cmp::min(45, s.len())].to_string()); }
            else if s.starts_with("shallow ") { shallow_bounds.insert(s[8..std::cmp::min(48, s.len())].trim().to_string()); }
            else if s.starts_with("deepen ") { depth = s[7..].parse().ok(); }
            else if s == "done" { break; }
        }
    }
    let key = plan_key(&repo, &wants, &haves, &shallow_bounds, depth);
    let side_band = String::from_utf8_lossy(&body).contains("side-band-64k");
    let fkey = format!("v0:{}:{}", if side_band { "sb" } else { "raw" }, key);
    if let Some(hit) = store.fetch_cache.get(&fkey) {
        return Response::raw(200, "application/x-git-upload-pack-result", hit.clone());
    }
    let plan = plan_fetch(&repo.objects, &wants.iter().cloned().collect::<Vec<_>>(), &haves.iter().cloned().collect::<Vec<_>>(), &shallow_bounds.iter().cloned().collect::<Vec<_>>(), depth);
    let pack = pack_from_plan(store, &repo, &plan, &key);
    let mut parts: Vec<u8> = Vec::new();
    if !plan.shallow.is_empty() || !plan.unshallow.is_empty() {
        let mut items: Vec<PktItem> = Vec::new();
        for x in &plan.shallow { items.push(PktItem::from(format!("shallow {}", x))); }
        for x in &plan.unshallow { items.push(PktItem::from(format!("unshallow {}", x))); }
        parts.extend_from_slice(&pkt_lines(&items));
    }
    let body_str = String::from_utf8_lossy(&body).into_owned();
    if body_str.contains("side-band-64k") {
        let mut off = 0;
        while off < pack.len() {
            let chunk = &pack[off..std::cmp::min(pack.len(), off + 65515)];
            let mut frame = vec![1u8]; frame.extend_from_slice(chunk);
            parts.extend_from_slice(&pkt(&frame));
            off += chunk.len();
        }
        parts.extend_from_slice(FLUSH);
    } else {
        parts.extend_from_slice(&pack);
    }
    if store.fetch_cache.len() > 64 { store.fetch_cache.clear(); }
    store.fetch_cache.insert(fkey, parts.clone());
    Response::raw(200, "application/x-git-upload-pack-result", parts)
}

fn handle_receive_pack(req: &Request, store: &mut Store, repo_key: &str) -> Response {
    let body = req.body.clone();
    let (packets, offset) = decode_pkt_full(&body);
    let mut commands: Vec<(String, String, String)> = Vec::new();
    for l in &packets {
        if matches!(l, Packet::Flush) { break; }
        if let Packet::Data(d) = l {
            let raw = String::from_utf8_lossy(d).trim_end().to_string();
            let s = raw.split('\0').next().unwrap_or("").to_string();
            let parts: Vec<&str> = s.split(' ').collect();
            if parts.len() >= 3 { commands.push((parts[0].to_string(), parts[1].to_string(), parts[2..].join(" "))); }
        }
    }
    let pack_data = &body[offset..];
    if pack_data.len() > 12 {
        let repo = store.get_repo(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap().clone();
        if let Ok(parsed) = read_pack(pack_data, &|oid: &str| repo.objects.get(oid).cloned()) {
            let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
            for (oid, o) in parsed.objects { repo.objects.insert(oid, o); }
        }
    }
    let mut report: Vec<String> = Vec::new();
    for (old_oid, new_oid, r) in &commands {
        let repo = store.get_repo(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap().clone();
        let current = repo.ref_tip(r).unwrap_or_else(|| ZERO_OID.to_string());
        let is_delete = new_oid == ZERO_OID;
        let is_create = old_oid == ZERO_OID;
        let mut ok = false; let mut reason = String::new();
        if is_delete {
            let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
            repo.del_ref(r); ok = true;
        } else if is_create {
            if current == ZERO_OID || current == *old_oid {
                let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
                repo.set_ref(r, new_oid.clone()); ok = true;
            } else { reason = "non-fast-forward".to_string(); }
        } else {
            if current != *old_oid { reason = "non-fast-forward".to_string(); }
            else if old_oid == new_oid { ok = true; }
            else {
                let new_obj = repo.objects.get(new_oid).cloned();
                let new_parents = match &new_obj { Some(o) if o.typ == ObjType::Commit => parse_commit(&o.data).parents, _ => Vec::new() };
                if new_parents.contains(old_oid) {
                    let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
                    repo.set_ref(r, new_oid.clone()); ok = true;
                } else {
                    let old_commit = Store::peel_to_commit(&repo, old_oid);
                    let new_commit = Store::peel_to_commit(&repo, new_oid);
                    if let (Some(oc), Some(nc)) = (old_commit, new_commit) {
                        if is_ancestor(&repo.objects, &oc, &nc) {
                            let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
                            repo.set_ref(r, new_oid.clone()); ok = true;
                        } else { reason = "non-fast-forward".to_string(); }
                    } else { reason = "non-fast-forward".to_string(); }
                }
            }
        }
        if ok { report.push(format!("ok {}", r)); } else { report.push(format!("ng {} {}", r, if reason.is_empty() { "failed" } else { &reason })); }
    }
    {
        let repo = store.get_repo_mut(&repo_key.split('/').next().unwrap().to_string(), &repo_key.split('/').nth(1).unwrap().to_string()).unwrap();
        repo.pack_version += 1;
        repo.json_cache = None;
    }
    let mut payload = pkt(b"unpack ok\n");
    for r in &report { payload.extend_from_slice(&pkt(format!("{}\n", r).as_bytes())); }
    let mut frame = vec![1u8]; frame.extend_from_slice(&payload);
    let mut out = pkt(&frame);
    out.extend_from_slice(FLUSH);
    Response::raw(200, "application/x-git-receive-pack-result", out)
}

// ---------------------------------------------------------------- REST
fn page_params(q: &HashMap<String, String>) -> (i64, i64) {
    let per_page = q.get("per_page").and_then(|v| v.parse::<i64>().ok()).unwrap_or(30).clamp(1, 100);
    let page = q.get("page").and_then(|v| v.parse::<i64>().ok()).unwrap_or(1).max(1);
    (page, per_page)
}
fn paginate_link(total: usize, q: &HashMap<String, String>, base: &str) -> String {
    let (page, per_page) = page_params(q);
    let total_pages = ((total as i64 + per_page - 1) / per_page).max(0);
    let mut links = Vec::new();
    if page < total_pages { links.push(format!("<{}?per_page={}&page={}>; rel=\"next\"", base, per_page, page + 1)); }
    if page > 1 { links.push(format!("<{}?per_page={}&page={}>; rel=\"prev\"", base, per_page, page - 1)); }
    links.push(format!("<{}?per_page={}&page=1>; rel=\"first\"", base, per_page));
    if total_pages > 0 { links.push(format!("<{}?per_page={}&page={}>; rel=\"last\"", base, per_page, total_pages)); }
    links.join(", ")
}
fn paginate_slice<T: Clone>(items: &[T], q: &HashMap<String, String>) -> Vec<T> {
    let (page, per_page) = page_params(q);
    let start = ((page - 1) * per_page) as usize;
    items.iter().skip(start).take(per_page as usize).cloned().collect()
}
fn abs_url(req: &Request, path: &str) -> String {
    let host = req.headers.get("host").cloned().unwrap_or_else(|| "localhost".to_string());
    format!("http://{}{}", host, path)
}
fn cache_repo_json(store: &mut Store, key: &str) -> String {
    let repo = store.get_repo(&key.split('/').next().unwrap().to_string(), &key.split('/').nth(1).unwrap().to_string()).unwrap();
    if let Some(s) = &repo.json_cache { return s.clone(); }
    let s = repo_json(repo);
    let repo = store.get_repo_mut(&key.split('/').next().unwrap().to_string(), &key.split('/').nth(1).unwrap().to_string()).unwrap();
    repo.json_cache = Some(s.clone());
    s
}
fn cache_issue_json(store: &mut Store, key: &str, number: i64) -> String {
    let (o, n) = (key.split('/').next().unwrap().to_string(), key.split('/').nth(1).unwrap().to_string());
    let repo = store.get_repo(&o, &n).unwrap();
    let issue = repo.issues.get(&number).unwrap();
    if let Some(s) = &issue.json_cache { return s.clone(); }
    let s = issue_json(repo, issue);
    let repo = store.get_repo_mut(&o, &n).unwrap();
    repo.issues.get_mut(&number).unwrap().json_cache = Some(s.clone());
    s
}

fn handle_rest(req: &Request, store: &mut Store) -> Response {
    let method = req.method.as_str();
    let segs: Vec<String> = req.path.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if !auth_ok(req) { return unauthorized(); }
    let rest: Vec<String> = segs.iter().skip(2).cloned().collect();
    if rest.is_empty() {
        if method != "GET" { return Response::json_value(405, &json!({"message": "Method Not Allowed"}), &[]); }
        return Response::json_value(200, &json!({"current_user_url": "/api/v3/user", "rate_limit_url": "/api/v3/rate_limit"}), &[("X-OAuth-Scopes", "repo, read:org")]);
    }
    if rest.len() == 1 && rest[0] == "user" {
        return Response::json_value(200, &json!({"id": VIEWER_ID, "login": VIEWER_LOGIN, "name": VIEWER_NAME, "email": VIEWER_EMAIL, "type": "User"}), &[]);
    }
    if rest.len() == 1 && rest[0] == "meta" {
        return Response::json_value(200, &json!({"verifiable_password_authentication": true, "installed_version": "1.0.0", "git_protocol": true}), &[]);
    }
    if rest.len() == 1 && rest[0] == "rate_limit" {
        return Response::json_value(200, &json!({"resources": {"core": {"limit": 5000, "remaining": 5000, "reset": 1700000000}}, "rate": {"limit": 5000, "remaining": 5000, "reset": 1700000000}}), &[]);
    }
    if rest.len() == 2 && rest[0] == "user" && rest[1] == "repos" && method == "POST" {
        let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
        let name = get_s(&b, "name");
        if name.is_empty() { return unprocessable("name is required"); }
        let desc = get_s(&b, "description");
        let priv_ = get_bool(&b, "private");
        match store.create_repo(&name, desc, priv_) {
            Err(_) => return unprocessable("Repository already exists"),
            Ok(_) => {
                let key = Store::key(VIEWER_LOGIN, &name);
                let s = cache_repo_json(store, &key);
                return Response::json(201, &s, &[]);
            }
        }
    }
    if rest.len() == 2 && rest[0] == "user" && rest[1] == "repos" && method == "GET" {
        let keys: Vec<String> = store.repo_order.clone();
        let link = paginate_link(keys.len(), &req.query, &abs_url(req, "/api/v3/user/repos"));
        let page = paginate_slice(&keys, &req.query);
        let items: Vec<String> = page.iter().map(|k| cache_repo_json(store, k)).collect();
        let body = format!("[{}]", items.join(","));
        return Response::json(200, &body, &[("Link", &link)]);
    }
    if rest.first().map(|s| s.as_str()) == Some("repos") && rest.len() >= 3 {
        let owner = rest[1].clone(); let name = rest[2].clone();
        let key = Store::key(&owner, &name);
        if store.get_repo(&owner, &name).is_none() { return not_found(); }
        let tail: Vec<String> = rest.iter().skip(3).cloned().collect();
        if tail.is_empty() {
            if method == "GET" { let s = cache_repo_json(store, &key); return Response::json(200, &s, &[]); }
            if method == "DELETE" { store.delete_repo(&owner, &name); return Response::empty(204); }
            if method == "PATCH" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                if let Some(d) = b.get("description").and_then(|v| v.as_str()) {
                    let repo = store.get_repo_mut(&owner, &name).unwrap();
                    repo.description = d.to_string(); repo.json_cache = None;
                }
                let s = cache_repo_json(store, &key);
                return Response::json(200, &s, &[]);
            }
            return Response::json_value(405, &json!({"message": "Method Not Allowed"}), &[]);
        }
        if tail.len() == 1 && tail[0] == "branches" && method == "GET" {
            let repo = store.get_repo(&owner, &name).unwrap().clone();
            let mut out = Vec::new();
            for (r, oid) in &repo.refs {
                if let Some(b) = r.strip_prefix("refs/heads/") { out.push(json!({"name": b, "commit": {"sha": oid, "url": ""}, "protected": false})); }
            }
            return Response::json_value(200, &Value::Array(out), &[]);
        }
        if tail.len() == 1 && tail[0] == "commits" && method == "GET" {
            let repo = store.get_repo(&owner, &name).unwrap().clone();
            let tip = match req.query.get("sha") {
                Some(sha) => Store::resolve_ref(&repo, sha),
                None => repo.ref_tip(&format!("refs/heads/{}", repo.default_branch)),
            };
            let commit_oid = tip.and_then(|t| Store::peel_to_commit(&repo, &t));
            let hist = commit_oid.map(|c| Store::history(&repo, &c, 100)).unwrap_or_default();
            let page = paginate_slice(&hist, &req.query);
            let out: Vec<Value> = page.iter().map(|oid| {
                let parsed = repo.objects.get(oid).map(|o| parse_commit(&o.data));
                json!({"sha": oid, "commit": {"message": parsed.as_ref().map(|p| p.message.trim_end_matches('\n').to_string()).unwrap_or_default()}, "author": {"login": VIEWER_LOGIN}, "committer": {"login": VIEWER_LOGIN}})
            }).collect();
            return Response::json_value(200, &Value::Array(out), &[]);
        }
        if tail.len() == 2 && tail[0] == "git" && tail[1] == "refs" && method == "POST" {
            let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
            let r = get_s(&b, "ref");
            let sha = get_s(&b, "sha");
            if !r.starts_with("refs/") { return unprocessable("ref must start with refs/"); }
            let repo = store.get_repo(&owner, &name).unwrap();
            if !repo.objects.contains_key(&sha) { return unprocessable("sha does not exist"); }
            let repo = store.get_repo_mut(&owner, &name).unwrap();
            repo.set_ref(&r, sha.clone());
            repo.pack_version += 1;
            return Response::json_value(201, &json!({"ref": r, "object": {"sha": sha, "type": "commit", "url": ""}}), &[]);
        }
        if tail.first().map(|s| s.as_str()) == Some("contents") {
            let path = percent_decode(&tail[1..].join("/"));
            let branch_of = |body: Option<&Map<String, Value>>| -> String {
                if let Some(b) = req.query.get("branch").or_else(|| req.query.get("ref")) { return b.clone(); }
                if let Some(b) = body { if let Some(s) = b.get("branch").and_then(|v| v.as_str()) { return s.to_string(); } }
                store.get_repo(&owner, &name).unwrap().default_branch.clone()
            };
            if method == "GET" {
                let branch = branch_of(None);
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                let tip = match Store::branch_tip(&repo, &branch) { Some(t) => t, None => return not_found() };
                let tree = Store::tree_of(&repo, &tip).unwrap();
                let flat = flatten_tree(&repo.objects, &tree);
                if path.is_empty() {
                    let mut out = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    for (p, e) in &flat {
                        let seg = p.split('/').next().unwrap();
                        if !seen.insert(seg.to_string()) { continue; }
                        let is_dir = flat.keys().any(|k| k.starts_with(&format!("{}/", seg)));
                        out.push(json!({"type": if is_dir { "dir" } else { "file" }, "name": seg, "path": seg, "sha": e.0, "size": 0}));
                    }
                    return Response::json_value(200, &Value::Array(out), &[]);
                }
                if let Some(entry) = flat.get(&path) {
                    let file = Store::read_path(&repo, &tree, &path);
                    match file {
                        Some((oid, data)) => {
                            let _ = entry;
                            return Response::json_value(200, &json!({"type": "file", "encoding": "base64", "size": data.len(), "name": path.split('/').last().unwrap_or(""), "path": path, "sha": oid, "content": base64_encode(&data)}), &[]);
                        }
                        None => return not_found(),
                    }
                }
                let children: Vec<Value> = flat.iter().filter(|(p, _)| p.starts_with(&format!("{}/", path)) && !p[path.len() + 1..].contains('/')).map(|(p, e)| json!({"type": "file", "name": &p[path.len() + 1..], "path": p, "sha": e.0, "size": 0})).collect();
                if !children.is_empty() { return Response::json_value(200, &Value::Array(children), &[]); }
                return not_found();
            }
            if method == "PUT" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let put_branch = branch_of(Some(&b));
                let content = b.get("content").and_then(|v| v.as_str()).map(|s| base64_decode(s).unwrap_or_default()).unwrap_or_default();
                let message = b.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let message = if message.is_empty() { format!("Update {}", path) } else { message };
                let sha = b.get("sha").and_then(|v| v.as_str()).map(|s| s.to_string());
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                let tip = Store::branch_tip(&repo, &put_branch);
                let existing = tip.and_then(|t| Store::tree_of(&repo, &t)).and_then(|tr| Store::read_path(&repo, &tr, &path));
                if let Some(s) = &sha {
                    if existing.as_ref().map(|(o, _)| o) != Some(s) { return Response::json_value(409, &json!({"message": "sha does not match"}), &[]); }
                }
                let is_update = existing.is_some();
                let (commit_sha, content_sha) = store.commit_file(&key, &put_branch, &path, content.clone(), &message);
                return Response::json_value(if is_update { 200 } else { 201 }, &json!({"content": {"name": path.split('/').last().unwrap_or(""), "path": path, "sha": content_sha, "size": content.len(), "type": "file", "encoding": "base64"}, "commit": {"sha": commit_sha, "message": message}}), &[]);
            }
            if method == "DELETE" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let del_branch = branch_of(Some(&b));
                let message = b.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let message = if message.is_empty() { format!("Delete {}", path) } else { message };
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                if Store::branch_tip(&repo, &del_branch).is_none() { return not_found(); }
                match store.delete_file(&key, &del_branch, &path, &message) {
                    None => return not_found(),
                    Some(commit) => return Response::json_value(200, &json!({"commit": {"sha": commit, "message": message}}), &[]),
                }
            }
            return Response::json_value(405, &json!({"message": "Method Not Allowed"}), &[]);
        }
        if tail.len() == 1 && tail[0] == "labels" {
            if method == "POST" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let label_name = get_s(&b, "name");
                if label_name.is_empty() { return unprocessable("name is required"); }
                let color = b.get("color").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| label_color(&label_name));
                let desc = get_s(&b, "description");
                let repo = match store.get_repo_mut(&owner, &name) { Some(r) => r, None => return not_found() };
                repo.labels.insert(label_name.clone(), Label { name: label_name.clone(), color: color.clone(), description: desc.clone() });
                return Response::json_value(201, &json!({"name": label_name, "color": color, "description": desc}), &[]);
            }
            if method == "GET" {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                let out: Vec<Value> = repo.labels.values().map(|l| json!({"name": l.name, "color": l.color, "description": l.description})).collect();
                return Response::json_value(200, &Value::Array(out), &[]);
            }
        }
        if tail.first().map(|s| s.as_str()) == Some("issues") {
            if tail.len() == 1 && method == "POST" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let title = get_s(&b, "title");
                if title.is_empty() { return unprocessable("title is required"); }
                let labels: Vec<String> = b.get("labels").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                let body = get_s(&b, "body");
                match store.create_issue(&key, title, body, &labels) {
                    Err(e) => return unprocessable(&e),
                    Ok(num) => { let s = cache_issue_json(store, &key, num); return Response::json(201, &s, &[]); }
                }
            }
            if tail.len() == 1 && method == "GET" {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                let mut list: Vec<Issue> = repo.issues.values().cloned().collect();
                if let Some(state) = req.query.get("state") {
                    if state != "all" { list.retain(|i| &i.state == state); }
                }
                let dir = req.query.get("direction").map(|s| s.as_str()).unwrap_or("desc");
                let d: i64 = if dir == "asc" { 1 } else { -1 };
                list.sort_by(|a, b| if d == 1 { a.number.cmp(&b.number) } else { b.number.cmp(&a.number) });
                let link = paginate_link(list.len(), &req.query, &abs_url(req, &req.path));
                let page = paginate_slice(&list, &req.query);
                let items: Vec<String> = page.iter().map(|i| cache_issue_json(store, &key, i.number)).collect();
                let body = format!("[{}]", items.join(","));
                return Response::json(200, &body, &[("Link", &link)]);
            }
            let num: i64 = tail.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
            if tail.len() == 2 {
                if store.get_repo(&owner, &name).unwrap().issues.get(&num).is_none() { return not_found(); }
                if method == "GET" { let s = cache_issue_json(store, &key, num); return Response::json(200, &s, &[]); }
                if method == "PATCH" {
                    let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                    let ts = store.next_ts();
                    let updated = iso_of(ts);
                    let repo = store.get_repo_mut(&owner, &name).unwrap();
                    let issue = repo.issues.get_mut(&num).unwrap();
                    if let Some(s) = b.get("state").and_then(|v| v.as_str()) { issue.state = if s == "closed" { "closed".to_string() } else { "open".to_string() }; }
                    if let Some(t) = b.get("title").and_then(|v| v.as_str()) { issue.title = t.to_string(); }
                    if let Some(bd) = b.get("body").and_then(|v| v.as_str()) { issue.body = bd.to_string(); }
                    issue.updated_at = updated;
                    issue.json_cache = None;
                    let s = cache_issue_json(store, &key, num);
                    return Response::json(200, &s, &[]);
                }
            }
            if tail.len() == 3 && tail[2] == "comments" {
                if store.get_repo(&owner, &name).unwrap().issues.get(&num).is_none() { return not_found(); }
                if method == "POST" {
                    let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                    let body = get_s(&b, "body");
                    if body.is_empty() { return unprocessable("body is required"); }
                    let repo = store.get_repo_mut(&owner, &name).unwrap();
                    let issue = repo.issues.get_mut(&num).unwrap();
                    let cid = repo.next_comment_id; repo.next_comment_id += 1;
                    let c = Comment { id: cid, body: body.clone(), user: VIEWER_LOGIN.to_string(), created_at: iso_of(EPOCH + 200000 + cid), gid: gid("Comment", &format!("{}#{}", format!("{}/{}", owner, name), format!("{}/c{}", num, cid))) };
                    issue.comments.push(c.clone());
                    let repo_c = store.get_repo(&owner, &name).unwrap().clone();
                    let issue_c = repo_c.issues.get(&num).unwrap().clone();
                    return Response::json_value(201, &comment_json(&repo_c, &issue_c, &c), &[]);
                }
                if method == "GET" {
                    let repo = store.get_repo(&owner, &name).unwrap().clone();
                    let issue = repo.issues.get(&num).unwrap().clone();
                    let out: Vec<Value> = issue.comments.iter().map(|c| comment_json(&repo, &issue, c)).collect();
                    return Response::json_value(200, &Value::Array(out), &[]);
                }
            }
        }
        if tail.first().map(|s| s.as_str()) == Some("pulls") {
            if tail.len() == 1 && method == "POST" {
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let title = get_s(&b, "title");
                let head = get_s(&b, "head");
                let base = b.get("base").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let base = if base.is_empty() { store.get_repo(&owner, &name).unwrap().default_branch.clone() } else { base };
                if title.is_empty() || head.is_empty() { return unprocessable("title and head are required"); }
                let body = get_s(&b, "body");
                match store.create_pull(&key, title, body, &head, &base) {
                    Err(e) => return unprocessable(&e),
                    Ok(num) => {
                        let repo = store.get_repo(&owner, &name).unwrap().clone();
                        let pr = repo.pulls.get(&num).unwrap().clone();
                        return Response::json_value(201, &pull_json(store, &repo, &pr), &[]);
                    }
                }
            }
            if tail.len() == 1 && method == "GET" {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                let mut list: Vec<Pull> = repo.pulls.values().cloned().collect();
                let state = req.query.get("state").map(|s| s.as_str()).unwrap_or("open");
                if state != "all" { list.retain(|p| (if p.merged { "closed" } else { &p.state }) == state); }
                list.sort_by(|a, b| b.number.cmp(&a.number));
                let link = paginate_link(list.len(), &req.query, &abs_url(req, &req.path));
                let page = paginate_slice(&list, &req.query);
                let out: Vec<Value> = page.iter().map(|p| pull_json(store, &repo, p)).collect();
                return Response::json_value(200, &Value::Array(out), &[("Link", &link)]);
            }
            let num: i64 = tail.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
            if tail.len() == 2 {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                if repo.pulls.get(&num).is_none() { return not_found(); }
                if method == "GET" {
                    let pr = repo.pulls.get(&num).unwrap().clone();
                    return Response::json_value(200, &pull_json(store, &repo, &pr), &[]);
                }
            }
            if tail.len() == 3 && tail[2] == "files" && method == "GET" {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                if repo.pulls.get(&num).is_none() { return not_found(); }
                let pr = repo.pulls.get(&num).unwrap().clone();
                let files = Store::pull_files(&repo, &pr);
                let out: Vec<Value> = files.iter().map(|(filename, status, a, d)| json!({"filename": filename, "status": status, "additions": a, "deletions": d, "changes": a + d})).collect();
                return Response::json_value(200, &Value::Array(out), &[]);
            }
            if tail.len() == 3 && tail[2] == "merge" && method == "PUT" {
                let repo = store.get_repo(&owner, &name).unwrap().clone();
                if repo.pulls.get(&num).is_none() { return not_found(); }
                let b = match read_body_json(req) { Ok(b) => b, Err(r) => return r };
                let method_ = b.get("merge_method").and_then(|v| v.as_str()).unwrap_or("squash").to_string();
                match store.merge_pull(&key, num, &method_) {
                    Err((e, status)) => return Response::json_value(status as u16, &json!({"message": e}), &[]),
                    Ok((sha, message)) => return Response::json_value(200, &json!({"sha": sha, "merged": true, "message": message}), &[]),
                }
            }
        }
        return not_found();
    }
    not_found()
}

// ---------------------------------------------------------------- GraphQL
fn handle_graphql(req: &Request, store: &mut Store) -> Response {
    if !auth_ok(req) { return unauthorized(); }
    let parsed: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => return bad_request("Invalid JSON"),
    };
    let run_one = |op: &Value, store: &mut Store| -> Value {
        let query = op.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let vars: HashMap<String, Value> = op.get("variables").and_then(|v| v.as_object()).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        execute_graphql(store, query, vars)
    };
    if let Value::Array(arr) = parsed {
        let out: Vec<Value> = arr.iter().map(|op| run_one(op, store)).collect();
        return Response::json_value(200, &Value::Array(out), &[]);
    }
    let out = run_one(&parsed, store);
    Response::json_value(200, &out, &[])
}

// ---------------------------------------------------------------- MCP
const MCP_TOOLS_JSON: &str = r#"[{"name":"create_repo","description":"Create a new repository for the authenticated user.","inputSchema":{"type":"object","properties":{"name":{"type":"string","description":"Repository name"},"description":{"type":"string","description":"Repository description"},"private":{"type":"boolean","description":"Whether the repository is private"}},"required":["name"]}},{"name":"get_repo","description":"Get a repository by owner and name.","inputSchema":{"type":"object","properties":{"owner":{"type":"string","description":"Repository owner login"},"repo":{"type":"string","description":"Repository name"}},"required":["owner","repo"]}},{"name":"create_issue","description":"Create a new issue in a repository.","inputSchema":{"type":"object","properties":{"owner":{"type":"string","description":"Repository owner login"},"repo":{"type":"string","description":"Repository name"},"title":{"type":"string","description":"Issue title"},"body":{"type":"string","description":"Issue body"}},"required":["owner","repo","title"]}},{"name":"list_issues","description":"List issues in a repository.","inputSchema":{"type":"object","properties":{"owner":{"type":"string","description":"Repository owner login"},"repo":{"type":"string","description":"Repository name"},"state":{"type":"string","description":"Issue state filter: open, closed, or all"}},"required":["owner","repo"]}},{"name":"create_pull_request","description":"Create a new pull request in a repository.","inputSchema":{"type":"object","properties":{"owner":{"type":"string","description":"Repository owner login"},"repo":{"type":"string","description":"Repository name"},"title":{"type":"string","description":"Pull request title"},"body":{"type":"string","description":"Pull request body"},"head":{"type":"string","description":"Head branch name"},"base":{"type":"string","description":"Base branch name"}},"required":["owner","repo","title","head","base"]}},{"name":"merge_pull_request","description":"Merge a pull request in a repository.","inputSchema":{"type":"object","properties":{"owner":{"type":"string","description":"Repository owner login"},"repo":{"type":"string","description":"Repository name"},"pull_number":{"type":"integer","description":"Pull request number"}},"required":["owner","repo","pull_number"]}}]"#;
const MCP_VERSION: &str = "2026-07-28";
fn mcp_result(id: &Value, result: Value) -> String { json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string() }
fn mcp_error(id: &Value, code: i64, message: &str) -> String { json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string() }
fn pick_arg<'a>(args: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    for n in names { if let Some(v) = args.get(*n) { return Some(v); } }
    None
}
fn split_full_name(args: &Map<String, Value>) -> (Option<String>, Option<String>) {
    let mut owner = pick_arg(args, &["owner", "owner_login", "org"]).and_then(|v| v.as_str()).map(|s| s.to_string());
    let mut repo = pick_arg(args, &["repo", "repository"]).and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(full) = pick_arg(args, &["full_name", "fullName"]).and_then(|v| v.as_str()) {
        if (owner.is_none() || repo.is_none()) && full.contains('/') {
            let mut p = full.splitn(2, '/');
            if owner.is_none() { owner = p.next().map(|s| s.to_string()); }
            if repo.is_none() { repo = p.next().map(|s| s.to_string()); }
        }
    }
    (owner, repo)
}
fn mcp_text(id: &Value, text: &str, structured: Option<Value>) -> Response {
    let mut result = Map::new();
    result.insert("content".to_string(), json!([{"type": "text", "text": text}]));
    if let Some(s) = structured { result.insert("structuredContent".to_string(), s); }
    let body = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    Response { status: 200, headers: vec![("Content-Type".to_string(), "application/json".to_string()), ("Content-Length".to_string(), body.len().to_string())], body: body.into_bytes() }
}
fn mcp_err_text(id: &Value, text: &str) -> Response {
    let body = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": true}}).to_string();
    Response { status: 200, headers: vec![("Content-Type".to_string(), "application/json".to_string()), ("Content-Length".to_string(), body.len().to_string())], body: body.into_bytes() }
}
fn mcp_json(id: &Value, body_str: &str) -> Response {
    Response { status: 200, headers: vec![("Content-Type".to_string(), "application/json".to_string()), ("Content-Length".to_string(), body_str.len().to_string())], body: body_str.as_bytes().to_vec() }
}

fn handle_mcp(req: &Request, store: &mut Store) -> Response {
    if req.method == "DELETE" { return Response::empty(202); }
    if req.method == "GET" { return Response::json_value(405, &json!({"message": "Method Not Allowed"}), &[]); }
    if !auth_ok(req) { return unauthorized(); }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => {
            let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}).to_string();
            return Response::json(400, &body, &[]);
        }
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = msg.get("id").is_none() || msg.get("id").map(|v| v.is_null()).unwrap_or(false);
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let params = msg.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    if method == "server/discover" {
        let result = json!({"protocolVersion": MCP_VERSION, "supportedVersions": ["2026-07-28", "2025-06-18", "2025-03-26", "2024-11-05"], "serverInfo": {"name": "forge", "version": "1.0.0"}, "capabilities": {"tools": {}}, "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "forge", "version": "1.0.0"}}});
        return Response::json(200, &mcp_result(&id, result), &[]);
    }
    if method == "initialize" {
        let result = json!({"protocolVersion": MCP_VERSION, "serverInfo": {"name": "forge", "version": "1.0.0"}, "capabilities": {"tools": {}}, "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "forge", "version": "1.0.0"}}});
        return Response::json(200, &mcp_result(&id, result), &[]);
    }
    if method.starts_with("notifications/") { return Response::empty(202); }
    if method == "tools/list" {
        let tools: Value = serde_json::from_str(MCP_TOOLS_JSON).unwrap();
        let body = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}}).to_string();
        return Response::json(200, &body, &[]);
    }
    if method == "tools/call" {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let args = params.get("arguments").and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let tools: Vec<Value> = serde_json::from_str(MCP_TOOLS_JSON).unwrap();
        let tool = tools.iter().find(|t| {
            let tn = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
            tn == name || tn.ends_with(&format!("__{}", name)) || name.ends_with(tn)
        }).cloned();
        let tool = match tool { Some(t) => t, None => return mcp_json(&id, &mcp_error(&id, -32601, &format!("Unknown tool: {}", name))) };
        let tname = tool.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let required: Vec<String> = tool.get("inputSchema").and_then(|v| v.get("required")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
        let mut missing: Vec<String> = Vec::new();
        for r in &required {
            let present = match r.as_str() {
                "owner" => split_full_name(&args).0.is_some(),
                "repo" => split_full_name(&args).1.is_some(),
                "pull_number" => pick_arg(&args, &["pull_number", "number", "pullNumber"]).is_some(),
                _ => pick_arg(&args, &[r.as_str()]).is_some() || pick_arg(&args, &[&r.replace('_', "")]).is_some(),
            };
            if !present { missing.push(r.clone()); }
        }
        if !missing.is_empty() { return mcp_json(&id, &mcp_error(&id, -32602, &format!("Missing required arguments: {}", missing.join(", ")))); }
        match tname.as_str() {
            "create_repo" => {
                let name_arg = pick_arg(&args, &["name"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let desc = pick_arg(&args, &["description"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let priv_ = pick_arg(&args, &["private"]).and_then(|v| v.as_bool()).unwrap_or(false);
                match store.create_repo(&name_arg, desc, priv_) {
                    Err(e) => return mcp_err_text(&id, &e),
                    Ok(_) => {
                        let key = Store::key(VIEWER_LOGIN, &name_arg);
                        let s = cache_repo_json(store, &key);
                        let obj: Value = serde_json::from_str(&s).unwrap();
                        return mcp_text(&id, &s, Some(obj));
                    }
                }
            }
            "get_repo" => {
                let (owner, repo) = split_full_name(&args);
                let r = store.get_repo(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or(""));
                match r {
                    None => return mcp_err_text(&id, "Not Found"),
                    Some(_) => {
                        let key = Store::key(owner.as_deref().unwrap(), repo.as_deref().unwrap());
                        let s = cache_repo_json(store, &key);
                        let obj: Value = serde_json::from_str(&s).unwrap();
                        return mcp_text(&id, &s, Some(obj));
                    }
                }
            }
            "create_issue" => {
                let (owner, repo) = split_full_name(&args);
                let key = Store::key(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or(""));
                if store.get_repo(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or("")).is_none() { return mcp_err_text(&id, "Not Found"); }
                let title = pick_arg(&args, &["title"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body = pick_arg(&args, &["body"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                match store.create_issue(&key, title, body, &[]) {
                    Err(e) => return mcp_err_text(&id, &e),
                    Ok(num) => {
                        let s = cache_issue_json(store, &key, num);
                        let obj: Value = serde_json::from_str(&s).unwrap();
                        return mcp_text(&id, &s, Some(obj));
                    }
                }
            }
            "list_issues" => {
                let (owner, repo) = split_full_name(&args);
                let repo_c = match store.get_repo(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or("")) { Some(r) => r.clone(), None => return mcp_err_text(&id, "Not Found") };
                let state = pick_arg(&args, &["state"]).and_then(|v| v.as_str()).unwrap_or("open").to_string();
                let mut list: Vec<Issue> = repo_c.issues.values().cloned().collect();
                if state != "all" { list.retain(|i| i.state == state); }
                let key = Store::key(owner.as_deref().unwrap(), repo.as_deref().unwrap());
                let arr: Vec<Value> = list.iter().map(|i| serde_json::from_str(&cache_issue_json(store, &key, i.number)).unwrap()).collect();
                let text = serde_json::to_string(&arr).unwrap();
                return mcp_text(&id, &text, Some(Value::Array(arr)));
            }
            "create_pull_request" => {
                let (owner, repo) = split_full_name(&args);
                let key = Store::key(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or(""));
                if store.get_repo(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or("")).is_none() { return mcp_err_text(&id, "Not Found"); }
                let head = pick_arg(&args, &["head", "head_branch"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let base = pick_arg(&args, &["base", "base_branch"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let title = pick_arg(&args, &["title"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body = pick_arg(&args, &["body"]).and_then(|v| v.as_str()).unwrap_or("").to_string();
                match store.create_pull(&key, title, body, &head, &base) {
                    Err(e) => return mcp_err_text(&id, &e),
                    Ok(num) => {
                        let repo_c = store.get_repo(owner.as_deref().unwrap(), repo.as_deref().unwrap()).unwrap().clone();
                        let pr = repo_c.pulls.get(&num).unwrap().clone();
                        let obj = pull_json(store, &repo_c, &pr);
                        let text = serde_json::to_string(&obj).unwrap();
                        return mcp_text(&id, &text, Some(obj));
                    }
                }
            }
            "merge_pull_request" => {
                let (owner, repo) = split_full_name(&args);
                let key = Store::key(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or(""));
                let num = pick_arg(&args, &["pull_number", "number", "pullNumber"]).and_then(|v| v.as_i64()).unwrap_or(-1);
                let exists = store.get_repo(owner.as_deref().unwrap_or(""), repo.as_deref().unwrap_or("")).and_then(|r| r.pulls.get(&num)).is_some();
                if !exists { return mcp_err_text(&id, "Not Found"); }
                match store.merge_pull(&key, num, "squash") {
                    Err((e, _)) => return mcp_err_text(&id, &e),
                    Ok((sha, message)) => {
                        let obj = json!({"sha": sha, "merged": true, "message": message});
                        let text = serde_json::to_string(&obj).unwrap();
                        return mcp_text(&id, &text, Some(obj));
                    }
                }
            }
            _ => return mcp_json(&id, &mcp_error(&id, -32601, &format!("Unknown tool: {}", name))),
        }
    }
    if is_notification { return Response::empty(202); }
    Response::json(200, &mcp_error(&id, -32601, &format!("Method not found: {}", method)), &[])
}

// ---------------------------------------------------------------- dispatch
pub fn dispatch(req: &Request, store: &mut Store) -> Response {
    let method = req.method.as_str();
    let path = req.path.as_str();
    if path == "/healthz" {
        return Response::json_value(200, &json!({"ok": true}), &[]);
    }
    if path == "/api/graphql" || path == "/graphql" {
        if method != "POST" { return Response::json_value(405, &json!({"message": "Method Not Allowed"}), &[]); }
        return handle_graphql(req, store);
    }
    if path == "/mcp" { return handle_mcp(req, store); }
    if path.starts_with("/api/v3") { return handle_rest(req, store); }
    // git smart HTTP: /{owner}/{repo}.git/{info/refs|git-upload-pack|git-receive-pack}
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 3 && parts[1].ends_with(".git") {
        let owner = parts[0].to_string();
        let repo_name = parts[1][..parts[1].len() - 4].to_string();
        let what: &str = if parts.len() == 4 && parts[2] == "info" && parts[3] == "refs" { "info/refs" } else if parts.len() == 3 { parts[2] } else { return not_found(); };
        if store.get_repo(&owner, &repo_name).is_none() { return not_found(); }
        let key = Store::key(&owner, &repo_name);
        if what == "info/refs" {
            let service = req.query.get("service").cloned().unwrap_or_default();
            if service == "git-upload-pack" || service == "git-receive-pack" {
                let repo = store.get_repo(&owner, &repo_name).unwrap().clone();
                return handle_info_refs(req, &repo, &service);
            }
            return not_found();
        }
        if what == "git-upload-pack" && method == "POST" { return handle_upload_pack(req, store, &key); }
        if what == "git-receive-pack" && method == "POST" { return handle_receive_pack(req, store, &key); }
        return not_found();
    }
    not_found()
}
