// Git object model, pkt-line framing and packfiles — port of the Node entry.
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};

pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjType { Commit, Tree, Blob, Tag }
impl ObjType {
    pub fn code(self) -> u8 { match self { ObjType::Commit => 1, ObjType::Tree => 2, ObjType::Blob => 3, ObjType::Tag => 4 } }
    pub fn from_code(c: u8) -> Option<ObjType> { match c { 1 => Some(ObjType::Commit), 2 => Some(ObjType::Tree), 3 => Some(ObjType::Blob), 4 => Some(ObjType::Tag), _ => None } }
    pub fn name(self) -> &'static str { match self { ObjType::Commit => "commit", ObjType::Tree => "tree", ObjType::Blob => "blob", ObjType::Tag => "tag" } }
}

#[derive(Clone, Debug)]
pub struct GitObj { pub typ: ObjType, pub data: Vec<u8> }

pub fn is_oid(v: &str) -> bool {
    v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit() && (b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b)))
}
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes { s.push_str(&format!("{:02x}", b)); }
    s
}
pub fn hex_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i + 1 < b.len() {
        let h = |c: u8| -> u8 { if c >= b'0' && c <= b'9' { c - b'0' } else if c >= b'a' && c <= b'f' { c - b'a' + 10 } else { c - b'A' + 10 } };
        out.push((h(b[i]) << 4) | h(b[i + 1]));
        i += 2;
    }
    out
}

pub fn hash_object(typ: ObjType, data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("{} {}\0", typ.name(), data.len()).as_bytes());
    h.update(data);
    hex_encode(&h.finalize())
}
pub fn put_obj(store: &mut HashMap<String, GitObj>, typ: ObjType, data: Vec<u8>) -> String {
    let oid = hash_object(typ, &data);
    store.entry(oid.clone()).or_insert(GitObj { typ, data });
    oid
}

// ---------------------------------------------------------------- pkt-line
pub fn pkt(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
    out
}
pub const FLUSH: &[u8] = b"0000";
pub const DELIM: &[u8] = b"0001";
pub fn pkt_lines(items: &[PktItem]) -> Vec<u8> {
    let mut out = Vec::new();
    for it in items {
        match it {
            PktItem::Flush => out.extend_from_slice(FLUSH),
            PktItem::Delim => out.extend_from_slice(DELIM),
            PktItem::Data(d) => out.extend_from_slice(&pkt(d)),
        }
    }
    out
}
#[derive(Clone, Debug)]
pub enum PktItem { Data(Vec<u8>), Flush, Delim }
impl From<&str> for PktItem { fn from(s: &str) -> Self { PktItem::Data(s.as_bytes().to_vec()) } }
impl From<String> for PktItem { fn from(s: String) -> Self { PktItem::Data(s.into_bytes()) } }
impl From<Vec<u8>> for PktItem { fn from(v: Vec<u8>) -> Self { PktItem::Data(v) } }

#[derive(Clone, Debug)]
pub enum Packet { Data(Vec<u8>), Flush, Delim, End }
fn hex_val(b: u8) -> i32 {
    if (b'0'..=b'9').contains(&b) { (b - b'0') as i32 }
    else if (b'a'..=b'f').contains(&b) { (b - b'a' + 10) as i32 }
    else if (b'A'..=b'F').contains(&b) { (b - b'A' + 10) as i32 }
    else { -1 }
}
pub fn decode_pkt_full(buf: &[u8]) -> (Vec<Packet>, usize) {
    let mut packets = Vec::new();
    let mut off = 0usize;
    while off + 4 <= buf.len() {
        let h0 = hex_val(buf[off]); let h1 = hex_val(buf[off + 1]); let h2 = hex_val(buf[off + 2]); let h3 = hex_val(buf[off + 3]);
        if h0 < 0 || h1 < 0 || h2 < 0 || h3 < 0 { break; }
        let len = ((h0 << 12) | (h1 << 8) | (h2 << 4) | h3) as usize;
        if len == 0 { packets.push(Packet::Flush); off += 4; continue; }
        if len == 1 { packets.push(Packet::Delim); off += 4; continue; }
        if len == 2 { packets.push(Packet::End); off += 4; continue; }
        if len < 4 || off + len > buf.len() { break; }
        packets.push(Packet::Data(buf[off + 4..off + len].to_vec()));
        off += len;
    }
    (packets, off)
}

