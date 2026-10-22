//! Reads tables out of an SQLite database file, without SQLite and without ever writing: enough to
//! list another app's records (Medal keeps its clips in one). Pages the app wrote to its
//! write-ahead log (`<file>-wal`) but not yet back into the file are read from the log. Opening
//! the file with SQLite itself could fold that log into it, or leave lock files beside it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Bigger files aren't read (Medal's is a few MB).
const MAX_FILE: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Real(f) => Some(*f as i64),
            Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    /// A JSON value stored as JSONB (SQLite's binary form) or as JSON text.
    pub fn json(&self) -> Option<serde_json::Value> {
        match self {
            Value::Blob(b) => jsonb(b).or_else(|| serde_json::from_slice(b).ok()),
            Value::Text(s) => serde_json::from_str(s).ok(),
            Value::Int(i) => Some((*i).into()),
            Value::Real(f) => serde_json::Number::from_f64(*f).map(serde_json::Value::Number),
            Value::Null => None,
        }
    }
}

/// A table's rows, with its columns' names.
#[derive(Debug, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    /// The value in `row` under the column `name` (`Null` if there's no such column).
    pub fn get<'a>(&self, row: &'a [Value], name: &str) -> &'a Value {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .and_then(|i| row.get(i))
            .unwrap_or(&Value::Null)
    }
}

