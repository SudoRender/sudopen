//! CSV/TSV as a table: a record index plus on-demand parsing.
//!
//! Records can span lines (quoted newlines), so the line index cannot be
//! reused. The index keeps the byte offset of every `CHUNK`th record and is
//! built in the background for big files; rows are parsed a chunk at a time
//! when the table asks for them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use csv::{ByteRecord, ReaderBuilder};

use crate::source::Source;

pub const CHUNK: u64 = 64;
const SYNC_LIMIT: u64 = 4 << 20;

#[derive(Default)]
struct State {
    /// `starts[k]` = byte offset of record `k * CHUNK`.
    starts: Vec<u64>,
    records: u64,
    max_fields: usize,
    done: bool,
}

pub struct Table {
    source: Arc<Source>,
    pub delimiter: u8,
    state: Mutex<State>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableProgress {
    /// Records seen so far, header included.
    pub records: u64,
    pub max_fields: usize,
    pub done: bool,
}

impl Table {
    /// Opens `source` as a table. `tsv` forces tabs; otherwise the delimiter
    /// is guessed from the first line.
    pub fn new(source: Arc<Source>, tsv: bool, cancel: Arc<AtomicBool>) -> Arc<Table> {
        let delimiter = if tsv { b'\t' } else { sniff(source.bytes()) };
        let table = Arc::new(Table {
            source,
            delimiter,
            state: Mutex::new(State { starts: vec![0], ..Default::default() }),
        });
        if table.source.len() <= SYNC_LIMIT {
            table.build(&cancel);
        } else {
            let t = table.clone();
            std::thread::Builder::new()
                .name("sudopen-csv".into())
                .spawn(move || t.build(&cancel))
                .ok();
        }
        table
    }

    fn reader<'a>(&self, bytes: &'a [u8]) -> csv::Reader<&'a [u8]> {
        ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .delimiter(self.delimiter)
            .from_reader(bytes)
    }

    fn build(&self, cancel: &AtomicBool) {
        let bytes = self.source.bytes();
        let mut rdr = self.reader(bytes);
        let mut rec = ByteRecord::new();
        let mut records = 0u64;
        let mut max_fields = 0usize;
        let mut pending = Vec::new();
        loop {
            let start = rdr.position().byte();
            match rdr.read_byte_record(&mut rec) {
                Ok(true) => {}
                Ok(false) => break,
                // Malformed input: stop indexing; what was read stays usable.
                Err(_) => break,
            }
            if records > 0 && records.is_multiple_of(CHUNK) {
                pending.push(start);
            }
            records += 1;
            max_fields = max_fields.max(rec.len());
            if records.is_multiple_of(CHUNK * 16) {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let mut st = self.state.lock().unwrap();
                st.starts.append(&mut pending);
                st.records = records;
                st.max_fields = max_fields;
            }
        }
        let mut st = self.state.lock().unwrap();
        st.starts.append(&mut pending);
        st.records = records;
        st.max_fields = max_fields;
        st.done = true;
    }

    pub fn progress(&self) -> TableProgress {
        let st = self.state.lock().unwrap();
        TableProgress { records: st.records, max_fields: st.max_fields, done: st.done }
    }

    /// Records `k * CHUNK ..` (up to `CHUNK` of them), as strings. Chunk 0
    /// is always available, even before indexing has published anything.
    pub fn chunk(&self, k: u64) -> Option<Vec<Vec<String>>> {
        let start = *self.state.lock().unwrap().starts.get(k as usize)?;
        let bytes = self.source.bytes();
        let mut rdr = self.reader(&bytes[start as usize..]);
        let mut rec = ByteRecord::new();
        let mut rows = Vec::with_capacity(CHUNK as usize);
        while (rows.len() as u64) < CHUNK && rdr.read_byte_record(&mut rec).unwrap_or(false) {
            rows.push(rec.iter().map(|f| String::from_utf8_lossy(f).into_owned()).collect());
        }
        Some(rows)
    }
}

/// Picks the most frequent of `,` `;` `\t` `|` in the first line.
fn sniff(bytes: &[u8]) -> u8 {
    let line = &bytes[..memchr::memchr(b'\n', bytes).unwrap_or(bytes.len()).min(64 * 1024)];
    let mut best = (b',', 0usize);
    for d in *b",;\t|" {
        let n = memchr::memchr_iter(d, line).count();
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Arc<Table> {
        let src = Arc::new(Source::from_bytes("t.csv".into(), text.as_bytes().to_vec()));
        Table::new(src, false, Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn indexes_and_reads_chunks() {
        let mut text = String::from("id,name\n");
        for i in 0..200 {
            text.push_str(&format!("{i},\"item\n{i}\"\n"));
        }
        let t = table(&text);
        let p = t.progress();
        assert_eq!((p.records, p.max_fields, p.done), (201, 2, true));
        assert_eq!(t.chunk(0).unwrap()[0], vec!["id", "name"]);
        let c2 = t.chunk(2).unwrap();
        assert_eq!(c2[0], vec!["127", "item\n127"]);
        assert_eq!(t.chunk(3).unwrap().len(), 201 - 192);
    }

    #[test]
    fn sniffs_delimiters() {
        assert_eq!(sniff(b"a;b;c\n1;2;3"), b';');
        assert_eq!(sniff(b"a,b\n"), b',');
        assert_eq!(sniff(b"a\tb\tc"), b'\t');
    }
}