// ---------------------------------------------------------------- trees / commits
#[derive(Clone, Debug)]
pub struct TreeEntry { pub mode: String, pub name: String, pub oid: String }
pub fn parse_tree(data: &[u8]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        let sp = match data[off..].iter().position(|&b| b == b' ') { Some(p) => off + p, None => break };
        let nul = match data[sp + 1..].iter().position(|&b| b == 0) { Some(p) => sp + 1 + p, None => break };
        let mode = String::from_utf8_lossy(&data[off..sp]).into_owned();
        let name = String::from_utf8_lossy(&data[sp + 1..nul]).into_owned();
        let oid = hex_encode(&data[nul + 1..nul + 21]);
        entries.push(TreeEntry { mode, name, oid });
        off = nul + 21;
    }
    entries
}
pub fn build_tree(entries: &[TreeEntry]) -> Vec<u8> {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        let an = if a.mode == "40000" { format!("{}/", a.name) } else { a.name.clone() };
        let bn = if b.mode == "40000" { format!("{}/", b.name) } else { b.name.clone() };
        an.as_bytes().cmp(bn.as_bytes())
    });
    let mut out = Vec::new();
    for e in sorted {
        out.extend_from_slice(format!("{} {}\0", e.mode, e.name).as_bytes());
        out.extend_from_slice(&hex_decode(&e.oid));
    }
    out
}
#[derive(Clone, Debug, Default)]
pub struct CommitView { pub tree: String, pub parents: Vec<String>, pub author: String, pub committer: String, pub message: String }
pub fn parse_commit(data: &[u8]) -> CommitView {
    let blank = data.windows(2).position(|w| w == b"\n\n");
    let (head, message) = match blank {
        Some(b) => (String::from_utf8_lossy(&data[..b]).into_owned(), String::from_utf8_lossy(&data[b + 2..]).into_owned()),
        None => (String::from_utf8_lossy(data).into_owned(), String::new()),
    };
    let mut v = CommitView::default();
    v.message = message;
    for line in head.split('\n') {
        let sp = line.find(' ');
        let (k, val) = match sp { Some(i) => (&line[..i], &line[i + 1..]), None => (line, "") };
        match k {
            "tree" => v.tree = val.to_string(),
            "parent" => v.parents.push(val.to_string()),
            "author" => v.author = val.to_string(),
            "committer" => v.committer = val.to_string(),
            _ => {}
        }
    }
    v
}
pub fn build_commit(tree: &str, parents: &[String], author: &str, committer: &str, message: &str) -> Vec<u8> {
    let mut lines = vec![format!("tree {}", tree)];
    for p in parents { lines.push(format!("parent {}", p)); }
    lines.push(format!("author {}", author));
    lines.push(format!("committer {}", committer));
    lines.push(String::new());
    format!("{}\n{}", lines.join("\n"), message).into_bytes()
}
pub struct TagView { pub object: String, pub typ: ObjType, pub tag: String, pub tagger: String, pub message: String }
pub fn parse_tag(data: &[u8]) -> TagView {
    let blank = data.windows(2).position(|w| w == b"\n\n");
    let (head, message) = match blank {
        Some(b) => (String::from_utf8_lossy(&data[..b]).into_owned(), String::from_utf8_lossy(&data[b + 2..]).into_owned()),
        None => (String::from_utf8_lossy(data).into_owned(), String::new()),
    };
    let mut m: HashMap<String, String> = HashMap::new();
    for line in head.split('\n') {
        if let Some(sp) = line.find(' ') { m.insert(line[..sp].to_string(), line[sp + 1..].to_string()); }
    }
    let typ = match m.get("type").map(|s| s.as_str()) { Some("tree") => ObjType::Tree, Some("blob") => ObjType::Blob, Some("tag") => ObjType::Tag, _ => ObjType::Commit };
    TagView { object: m.get("object").cloned().unwrap_or_default(), typ, tag: m.get("tag").cloned().unwrap_or_default(), tagger: m.get("tagger").cloned().unwrap_or_default(), message }
}