pub struct Db {
    file: Vec<u8>,
    page_size: usize,
    /// Bytes of each page that hold data (the rest is reserved for extensions).
    usable: usize,
    /// Pages whose latest copy is in the log: where each starts in `log`.
    wal: HashMap<u32, usize>,
    log: Vec<u8>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        let file = read_limited(path)?;
        if file.len() < 100 || !file.starts_with(b"SQLite format 3\0") {
            return Err(format!("{} isn't an SQLite database", path.display()));
        }
        let page_size = match u16::from_be_bytes([file[16], file[17]]) {
            1 => 65536,
            n if n >= 512 && n.is_power_of_two() => n as usize,
            _ => return Err("bad page size".into()),
        };
        if file[56..60] != [0, 0, 0, 1] && file[56..60] != [0, 0, 0, 0] {
            return Err("only UTF-8 databases can be read".into());
        }
        let usable = page_size - file[20] as usize;
        if usable < 480 {
            return Err("bad page size".into());
        }
        let mut db = Db {
            file,
            page_size,
            usable,
            wal: HashMap::new(),
            log: Vec::new(),
        };
        let mut wal_path = path.as_os_str().to_owned();
        wal_path.push("-wal");
        if let Ok(log) = read_limited(Path::new(&wal_path)) {
            db.wal = wal_pages(&log, page_size);
            db.log = log;
        }
        Ok(db)
    }

    fn page(&self, n: u32) -> Result<&[u8], String> {
        let p = match self.wal.get(&n) {
            Some(&at) => self.log.get(at..at + self.page_size),
            None => (n as usize)
                .checked_sub(1)
                .and_then(|i| self.file.get(i * self.page_size..(i + 1) * self.page_size)),
        };
        p.ok_or_else(|| format!("page {n} is missing"))
    }

    /// Every row of the table `name`, in rowid order.
    pub fn table(&self, name: &str) -> Result<Table, String> {
        let master = self.rows(1)?;
        let (root, sql) = master
            .iter()
            .find_map(|(_, r)| {
                let is_table = r.first().and_then(Value::as_str) == Some("table");
                let named = r
                    .get(1)
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.eq_ignore_ascii_case(name));
                (is_table && named).then(|| {
                    (
                        r.get(3).and_then(Value::as_i64).unwrap_or(0),
                        r.get(4).and_then(Value::as_str).unwrap_or("").to_string(),
                    )
                })
            })
            .ok_or_else(|| format!("no table {name}"))?;
        let defs = column_defs(&sql);
        let columns: Vec<String> = defs.iter().map(|(n, _)| n.clone()).collect();
        let alias = defs.iter().position(|(_, rowid)| *rowid);
        let rows = self
            .rows(u32::try_from(root).map_err(|_| "bad root page")?)?
            .into_iter()
            .map(|(rowid, mut r)| {
                // Columns added later are missing from older rows.
                r.resize(columns.len(), Value::Null);
                // An INTEGER PRIMARY KEY column is the rowid, stored as NULL.
                if let Some(i) = alias {
                    r[i] = Value::Int(rowid);
                }
                r
            })
            .collect();
        Ok(Table { columns, rows })
    }

    /// The (rowid, values) of every row in the table b-tree at `root`.
    fn rows(&self, root: u32) -> Result<Vec<(i64, Vec<Value>)>, String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.walk(root, &mut out, &mut seen)?;
        Ok(out)
    }

    fn walk(
        &self,
        n: u32,
        out: &mut Vec<(i64, Vec<Value>)>,
        seen: &mut HashSet<u32>,
    ) -> Result<(), String> {
        // A page twice means the file changed while it was read, or is damaged.
        if !seen.insert(n) || seen.len() > 1_000_000 {
            return Err("the table's pages loop".into());
        }
        let page = self.page(n)?;
        let h = if n == 1 { 100 } else { 0 };
        let bad = || format!("page {n} is damaged");
        let kind = *page.get(h).ok_or_else(bad)?;
        if kind != 0x0d && kind != 0x05 {
            return Err(bad());
        }
        let cells = be16(page, h + 3).ok_or_else(bad)?;
        let head = if kind == 0x05 { 12 } else { 8 };
        for i in 0..cells {
            let at = be16(page, h + head + 2 * i).ok_or_else(bad)?;
            if kind == 0x0d {
                out.push(self.leaf_cell(page, at).ok_or_else(bad)?);
            } else {
                self.walk(be32(page, at).ok_or_else(bad)?, out, seen)?;
            }
        }
        if kind == 0x05 {
            self.walk(be32(page, h + 8).ok_or_else(bad)?, out, seen)?;
        }
        Ok(())
    }

    fn leaf_cell(&self, page: &[u8], at: usize) -> Option<(i64, Vec<Value>)> {
        let (size, a) = varint(page.get(at..)?)?;
        let (rowid, b) = varint(page.get(at + a..)?)?;
        let start = at + a + b;
        let size = usize::try_from(size).ok()?;
        let local = self.local_size(size);
        let mut payload = page.get(start..start + local)?.to_vec();
        if local < size {
            // The rest is on a chain of overflow pages: each starts with the next one's number.
            let mut next = be32(page, start + local)?;
            let mut hops = 0;
            while payload.len() < size {
                hops += 1;
                if next == 0 || hops > 1_000_000 {
                    return None;
                }
                let p = self.page(next).ok()?;
                let take = (size - payload.len()).min(self.usable - 4);
                payload.extend_from_slice(p.get(4..4 + take)?);
                next = be32(p, 0)?;
            }
        }
        Some((rowid as i64, record(&payload)?))
    }

    /// How much of a payload of `size` bytes is kept in its leaf page.
    fn local_size(&self, size: usize) -> usize {
        let u = self.usable;
        let max = u - 35;
        if size <= max {
            return size;
        }
        let min = (u - 12) * 32 / 255 - 23;
        let k = min + (size - min) % (u - 4);
        if k <= max { k } else { min }
    }
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len > MAX_FILE {
        return Err(format!("{} is too big", path.display()));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn be16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// SQLite's variable-length integer: 7 bits a byte, high bit set while more follow; the 9th byte
/// gives all 8. Returns the value and how many bytes it took.
fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b.get(i)?;
        if i == 8 {
            return Some(((v << 8) | byte as u64, 9));
        }
        v = (v << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// A row's values from its record: a header of types, then the values one after another.
fn record(p: &[u8]) -> Option<Vec<Value>> {
    let (head, mut at) = varint(p)?;
    let head = usize::try_from(head).ok()?;
    let mut body = head;
    let mut out = Vec::new();
    while at < head {
        let (t, n) = varint(p.get(at..head)?)?;
        at += n;
        let int = |len: usize| -> Option<i64> {
            let b = p.get(body..body + len)?;
            // Sign-extended big-endian.
            let mut v: i64 = if b[0] & 0x80 != 0 { -1 } else { 0 };
            for &x in b {
                v = (v << 8) | x as i64;
            }
            Some(v)
        };
        let (value, len) = match t {
            0 => (Value::Null, 0),
            1..=4 => (Value::Int(int(t as usize)?), t as usize),
            5 => (Value::Int(int(6)?), 6),
            6 => (Value::Int(int(8)?), 8),
            7 => (
                Value::Real(f64::from_bits(u64::from_be_bytes(
                    p.get(body..body + 8)?.try_into().ok()?,
                ))),
                8,
            ),
            8 => (Value::Int(0), 0),
            9 => (Value::Int(1), 0),
            10 | 11 => return None,
            t => {
                let len = usize::try_from((t - 12) / 2).ok()?;
                let b = p.get(body..body + len)?;
                if t % 2 == 0 {
                    (Value::Blob(b.to_vec()), len)
                } else {
                    (Value::Text(String::from_utf8_lossy(b).into_owned()), len)
                }
            }
        };
        body += len;
        out.push(value);
    }
    Some(out)
}

/// The columns in a `CREATE TABLE` statement, and whether each is an alias of the rowid
/// (`INTEGER PRIMARY KEY`).
fn column_defs(sql: &str) -> Vec<(String, bool)> {
    let (Some(open), Some(close)) = (sql.find('('), sql.rfind(')')) else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0, open + 1);
    for (i, c) in sql.char_indices().take_while(|(i, _)| *i < close) {
        match c {
            '(' if i > open => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&sql[start..close]);
    parts
        .into_iter()
        .filter_map(|p| {
            let p = p.trim();
            // The name, quoted or not, then its type and constraints.
            let (name, rest) = match p.chars().next()? {
                q @ ('"' | '`' | '[' | '\'') => {
                    let close = if q == '[' { ']' } else { q };
                    let end = p[1..].find(close)? + 1;
                    (&p[1..end], &p[end + 1..])
                }
                _ => {
                    let end = p
                        .find(|c: char| c.is_whitespace() || c == '(')
                        .unwrap_or(p.len());
                    let upper = p[..end].to_ascii_uppercase();
                    if ["PRIMARY", "UNIQUE", "CHECK", "FOREIGN", "CONSTRAINT"]
                        .contains(&upper.as_str())
                    {
                        return None;
                    }
                    (&p[..end], &p[end..])
                }
            };
            let words: Vec<String> = rest
                .split_whitespace()
                .map(|w| w.to_ascii_uppercase())
                .collect();
            let rowid = words.first().is_some_and(|w| w == "INTEGER")
                && words.windows(2).any(|w| w[0] == "PRIMARY" && w[1] == "KEY");
            Some((name.to_string(), rowid))
        })
        .collect()
}

/// Where the latest copy of each page is in a write-ahead log: frames up to the last complete
/// transaction, as long as their checksums hold (a half-written frame ends the log).
fn wal_pages(log: &[u8], page_size: usize) -> HashMap<u32, usize> {
    let mut pages = HashMap::new();
    let Some(magic) = be32(log, 0) else {
        return pages;
    };
    if magic & !1 != 0x377f_0682 || be32(log, 8) != Some(page_size as u32) {
        return pages;
    }
    let big = magic & 1 == 1;
    let (Some(salt1), Some(salt2)) = (be32(log, 16), be32(log, 20)) else {
        return pages;
    };
    let mut sum = checksum(&log[..24], big, (0, 0));
    if Some(sum.0) != be32(log, 24) || Some(sum.1) != be32(log, 28) {
        return pages;
    }
    let mut pending = Vec::new();
    let mut at = 32;
    while let Some(frame) = log.get(at..at + 24 + page_size) {
        if be32(frame, 8) != Some(salt1) || be32(frame, 12) != Some(salt2) {
            break;
        }
        sum = checksum(&frame[..8], big, sum);
        sum = checksum(&frame[24..], big, sum);
        if Some(sum.0) != be32(frame, 16) || Some(sum.1) != be32(frame, 20) {
            break;
        }
        pending.push((be32(frame, 0).unwrap_or(0), at + 24));
        // A frame with the database's size in it ends a transaction.
        if be32(frame, 4).is_some_and(|n| n != 0) {
            pages.extend(pending.drain(..));
        }
        at += 24 + page_size;
    }
    pages
}

fn checksum(data: &[u8], big: bool, (mut s0, mut s1): (u32, u32)) -> (u32, u32) {
    for c in data.chunks_exact(8) {
        let word = |b: &[u8]| {
            let b: [u8; 4] = b.try_into().unwrap_or_default();
            if big {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            }
        };
        s0 = s0.wrapping_add(word(&c[..4])).wrapping_add(s1);
        s1 = s1.wrapping_add(word(&c[4..])).wrapping_add(s0);
    }
    (s0, s1)
}

/// A JSONB value (SQLite's binary JSON) as JSON. `None` unless all of `b` is one valid value.
pub fn jsonb(b: &[u8]) -> Option<serde_json::Value> {
    match jsonb_at(b, 0, 0)? {
        (v, end) if end == b.len() => Some(v),
        _ => None,
    }
}

fn jsonb_at(b: &[u8], at: usize, depth: usize) -> Option<(serde_json::Value, usize)> {
    use serde_json::Value as J;
    if depth > 64 {
        return None;
    }
    let h = *b.get(at)?;
    let (kind, small) = (h & 15, (h >> 4) as usize);
    let (size, start) = match small {
        0..=11 => (small, at + 1),
        12 => (*b.get(at + 1)? as usize, at + 2),
        13 => (be16(b, at + 1)?, at + 3),
        14 => (be32(b, at + 1)? as usize, at + 5),
        _ => (
            usize::try_from(u64::from_be_bytes(b.get(at + 1..at + 9)?.try_into().ok()?)).ok()?,
            at + 9,
        ),
    };
    let end = start.checked_add(size)?;
    let p = b.get(start..end)?;
    let text = || std::str::from_utf8(p).ok();
    let v = match kind {
        0 => J::Null,
        1 => J::Bool(true),
        2 => J::Bool(false),
        // Integers and reals are kept as their text: JSON's, or JSON5's (hex, "Infinity").
        3..=6 => {
            let t = text()?;
            serde_json::from_str::<J>(t)
                .ok()
                .filter(J::is_number)
                .or_else(|| {
                    t.strip_prefix("0x")
                        .or_else(|| t.strip_prefix("0X"))
                        .and_then(|h| i64::from_str_radix(h, 16).ok())
                        .map(J::from)
                })
                .unwrap_or_else(|| J::String(t.to_string()))
        }
        // Text as is, or with JSON's escapes still in it.
        7 | 10 => J::String(text()?.to_string()),
        8 | 9 => serde_json::from_str(&format!("\"{}\"", text()?))
            .unwrap_or_else(|_| J::String(text().unwrap_or_default().to_string())),
        11 => {
            let mut items = Vec::new();
            let mut i = start;
            while i < end {
                let (v, next) = jsonb_at(&b[..end], i, depth + 1)?;
                items.push(v);
                i = next;
            }
            J::Array(items)
        }
        12 => {
            let mut map = serde_json::Map::new();
            let mut i = start;
            while i < end {
                let (k, next) = jsonb_at(&b[..end], i, depth + 1)?;
                let (v, after) = jsonb_at(&b[..end], next, depth + 1)?;
                map.insert(k.as_str()?.to_string(), v);
                i = after;
            }
            J::Object(map)
        }
        _ => return None,
    };
    Some((v, end))
}

/// Writes small databases for tests: each table on one page.
#[cfg(test)]
pub(crate) mod testdb {
    use super::Value;
    use std::path::Path;

    const PAGE: usize = 65536;

    fn varint(out: &mut Vec<u8>, v: u64) {
        let mut b = vec![(v & 0x7f) as u8];
        let mut x = v >> 7;
        while x > 0 {
            b.push((x & 0x7f) as u8 | 0x80);
            x >>= 7;
        }
        b.reverse();
        out.extend(b);
    }

    fn record(values: &[Value]) -> Vec<u8> {
        let (mut types, mut body) = (Vec::new(), Vec::new());
        for v in values {
            let t = match v {
                Value::Null => 0,
                Value::Int(i) => {
                    body.extend(i.to_be_bytes());
                    6
                }
                Value::Real(f) => {
                    body.extend(f.to_bits().to_be_bytes());
                    7
                }
                Value::Text(s) => {
                    body.extend(s.as_bytes());
                    13 + 2 * s.len() as u64
                }
                Value::Blob(b) => {
                    body.extend(b);
                    12 + 2 * b.len() as u64
                }
            };
            varint(&mut types, t);
        }
        let mut head = Vec::new();
        varint(&mut head, (types.len() + 1) as u64);
        assert!(types.len() + 1 < 128, "short headers only");
        head.extend(types);
        head.extend(body);
        head
    }

    fn leaf(rows: &[Vec<Value>], offset: usize) -> Vec<u8> {
        let mut page = vec![0u8; PAGE];
        let mut end = PAGE;
        let mut pointers = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let rec = record(r);
            let mut cell = Vec::new();
            varint(&mut cell, rec.len() as u64);
            varint(&mut cell, i as u64 + 1);
            cell.extend(rec);
            end -= cell.len();
            page[end..end + cell.len()].copy_from_slice(&cell);
            pointers.push(end as u16);
        }
        page[offset] = 0x0d;
        page[offset + 3..offset + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
        page[offset + 5..offset + 7].copy_from_slice(&((end % PAGE) as u16).to_be_bytes());
        for (i, p) in pointers.iter().enumerate() {
            let at = offset + 8 + 2 * i;
            page[at..at + 2].copy_from_slice(&p.to_be_bytes());
        }
        assert!(offset + 8 + 2 * rows.len() <= end, "the rows fit on a page");
        page
    }

    /// `tables`: each table's `CREATE TABLE` statement, name and rows.
    pub fn write(path: &Path, tables: &[(&str, &str, Vec<Vec<Value>>)]) {
        let master: Vec<Vec<Value>> = tables
            .iter()
            .enumerate()
            .map(|(i, (sql, name, _))| {
                vec![
                    Value::Text("table".into()),
                    Value::Text(name.to_string()),
                    Value::Text(name.to_string()),
                    Value::Int(i as i64 + 2),
                    Value::Text(sql.to_string()),
                ]
            })
            .collect();
        let mut file = leaf(&master, 100);
        file[..16].copy_from_slice(b"SQLite format 3\0");
        file[16..18].copy_from_slice(&1u16.to_be_bytes());
        file[18] = 1;
        file[19] = 1;
        file[21] = 64;
        file[22] = 32;
        file[23] = 32;
        file[28..32].copy_from_slice(&(tables.len() as u32 + 1).to_be_bytes());
        file[44..48].copy_from_slice(&4u32.to_be_bytes());
        file[56..60].copy_from_slice(&1u32.to_be_bytes());
        for (_, _, rows) in tables {
            file.extend(leaf(rows, 0));
        }
        std::fs::write(path, file).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_databases_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        let rows = vec![
            vec![Value::Text("a".into()), Value::Int(-5), Value::Null],
            vec![
                Value::Text("é".into()),
                Value::Real(1.5),
                Value::Blob(vec![1, 2]),
            ],
        ];
        testdb::write(
            &p,
            &[("CREATE TABLE t (k TEXT, n, b BLOB)", "t", rows.clone())],
        );
        let t = Db::open(&p).unwrap().table("t").unwrap();
        assert_eq!(t.columns, ["k", "n", "b"]);
        assert_eq!(t.rows, rows);
    }

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/medal-1234.db")
    }

    /// The fixture is made by the real SQLite (scripts/medal_fixture.py): small pages, so the
    /// table has interior pages; a long row on overflow pages; and a row only in the log.
    #[test]
    fn reads_a_real_sqlite_file_and_its_log() {
        let db = Db::open(&fixture()).unwrap();
        let t = db.table("contents").unwrap();
        assert_eq!(t.columns.len(), 10);
        assert_eq!(t.columns[9], "film_reel_path");
        assert_eq!(t.rows.len(), 46);
        let paths: Vec<&str> = t
            .rows
            .iter()
            .filter_map(|r| t.get(r, "video_path").as_str())
            .collect();
        assert_eq!(paths.len(), 44);
        assert!(paths[0].ends_with("\\Testgame\\MedalTVTestgame20260101120000123.mp4"));
        // Written to the log after the last checkpoint.
        assert!(paths[43].ends_with("MedalTVLateGame20260103100000000.mp4"));
        let meta = |i: usize| t.get(&t.rows[i], "metadata").json().unwrap();
        assert_eq!(meta(40)["title"], "My best shot");
        assert_eq!(meta(40)["isFavorited"], true);
        assert_eq!(meta(40)["clipDuration"], 30.016);
        assert_eq!(meta(41)["title"], "\u{1F44A}3K");
        assert_eq!(meta(41)["recorder"]["eventobj"]["name"], "\u{1F44A}3K");
        // On overflow pages.
        assert_eq!(
            meta(42)["remoteContent"]["contentDescription"]
                .as_str()
                .unwrap()
                .len(),
            3000
        );
        assert_eq!(t.get(&t.rows[0], "created_at").as_i64(), Some(1767268800));
        assert_eq!(*t.get(&t.rows[0], "film_reel_path"), Value::Null);
        assert_eq!(*t.get(&t.rows[0], "no_such_column"), Value::Null);

        let kv = db.table("key_values").unwrap();
        let get = |k: &str| {
            kv.rows
                .iter()
                .find(|r| kv.get(r, "key").as_str() == Some(k))
                .and_then(|r| kv.get(r, "value").json())
        };
        assert_eq!(get("ClipFolder").unwrap(), "Q:\\Medal\\");
        assert_eq!(get("LaunchAutomatically").unwrap(), true);
        // Changed in the log.
        assert_eq!(get("Codec").unwrap(), "H265");
        assert!(db.table("nope").is_err());
    }

    #[test]
    fn without_its_log_the_file_alone_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let only = dir.path().join("medal-1234.db");
        std::fs::copy(fixture(), &only).unwrap();
        let t = Db::open(&only).unwrap().table("contents").unwrap();
        assert_eq!(t.rows.len(), 45);
        // A log cut short (its last frame half written) is used up to its last whole
        // transaction: the new row is in, the change after it (the codec) isn't.
        let log = std::fs::read(fixture().with_extension("db-wal")).unwrap();
        std::fs::write(
            dir.path().join("medal-1234.db-wal"),
            &log[..log.len() - 100],
        )
        .unwrap();
        let db = Db::open(&only).unwrap();
        assert_eq!(db.table("contents").unwrap().rows.len(), 46);
        let kv = db.table("key_values").unwrap();
        let codec = kv
            .rows
            .iter()
            .find(|r| kv.get(r, "key").as_str() == Some("Codec"))
            .and_then(|r| kv.get(r, "value").json());
        assert_eq!(codec.unwrap(), "H264");
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.db");
        std::fs::write(&p, b"not a database").unwrap();
        assert!(Db::open(&p).is_err());
        let good = std::fs::read(fixture()).unwrap();
        for cut in [100, 1024, 3000, good.len() - 1] {
            std::fs::write(&p, &good[..cut]).unwrap();
            if let Ok(db) = Db::open(&p) {
                let _ = db.table("contents");
            }
        }
        // Scrambled bytes all through the pages.
        let mut bad = good.clone();
        for i in (120..bad.len()).step_by(7) {
            bad[i] = bad[i].wrapping_mul(31).wrapping_add(i as u8);
        }
        std::fs::write(&p, &bad).unwrap();
        if let Ok(db) = Db::open(&p) {
            let _ = db.table("contents");
            let _ = db.table("key_values");
        }
    }

    #[test]
    fn jsonb_values() {
        // Medal's ClipFolder: TEXTJ, with the backslashes escaped.
        assert_eq!(
            jsonb(b"\xb8D:\\\\Medal\\\\").unwrap(),
            serde_json::json!("D:\\Medal\\")
        );
        assert_eq!(jsonb(b"\x01").unwrap(), serde_json::json!(true));
        assert_eq!(jsonb(b"\x23\x36\x30").unwrap(), serde_json::json!(60));
        // {"width":1920}
        assert_eq!(
            jsonb(b"\xcc\x0b\x57width\x431920").unwrap(),
            serde_json::json!({"width": 1920})
        );
        assert!(jsonb(b"\xcc\x10\x57wid").is_none());
        assert!(jsonb(b"").is_none());
        assert_eq!(
            Value::Blob(br#"{"a":1}"#.to_vec()).json().unwrap(),
            serde_json::json!({"a": 1})
        );
    }

    #[test]
    fn columns_from_create_table() {
        let sql = "CREATE TABLE t (\n id INTEGER PRIMARY KEY, \"a b\" TEXT DEFAULT (x(1,2)), c, UNIQUE(c))";
        assert_eq!(
            column_defs(sql),
            vec![
                ("id".to_string(), true),
                ("a b".to_string(), false),
                ("c".to_string(), false)
            ]
        );
    }
}
