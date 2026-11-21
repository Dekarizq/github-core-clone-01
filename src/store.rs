// In-memory forge store — port of the Node entry (store.ts).
use crate::git::*;
use std::collections::{BTreeMap, HashMap};

pub const VIEWER_ID: i64 = 1000;
pub const VIEWER_LOGIN: &str = "adaiversen";
pub const VIEWER_NAME: &str = "ada iversen";
pub const VIEWER_EMAIL: &str = "adaiversen@example.invalid";
pub const IDENTITY: &str = "ada iversen <adaiversen@example.invalid>";
pub const EPOCH: i64 = 1700000000;

pub fn iso_of(ts: i64) -> String {
    // civil-from-days (Howard Hinnant), UTC
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[derive(Clone, Debug)]
pub struct Label { pub name: String, pub color: String, pub description: String }
#[derive(Clone, Debug)]
pub struct Comment { pub id: i64, pub body: String, pub user: String, pub created_at: String, pub gid: String }
#[derive(Clone, Debug)]
pub struct Issue {
    pub id: i64, pub number: i64, pub title: String, pub body: String, pub state: String,
    pub labels: Vec<Label>, pub comments: Vec<Comment>, pub created_at: String, pub updated_at: String,
    pub json_cache: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Pull {
    pub id: i64, pub number: i64, pub title: String, pub body: String, pub state: String,
    pub head_ref: String, pub base_ref: String, pub head_sha: String, pub base_sha: String,
    pub merged: bool, pub merge_commit_sha: Option<String>, pub merged_at: Option<String>,
    pub created_at: String,
}
#[derive(Clone, Debug)]
pub struct Repo {
    pub id: i64, pub name: String, pub owner: String, pub description: String, pub priv_: bool,
    pub default_branch: String, pub created_at: String,
    pub objects: HashMap<String, GitObj>,
    pub refs: Vec<(String, String)>, // insertion-ordered (name, oid)
    pub labels: HashMap<String, Label>,
    pub issues: BTreeMap<i64, Issue>,
    pub pulls: BTreeMap<i64, Pull>,
    pub next_number: i64,
    pub next_comment_id: i64,
    pub pack_version: i64,
    pub json_cache: Option<String>,
}
impl Repo {
    pub fn ref_tip(&self, name: &str) -> Option<String> {
        self.refs.iter().find(|(n, _)| n == name).map(|(_, o)| o.clone())
    }
    pub fn set_ref(&mut self, name: &str, oid: String) {
        if let Some(e) = self.refs.iter_mut().find(|(n, _)| n == name) { e.1 = oid; }
        else { self.refs.push((name.to_string(), oid)); }
    }
    pub fn del_ref(&mut self, name: &str) { self.refs.retain(|(n, _)| n != name); }
}

pub struct Store {
    pub pack_cache: HashMap<String, Vec<u8>>,
    pub fetch_cache: HashMap<String, Vec<u8>>,
    pub repos: HashMap<String, Repo>,
    pub repo_order: Vec<String>, // keys owner/name
    pub next_repo_id: i64,
    pub next_issue_id: i64,
    pub seq: i64,
}
impl Store {
    pub fn new() -> Self { Store { pack_cache: HashMap::new(), fetch_cache: HashMap::new(), repos: HashMap::new(), repo_order: Vec::new(), next_repo_id: 5000, next_issue_id: 20000, seq: 0 } }
    pub fn next_ts(&mut self) -> i64 { self.seq += 1; EPOCH + self.seq }
    pub fn key(owner: &str, name: &str) -> String { format!("{}/{}", owner, name) }
    pub fn get_repo(&self, owner: &str, name: &str) -> Option<&Repo> { self.repos.get(&Self::key(owner, name)) }
    pub fn get_repo_mut(&mut self, owner: &str, name: &str) -> Option<&mut Repo> { self.repos.get_mut(&Self::key(owner, name)) }
    pub fn create_repo(&mut self, name: &str, description: String, priv_: bool) -> Result<i64, String> {
        if self.repos.contains_key(&Self::key(VIEWER_LOGIN, name)) { return Err("Repository already exists".into()); }
        let id = self.next_repo_id; self.next_repo_id += 1;
        let repo = Repo {
            id, name: name.to_string(), owner: VIEWER_LOGIN.to_string(), description, priv_,
            default_branch: "main".to_string(), created_at: iso_of(EPOCH + id),
            objects: HashMap::new(), refs: Vec::new(), labels: HashMap::new(),
            issues: BTreeMap::new(), pulls: BTreeMap::new(), next_number: 1, next_comment_id: 1,
            pack_version: 0, json_cache: None,
        };
        let k = Self::key(VIEWER_LOGIN, name);
        self.repos.insert(k.clone(), repo);
        self.repo_order.push(k);
        Ok(id)
    }
    pub fn delete_repo(&mut self, owner: &str, name: &str) {
        let k = Self::key(owner, name);
        self.repos.remove(&k);
        self.repo_order.retain(|x| *x != k);
    }
    pub fn branch_tip(repo: &Repo, branch: &str) -> Option<String> { repo.ref_tip(&format!("refs/heads/{}", branch)) }
    pub fn resolve_ref(repo: &Repo, refish: &str) -> Option<String> {
        if is_oid(refish) && repo.objects.contains_key(refish) { return Some(refish.to_string()); }
        if let Some(o) = repo.ref_tip(refish) { return Some(o); }
        if let Some(o) = repo.ref_tip(&format!("refs/heads/{}", refish)) { return Some(o); }
        if let Some(o) = repo.ref_tip(&format!("refs/tags/{}", refish)) { return Some(o); }
        None
    }
    pub fn peel_to_commit(repo: &Repo, oid: &str) -> Option<String> {
        let o = repo.objects.get(oid)?;
        if o.typ == ObjType::Commit { return Some(oid.to_string()); }
        if o.typ == ObjType::Tag { return Self::peel_to_commit(repo, &parse_tag(&o.data).object); }
        None
    }
    pub fn history(repo: &Repo, tip: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![tip.to_string()];
        while let Some(cur) = stack.pop() {
            if out.len() >= limit { break; }
            if !seen.insert(cur.clone()) { continue; }
            let o = match repo.objects.get(&cur) { Some(o) => o, None => continue };
            if o.typ != ObjType::Commit { continue; }
            out.push(cur.clone());
            let parents = parse_commit(&o.data).parents;
            for i in (0..parents.len()).rev() { stack.push(parents[i].clone()); }
        }
        out
    }
    pub fn tree_of(repo: &Repo, commit_oid: &str) -> Option<String> {
        let o = repo.objects.get(commit_oid)?;
        if o.typ == ObjType::Commit { Some(parse_commit(&o.data).tree) } else { None }
    }
    pub fn read_path(repo: &Repo, tree_oid: &str, path: &str) -> Option<(String, Vec<u8>)> {
        let flat = flatten_tree(&repo.objects, tree_oid);
        let (oid, _) = flat.get(path)?;
        let o = repo.objects.get(oid)?;
        if o.typ != ObjType::Blob { return None; }
        Some((oid.clone(), o.data.clone()))
    }
    pub fn write_path(repo: &mut Repo, base_tree: Option<&str>, path: &str, data: Option<Vec<u8>>) -> String {
        let mut flat: HashMap<String, (String, String)> = match base_tree { Some(t) => flatten_tree(&repo.objects, t), None => HashMap::new() };
        if let Some(d) = data {
            let blob_oid = put_obj(&mut repo.objects, ObjType::Blob, d);
            flat.insert(path.to_string(), (blob_oid, "100644".to_string()));
        } else { flat.remove(path); }
        // nested tree rebuild
        struct Node { files: HashMap<String, (String, String)>, dirs: HashMap<String, Node> }
        let mut root = Node { files: HashMap::new(), dirs: HashMap::new() };
        for (p, e) in &flat {
            let segs: Vec<&str> = p.split('/').collect();
            let mut node = &mut root;
            for i in 0..segs.len() - 1 {
                node = node.dirs.entry(segs[i].to_string()).or_insert(Node { files: HashMap::new(), dirs: HashMap::new() });
            }
            node.files.insert(segs[segs.len() - 1].to_string(), e.clone());
        }
        fn build(node: &Node, objects: &mut HashMap<String, GitObj>) -> String {
            let mut entries: Vec<TreeEntry> = Vec::new();
            for (name, e) in &node.files { entries.push(TreeEntry { mode: e.1.clone(), name: name.clone(), oid: e.0.clone() }); }
            for (name, child) in &node.dirs { entries.push(TreeEntry { mode: "40000".to_string(), name: name.clone(), oid: build(child, objects) }); }
            put_obj(objects, ObjType::Tree, build_tree(&entries))
        }
        build(&root, &mut repo.objects)
    }
    pub fn make_commit(repo: &mut Repo, tree: &str, parents: &[String], message: &str, ts: i64) -> String {
        let who = format!("{} {} +0000", IDENTITY, ts);
        let msg = if message.ends_with('\n') { message.to_string() } else { format!("{}\n", message) };
        let data = build_commit(tree, parents, &who, &who, &msg);
        put_obj(&mut repo.objects, ObjType::Commit, data)
    }
    pub fn commit_file(&mut self, repo_key: &str, branch: &str, path: &str, data: Vec<u8>, message: &str) -> (String, String) {
        let content_sha = hash_object(ObjType::Blob, &data);
        let ts = self.next_ts();
        let repo = self.repos.get_mut(repo_key).unwrap();
        let tip = Self::branch_tip(repo, branch);
        let tip = match tip {
            Some(t) => t,
            None => {
                let default_tip = Self::branch_tip(repo, "main");
                match default_tip {
                    None => {
                        let tree = Self::write_path(repo, None, path, Some(data));
                        let commit = Self::make_commit(repo, &tree, &[], message, ts);
                        repo.set_ref(&format!("refs/heads/{}", branch), commit.clone());
                        repo.pack_version += 1;
                        return (commit, content_sha);
                    }
                    Some(t) => { repo.set_ref(&format!("refs/heads/{}", branch), t.clone()); t }
                }
            }
        };
        let tree = Self::tree_of(repo, &tip).unwrap();
        let new_tree = Self::write_path(repo, Some(&tree), path, Some(data));
        let commit = Self::make_commit(repo, &new_tree, &[tip], message, ts);
        repo.set_ref(&format!("refs/heads/{}", branch), commit.clone());
        repo.pack_version += 1;
        (commit, content_sha)
    }
    pub fn delete_file(&mut self, repo_key: &str, branch: &str, path: &str, message: &str) -> Option<String> {
        let ts = self.next_ts();
        let repo = self.repos.get_mut(repo_key)?;
        let tip = Self::branch_tip(repo, branch)?;
        let tree = Self::tree_of(repo, &tip)?;
        if !flatten_tree(&repo.objects, &tree).contains_key(path) { return None; }
        let new_tree = Self::write_path(repo, Some(&tree), path, None);
        let commit = Self::make_commit(repo, &new_tree, &[tip], message, ts);
        repo.set_ref(&format!("refs/heads/{}", branch), commit.clone());
        repo.pack_version += 1;
        Some(commit)
    }
    pub fn create_issue(&mut self, repo_key: &str, title: String, body: String, label_names: &[String]) -> Result<i64, String> {
        let repo = self.repos.get_mut(repo_key).unwrap();
        let mut labels = Vec::new();
        for n in label_names {
            match repo.labels.get(n) { Some(l) => labels.push(l.clone()), None => return Err(format!("Label '{}' does not exist", n)) }
        }
        let number = repo.next_number; repo.next_number += 1;
        let id = self.next_issue_id; self.next_issue_id += 1;
        let created = iso_of(EPOCH + 100000 + id);
        let issue = Issue { id, number, title, body, state: "open".to_string(), labels, comments: Vec::new(), created_at: created.clone(), updated_at: created, json_cache: None };
        repo.issues.insert(number, issue);
        Ok(number)
    }
    pub fn create_pull(&mut self, repo_key: &str, title: String, body: String, head: &str, base: &str) -> Result<i64, String> {
        let repo = self.repos.get_mut(repo_key).unwrap();
        let head_tip = Self::branch_tip(repo, head);
        let base_tip = Self::branch_tip(repo, base);
        if head_tip.is_none() { return Err(format!("Head branch '{}' does not exist", head)); }
        if base_tip.is_none() { return Err(format!("Base branch '{}' does not exist", base)); }
        for p in repo.pulls.values() {
            if p.state == "open" && p.head_ref == head && p.base_ref == base { return Err("A pull request already exists for this head branch".into()); }
        }
        let number = repo.next_number; repo.next_number += 1;
        let id = self.next_issue_id; self.next_issue_id += 1;
        let pr = Pull { id, number, title, body, state: "open".to_string(), head_ref: head.to_string(), base_ref: base.to_string(), head_sha: head_tip.unwrap(), base_sha: base_tip.unwrap(), merged: false, merge_commit_sha: None, merged_at: None, created_at: iso_of(EPOCH + 100000 + id) };
        repo.pulls.insert(number, pr);
        Ok(number)
    }
    pub fn diff_trees(repo: &Repo, old_tree: Option<&str>, new_tree: Option<&str>) -> Vec<(String, String, Option<Vec<u8>>, Option<Vec<u8>>)> {
        let old_f = old_tree.map(|t| flatten_tree(&repo.objects, t)).unwrap_or_default();
        let new_f = new_tree.map(|t| flatten_tree(&repo.objects, t)).unwrap_or_default();
        let mut all: Vec<String> = old_f.keys().chain(new_f.keys()).cloned().collect();
        all.sort(); all.dedup();
        let mut out = Vec::new();
        for p in all {
            let o = old_f.get(&p); let n = new_f.get(&p);
            match (o, n) {
                (Some(_), None) => out.push((p.clone(), "removed".to_string(), repo.objects.get(&o.unwrap().0).map(|x| x.data.clone()), None)),
                (None, Some(_)) => out.push((p.clone(), "added".to_string(), None, repo.objects.get(&n.unwrap().0).map(|x| x.data.clone()))),
                (Some(oe), Some(ne)) if oe.0 != ne.0 => out.push((p.clone(), "modified".to_string(), repo.objects.get(&oe.0).map(|x| x.data.clone()), repo.objects.get(&ne.0).map(|x| x.data.clone()))),
                _ => {}
            }
        }
        out
    }
    pub fn pull_files(repo: &Repo, pr: &Pull) -> Vec<(String, String, i64, i64)> {
        let base_tip = Self::branch_tip(repo, &pr.base_ref).unwrap_or_else(|| pr.base_sha.clone());
        let head_tip = Self::branch_tip(repo, &pr.head_ref).unwrap_or_else(|| pr.head_sha.clone());
        let base_tree = Self::tree_of(repo, &Self::peel_to_commit(repo, &base_tip).unwrap_or(base_tip.clone()));
        let head_tree = Self::tree_of(repo, &Self::peel_to_commit(repo, &head_tip).unwrap_or(head_tip.clone()));
        let diff = Self::diff_trees(repo, base_tree.as_deref(), head_tree.as_deref());
        let mut out = Vec::new();
        for (p, status, old_d, new_d) in diff {
            let (a, d) = line_diff(old_d.as_deref(), new_d.as_deref());
            out.push((p, status, a, d));
        }
        out
    }
    pub fn pull_conflict(repo: &Repo, pr: &Pull) -> bool {
        let base_tip = match Self::branch_tip(repo, &pr.base_ref) { Some(t) => t, None => return true };
        let head_tip = match Self::branch_tip(repo, &pr.head_ref) { Some(t) => t, None => return true };
        let base_commit = match Self::peel_to_commit(repo, &base_tip) { Some(c) => c, None => return true };
        let head_commit = match Self::peel_to_commit(repo, &head_tip) { Some(c) => c, None => return true };
        let mb = match merge_base(&repo.objects, &base_commit, &head_commit) { Some(m) => m, None => return true };
        let mb_tree = Self::tree_of(repo, &mb);
        let base_tree = Self::tree_of(repo, &base_commit);
        let head_tree = Self::tree_of(repo, &head_commit);
        let d_base = Self::diff_trees(repo, mb_tree.as_deref(), base_tree.as_deref());
        let d_head = Self::diff_trees(repo, mb_tree.as_deref(), head_tree.as_deref());
        for (p, _, _, head_new) in &d_head {
            if let Some((_, _, _, base_new)) = d_base.iter().find(|(q, _, _, _)| q == p) {
                let h = head_new.as_ref().map(|d| hash_object(ObjType::Blob, d));
                let b = base_new.as_ref().map(|d| hash_object(ObjType::Blob, d));
                if h != b { return true; }
            }
        }
        false
    }
    pub fn merge_pull(&mut self, repo_key: &str, number: i64, method: &str) -> Result<(String, String), (String, i64)> {
        // check state/conflict first (immutable borrow), then mutate
        let (conflict, state_open) = {
            let repo = self.repos.get(repo_key).unwrap();
            let pr = repo.pulls.get(&number).unwrap();
            (Self::pull_conflict(repo, pr), pr.state == "open")
        };
        if !state_open { return Err(("Pull request is already closed".into(), 405)); }
        if conflict { return Err(("Merge conflict".into(), 405)); }
        let ts = self.next_ts();
        let merged_at = iso_of(self.next_ts());
        let repo = self.repos.get_mut(repo_key).unwrap();
        let pr = repo.pulls.get(&number).unwrap().clone();
        let base_tip = Self::branch_tip(repo, &pr.base_ref).unwrap();
        let head_tip = Self::branch_tip(repo, &pr.head_ref).unwrap();
        let head_commit = Self::peel_to_commit(repo, &head_tip).unwrap();
        let head_tree = Self::tree_of(repo, &head_commit).unwrap();
        let parents = if method == "merge" { vec![base_tip.clone(), head_commit] } else { vec![base_tip.clone()] };
        let message = format!("Merge pull request #{} from {}\n\n{}", pr.number, pr.head_ref, pr.title);
        let commit = Self::make_commit(repo, &head_tree, &parents, &message, ts);
        repo.set_ref(&format!("refs/heads/{}", pr.base_ref), commit.clone());
        repo.pack_version += 1;
        let pr = repo.pulls.get_mut(&number).unwrap();
        pr.state = "closed".to_string(); pr.merged = true; pr.merge_commit_sha = Some(commit.clone()); pr.merged_at = Some(merged_at);
        Ok((commit, "Pull request successfully merged".to_string()))
    }
}

pub fn gid(kind: &str, key: &str) -> String {
    use std::io::Write;
    // base64 standard, no padding issues: implement inline
    let raw = format!("{}:{}", kind, key);
    base64_encode(raw.as_bytes())
}
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut vals: Vec<u8> = Vec::new();
    let mut acc: u32 = 0; let mut nbits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62, b'/' => 63, b'=' => break, _ => return None,
        };
        acc = (acc << 6) | v as u32; nbits += 6;
        if nbits >= 8 { nbits -= 8; vals.push((acc >> nbits) as u8); }
    }
    Some(vals)
}
pub fn decode_gid(id: &str) -> Option<(String, String)> {
    let bytes = base64_decode(id)?;
    let s = String::from_utf8(bytes).ok()?;
    let i = s.find(':')?;
    Some((s[..i].to_string(), s[i + 1..].to_string()))
}
pub fn line_diff(old_data: Option<&[u8]>, new_data: Option<&[u8]>) -> (i64, i64) {
    let split = |d: Option<&[u8]>| -> Vec<String> {
        match d { None => Vec::new(), Some(b) => {
            let s = String::from_utf8_lossy(b);
            let mut v: Vec<String> = s.split('\n').map(|x| x.to_string()).collect();
            if v.last().map(|x| x.is_empty()) == Some(true) { v.pop(); }
            v
        }}
    };
    let old_lines = split(old_data);
    let new_lines = split(new_data);
    let n = old_lines.len(); let m = new_lines.len();
    if n == 0 { return (m as i64, 0); }
    if m == 0 { return (0, n as i64); }
    if n * m > 400000 {
        let set: std::collections::HashSet<&str> = old_lines.iter().map(|s| s.as_str()).collect();
        let common = new_lines.iter().filter(|l| set.contains(l.as_str())).count();
        return ((m - common) as i64, (n - common) as i64);
    }
    let mut prev = vec![0i64; m + 1];
    let mut cur = vec![0i64; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            cur[j] = if old_lines[i - 1] == new_lines[j - 1] { prev[j - 1] + 1 } else { prev[j].max(cur[j - 1]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let lcs = prev[m];
    ((m as i64) - lcs, (n as i64) - lcs)
}