pub fn collect_reachable(roots: &[String], lookup: &dyn Fn(&str) -> Option<GitObj>) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut queue: Vec<String> = roots.iter().filter(|r| is_oid(r)).cloned().collect();
    while let Some(oid) = queue.pop() {
        if seen.contains(&oid) { continue; }
        let o = match lookup(&oid) { Some(o) => o, None => continue };
        seen.insert(oid);
        match o.typ {
            ObjType::Commit => { let c = parse_commit(&o.data); queue.push(c.tree); for p in c.parents { queue.push(p); } }
            ObjType::Tag => queue.push(parse_tag(&o.data).object),
            ObjType::Tree => { for e in parse_tree(&o.data) { if e.mode != "160000" { queue.push(e.oid); } } }
            ObjType::Blob => {}
        }
    }
    seen
}
pub fn is_ancestor(store: &HashMap<String, GitObj>, anc: &str, oid: &str) -> bool {
    if anc == oid { return true; }
    let mut seen = HashSet::new();
    let mut queue = vec![oid.to_string()];
    while let Some(cur) = queue.pop() {
        if !seen.insert(cur.clone()) { continue; }
        let o = match store.get(&cur) { Some(o) => o, None => continue };
        if o.typ != ObjType::Commit { continue; }
        for p in parse_commit(&o.data).parents {
            if p == anc { return true; }
            queue.push(p);
        }
    }
    false
}
pub fn merge_base(store: &HashMap<String, GitObj>, a: &str, b: &str) -> Option<String> {
    if a == b { return Some(a.to_string()); }
    let mut anc_a: HashMap<String, usize> = HashMap::new();
    let mut qa: Vec<(String, usize)> = vec![(a.to_string(), 0)];
    while let Some((cur, d)) = qa.pop() {
        // BFS order matters for the deterministic choice; use a queue via Vec as stack
        // but the Node version uses shift() (FIFO). Emulate FIFO with an index cursor.
        let _ = (cur, d);
        break;
    }
    // FIFO BFS (Node uses shift())
    let mut queue: Vec<(String, usize)> = vec![(a.to_string(), 0)];
    let mut qi = 0;
    while qi < queue.len() {
        let (cur, d) = queue[qi].clone(); qi += 1;
        if anc_a.contains_key(&cur) { continue; }
        anc_a.insert(cur.clone(), d);
        if let Some(o) = store.get(&cur) { if o.typ == ObjType::Commit { for p in parse_commit(&o.data).parents { queue.push((p, d + 1)); } } }
    }
    let mut qb: Vec<(String, usize)> = vec![(b.to_string(), 0)];
    let mut qi = 0;
    let mut seen_b = HashSet::new();
    let mut best: Option<String> = None;
    let mut best_score = usize::MAX;
    while qi < qb.len() {
        let (cur, d) = qb[qi].clone(); qi += 1;
        if !seen_b.insert(cur.clone()) { continue; }
        if let Some(da) = anc_a.get(&cur) {
            let score = da + d;
            if score < best_score { best_score = score; best = Some(cur.clone()); }
        }
        if let Some(o) = store.get(&cur) { if o.typ == ObjType::Commit { for p in parse_commit(&o.data).parents { qb.push((p, d + 1)); } } }
    }
    let _ = qa;
    best
}
pub fn flatten_tree(store: &HashMap<String, GitObj>, tree_oid: &str) -> HashMap<String, (String, String)> {
    let mut out: HashMap<String, (String, String)> = HashMap::new();
    fn walk(store: &HashMap<String, GitObj>, oid: &str, pre: &str, out: &mut HashMap<String, (String, String)>) {
        let o = match store.get(oid) { Some(o) if o.typ == ObjType::Tree => o, _ => return };
        for e in parse_tree(&o.data) {
            if e.mode == "40000" { walk(store, &e.oid, &format!("{}{}/", pre, e.name), out); }
            else { out.insert(format!("{}{}", pre, e.name), (e.oid, e.mode)); }
        }
    }
    walk(store, tree_oid, "", &mut out);
    out
}

// ---------------------------------------------------------------- delta / pack read
fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut off = 0usize;
    let mut read_size = |off: &mut usize| -> usize {
        let mut v = 0usize; let mut sh = 0u32;
        loop {
            let b = delta[*off]; *off += 1;
            v += ((b & 0x7f) as usize) << sh; sh += 7;
            if b & 0x80 == 0 { break; }
        }
        v
    };
    let src_size = read_size(&mut off);
    let tgt_size = read_size(&mut off);
    if src_size != base.len() { return Err("delta base size mismatch".into()); }
    let mut out = vec![0u8; tgt_size];
    let mut written = 0usize;
    while off < delta.len() {
        let op = delta[off]; off += 1;
        if op & 0x80 != 0 {
            let mut co = 0usize; let mut cs = 0usize; let mut sh = 0u32;
            for bit in 0..4 { if op & (1 << bit) != 0 { co += (delta[off] as usize) << sh; off += 1; } sh += 8; }
            sh = 0;
            for bit in 4..7 { if op & (1 << bit) != 0 { cs += (delta[off] as usize) << sh; off += 1; } sh += 8; }
            if cs == 0 { cs = 0x10000; }
            out[written..written + cs].copy_from_slice(&base[co..co + cs]);
            written += cs;
        } else if op != 0 {
            let n = (op & 0x7f) as usize;
            out[written..written + n].copy_from_slice(&delta[off..off + n]);
            off += n; written += n;
        } else { return Err("delta opcode 0".into()); }
    }
    if written != tgt_size { return Err("delta size mismatch".into()); }
    Ok(out)
}

fn zlib_inflate(data: &[u8], expected: usize) -> Result<(Vec<u8>, usize), String> {
    use flate2::{Decompress, FlushDecompress};
    let mut d = Decompress::new(true);
    // NOTE: decompress_vec APPENDS into spare capacity, so `out` must start
    // EMPTY (with capacity only). Starting from vec![0; expected] used to
    // return all-zero data truncated to total_out — the root cause of every
    // git failure (zero-filled commits/trees/blobs, lost parents).
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let status = d.decompress_vec(data, &mut out, FlushDecompress::Finish).map_err(|e| e.to_string())?;
    let _ = status;
    let consumed = d.total_in() as usize;
    Ok((out, consumed))
}
fn zlib_deflate(data: &[u8]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut c = Compress::new(Compression::fast(), true);
    // Same append-into-spare-capacity rule as decompress_vec: start empty.
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + data.len() / 10 + 64);
    let _ = c.compress_vec(data, &mut out, FlushCompress::Finish);
    out
}

pub struct PackRead { pub objects: HashMap<String, GitObj>, pub delta_count: usize }
pub fn read_pack(pack: &[u8], resolve_base: &dyn Fn(&str) -> Option<GitObj>) -> Result<PackRead, String> {
    if pack.len() < 32 || &pack[0..4] != b"PACK" { return Err("bad pack header".into()); }
    let version = u32::from_be_bytes([pack[4], pack[5], pack[6], pack[7]]);
    if version != 2 && version != 3 { return Err("bad pack version".into()); }
    let count = u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]) as usize;
    let trailer_start = pack.len() - 20;
    struct Raw { offset: usize, type_code: u8, size: usize, data: Vec<u8>, base_offset: Option<usize>, base_oid: Option<String> }
    let mut raw: Vec<Raw> = Vec::with_capacity(count);
    let mut by_offset: HashMap<usize, usize> = HashMap::new();
    let mut off = 12usize;
    let mut delta_count = 0usize;
    for _ in 0..count {
        let entry_off = off;
        // varint
        let mut b = pack[off];
        let first = b;
        let mut size = (b & 0x0f) as usize; let mut sh = 4u32; off += 1;
        while b & 0x80 != 0 { b = pack[off]; off += 1; size += ((b & 0x7f) as usize) << sh; sh += 7; }
        let type_code = (first >> 4) & 0x07;
        if type_code == 6 || type_code == 7 { delta_count += 1; }
        let mut e = Raw { offset: entry_off, type_code, size, data: Vec::new(), base_offset: None, base_oid: None };
        if type_code == 6 {
            let mut b = pack[off]; off += 1;
            let mut v = (b & 0x7f) as usize;
            while b & 0x80 != 0 { b = pack[off]; off += 1; v = ((v + 1) * 128) + ((b & 0x7f) as usize); }
            e.base_offset = Some(entry_off - v);
        } else if type_code == 7 {
            e.base_oid = Some(hex_encode(&pack[off..off + 20])); off += 20;
        } else if ObjType::from_code(type_code).is_none() { return Err("bad pack type".into()); }
        let (inflated, consumed) = zlib_inflate(&pack[off..trailer_start], size)?;
        if inflated.len() != size { return Err("pack entry size mismatch".into()); }
        e.data = inflated;
        off += consumed;
        by_offset.insert(entry_off, raw.len());
        raw.push(e);
    }
    let mut objects: HashMap<String, GitObj> = HashMap::new();
    if delta_count == 0 {
        for e in &raw {
            let typ = ObjType::from_code(e.type_code).unwrap();
            let oid = hash_object(typ, &e.data);
            objects.entry(oid).or_insert(GitObj { typ, data: e.data.clone() });
        }
        return Ok(PackRead { objects, delta_count: 0 });
    }
    // delta resolution (recursive, with memo)
    struct Resolved { typ: ObjType, data: Vec<u8>, oid: String }
    fn resolve(idx: usize, raw: &[Raw], by_offset: &HashMap<usize, usize>, resolve_base: &dyn Fn(&str) -> Option<GitObj>, memo: &mut HashMap<usize, Resolved>, resolving: &mut HashSet<usize>) -> Result<Resolved, String> {
        if let Some(r) = memo.get(&idx) { return Ok(Resolved { typ: r.typ, data: r.data.clone(), oid: r.oid.clone() }); }
        if !resolving.insert(idx) { return Err("delta cycle".into()); }
        let e = &raw[idx];
        let (typ, data): (ObjType, Vec<u8>) = if e.type_code == 6 {
            let base_idx = *by_offset.get(&e.base_offset.unwrap()).ok_or("ofs-delta base missing")?;
            let base = resolve(base_idx, raw, by_offset, resolve_base, memo, resolving)?;
            (base.typ, apply_delta(&base.data, &e.data)?)
        } else if e.type_code == 7 {
            let base_oid = e.base_oid.as_ref().unwrap();
            // find base inside pack by oid (resolve candidates whose oid matches)
            let mut found: Option<Resolved> = None;
            for (j, cand) in raw.iter().enumerate() {
                if cand.type_code == 6 || cand.type_code == 7 { continue; }
                let typ = ObjType::from_code(cand.type_code).unwrap();
                if hash_object(typ, &cand.data) == *base_oid { found = Some(resolve(j, raw, by_offset, resolve_base, memo, resolving)?); break; }
            }
            match found {
                Some(base) => (base.typ, apply_delta(&base.data, &e.data)?),
                None => {
                    let ext = resolve_base(base_oid).ok_or("ref-delta base missing")?;
                    (ext.typ, apply_delta(&ext.data, &e.data)?)
                }
            }
        } else {
            (ObjType::from_code(e.type_code).unwrap(), e.data.clone())
        };
        let oid = hash_object(typ, &data);
        resolving.remove(&idx);
        memo.insert(idx, Resolved { typ, data: data.clone(), oid: oid.clone() });
        Ok(Resolved { typ, data, oid })
    }
    let mut memo: HashMap<usize, Resolved> = HashMap::new();
    let mut resolving: HashSet<usize> = HashSet::new();
    for i in 0..raw.len() {
        let r = resolve(i, &raw, &by_offset, resolve_base, &mut memo, &mut resolving)?;
        objects.entry(r.oid).or_insert(GitObj { typ: r.typ, data: r.data });
    }
    Ok(PackRead { objects, delta_count })
}

fn pack_obj_header(typ: ObjType, size: usize) -> Vec<u8> {
    let code = typ.code() as usize;
    let mut bytes: Vec<u8> = Vec::new();
    let mut rem = size;
    let mut first = (code << 4) | (rem & 0x0f);
    rem /= 16;
    while rem > 0 { bytes.push((first | 0x80) as u8); first = rem & 0x7f; rem /= 128; }
    bytes.push(first as u8);
    bytes
}
pub fn write_pack(objs: &[&GitObj]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"PACK");
    body.extend_from_slice(&2u32.to_be_bytes());
    body.extend_from_slice(&(objs.len() as u32).to_be_bytes());
    for o in objs {
        body.extend_from_slice(&pack_obj_header(o.typ, o.data.len()));
        body.extend_from_slice(&zlib_deflate(&o.data));
    }
    let mut h = Sha1::new(); h.update(&body);
    let trailer = h.finalize();
    body.extend_from_slice(&trailer);
    body
}

// ---------------------------------------------------------------- fetch planning
pub struct FetchPlan { pub send: HashSet<String>, pub shallow: Vec<String>, pub unshallow: Vec<String> }
pub fn plan_fetch(store: &HashMap<String, GitObj>, wants: &[String], haves: &[String], shallow_bounds: &[String], depth: Option<usize>) -> FetchPlan {
    let lookup = |oid: &str| -> Option<GitObj> { store.get(oid).cloned() };
    let shallow_set: HashSet<String> = shallow_bounds.iter().cloned().collect();
    let mut included: HashSet<String> = HashSet::new();
    let mut boundary: HashSet<String> = HashSet::new();
    if let Some(depth) = depth {
        let mut commit_cache: HashMap<String, CommitView> = HashMap::new();
        let commit_of = |oid: &str, store: &HashMap<String, GitObj>, cache: &mut HashMap<String, CommitView>| -> Option<CommitView> {
            if let Some(c) = cache.get(oid) { return Some(c.clone()); }
            let o = store.get(oid)?; if o.typ != ObjType::Commit { return None; }
            let c = parse_commit(&o.data); cache.insert(oid.to_string(), c.clone()); Some(c)
        };
        let mut dist: HashMap<String, usize> = HashMap::new();
        let mut queue: Vec<String> = Vec::new();
        for w in wants {
            let o = match store.get(w) { Some(o) => o, None => continue };
            if o.typ == ObjType::Tag { let t = parse_tag(&o.data).object; dist.insert(w.clone(), 0); if !dist.contains_key(&t) { dist.insert(t.clone(), 0); queue.push(t); } }
            else if o.typ == ObjType::Commit { dist.insert(w.clone(), 0); queue.push(w.clone()); }
            else { dist.insert(w.clone(), 0); }
        }
        let mut commit_at: HashMap<String, usize> = HashMap::new();
        let mut qi = 0;
        while qi < queue.len() {
            let cur = queue[qi].clone(); qi += 1;
            let d = dist[&cur];
            let o = match store.get(&cur) { Some(o) => o, None => continue };
            if o.typ == ObjType::Commit {
                commit_at.insert(cur.clone(), d);
                if d < depth - 1 {
                    for p in commit_of(&cur, store, &mut commit_cache).map(|c| c.parents).unwrap_or_default() {
                        if !dist.contains_key(&p) { dist.insert(p.clone(), d + 1); queue.push(p); }
                    }
                }
            }
        }
        for (c, d) in &commit_at {
            let parents = commit_of(c, store, &mut commit_cache).map(|x| x.parents).unwrap_or_default();
            if *d == depth - 1 || parents.is_empty() || parents.iter().all(|p| !commit_at.contains_key(p)) { boundary.insert(c.clone()); }
        }
        let mut keep: HashSet<String> = HashSet::new();
        fn add_closure(oid: &str, store: &HashMap<String, GitObj>, commit_at: &HashMap<String, usize>, cache: &mut HashMap<String, CommitView>, keep: &mut HashSet<String>) {
            let o = match store.get(oid) { Some(o) => o, None => return };
            if !keep.insert(oid.to_string()) { return; }
            match o.typ {
                ObjType::Commit => {
                    if commit_at.contains_key(oid) {
                        let c = parse_commit(&o.data);
                        cache.insert(oid.to_string(), c.clone());
                        add_closure(&c.tree, store, commit_at, cache, keep);
                        for p in c.parents { if commit_at.contains_key(&p) { add_closure(&p, store, commit_at, cache, keep); } }
                    }
                }
                ObjType::Tag => { let t = parse_tag(&o.data).object; add_closure(&t, store, commit_at, cache, keep); }
                ObjType::Tree => { for e in parse_tree(&o.data) { add_closure(&e.oid, store, commit_at, cache, keep); } }
                ObjType::Blob => {}
            }
        }
        for w in wants { add_closure(w, store, &commit_at, &mut commit_cache, &mut keep); }
        included = keep;
    } else {
        included = collect_reachable(wants, &lookup);
    }
    if !haves.is_empty() {
        if depth.is_some() {
            let mut held: HashSet<String> = HashSet::new();
            for h in haves {
                let o = match store.get(h) { Some(o) => o, None => continue };
                if shallow_set.contains(h) && o.typ == ObjType::Commit {
                    held.insert(h.clone());
                    let c = parse_commit(&o.data);
                    for x in collect_reachable(&[c.tree], &lookup) { held.insert(x); }
                } else {
                    for x in collect_reachable(&[h.clone()], &lookup) { held.insert(x); }
                }
            }
            for x in held { included.remove(&x); }
        } else {
            let held = collect_reachable(haves, &lookup);
            for x in held { included.remove(&x); }
        }
    }
    let mut unshallow: Vec<String> = Vec::new();
    for b in shallow_bounds {
        if boundary.contains(b) { continue; }
        if included.contains(b) && !boundary.contains(b) { unshallow.push(b.clone()); }
    }
    let mut shallow: Vec<String> = boundary.into_iter().collect(); shallow.sort();
    unshallow.sort();
    FetchPlan { send: included, shallow, unshallow }
}
