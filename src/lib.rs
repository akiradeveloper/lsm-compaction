use std::time::{Duration, Instant};

use cubecl::{
    prelude::*,
    wgpu::{WgpuDevice, WgpuRuntime},
};
use massively::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactionKind {
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub key: u32,
    pub seq: u32,
    pub value: Option<u64>,
}

impl Entry {
    pub fn put(key: u32, seq: u32, value: u64) -> Self {
        Self {
            key,
            seq,
            value: Some(value),
        }
    }

    pub fn delete(key: u32, seq: u32) -> Self {
        Self {
            key,
            seq,
            value: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SsTable {
    entries: Vec<Entry>,
}

impl SsTable {
    pub fn new(mut entries: Vec<Entry>) -> Self {
        entries.sort_by_key(|entry| (entry.key, entry.seq));
        Self { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: u32) -> Option<Option<u64>> {
        self.get_entry(key).map(|entry| entry.value)
    }

    fn get_entry(&self, key: u32) -> Option<Entry> {
        let start = self.entries.partition_point(|entry| entry.key < key);
        let mut latest = None;

        for entry in self.entries[start..]
            .iter()
            .take_while(|entry| entry.key == key)
        {
            if latest
                .map(|latest: Entry| entry.seq > latest.seq)
                .unwrap_or(true)
            {
                latest = Some(*entry);
            }
        }

        latest
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactionStats {
    pub id: u64,
    pub kind: CompactionKind,
    pub input_tables: usize,
    pub output_tables: usize,
    pub input_entries: usize,
    pub output_entries: usize,
    pub elapsed: Duration,
}

impl CompactionStats {
    pub fn removed_entries(&self) -> usize {
        self.input_entries.saturating_sub(self.output_entries)
    }

    pub fn entry_reduction_percent(&self) -> f64 {
        if self.input_entries == 0 {
            0.0
        } else {
            self.removed_entries() as f64 * 100.0 / self.input_entries as f64
        }
    }
}

#[derive(Clone, Debug)]
pub struct Kvs {
    buf: Vec<Entry>,
    tables: Vec<SsTable>,
    next_seq: u32,
    buf_capacity: usize,
    compaction_threshold: usize,
    compaction_kind: CompactionKind,
    next_compaction_id: u64,
    compaction_stats: Vec<CompactionStats>,
}

impl Default for Kvs {
    fn default() -> Self {
        Self::new(64, 4)
    }
}

impl Kvs {
    pub fn new(buf_capacity: usize, compaction_threshold: usize) -> Self {
        Self::new_with_compaction(buf_capacity, compaction_threshold, CompactionKind::Cpu)
    }

    pub fn new_with_compaction(
        buf_capacity: usize,
        compaction_threshold: usize,
        compaction_kind: CompactionKind,
    ) -> Self {
        assert!(buf_capacity > 0);
        assert!(compaction_threshold > 0);

        Self {
            buf: Vec::with_capacity(buf_capacity),
            tables: Vec::new(),
            next_seq: 0,
            buf_capacity,
            compaction_threshold,
            compaction_kind,
            next_compaction_id: 0,
            compaction_stats: Vec::new(),
        }
    }

    pub fn get(&self, key: u32) -> Option<u64> {
        for entry in self.buf.iter().rev() {
            if entry.key == key {
                return entry.value;
            }
        }

        for table in self.tables.iter().rev() {
            if let Some(entry) = table.get_entry(key) {
                return entry.value;
            }
        }

        None
    }

    pub fn put(&mut self, key: u32, value: u64) {
        let seq = self.alloc_seq();
        self.push_entry(Entry::put(key, seq, value));
    }

    pub fn delete(&mut self, key: u32) {
        let seq = self.alloc_seq();
        self.push_entry(Entry::delete(key, seq));
    }

    pub fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }

        let table = SsTable::new(std::mem::take(&mut self.buf));
        self.tables.push(table);

        if self.tables.len() >= self.compaction_threshold {
            self.compact();
        }
    }

    pub fn compact(&mut self) -> Option<CompactionStats> {
        if self.tables.is_empty() {
            return None;
        }

        let input_tables = self.tables.len();
        let input_entries = self.tables.iter().map(SsTable::len).sum();
        let tables = std::mem::take(&mut self.tables);
        let started = Instant::now();
        let output = compact_with_kind(tables, self.compaction_kind);
        let elapsed = started.elapsed();
        let output_entries = output.len();

        self.tables.push(output);

        let stats = CompactionStats {
            id: self.next_compaction_id,
            kind: self.compaction_kind,
            input_tables,
            output_tables: self.tables.len(),
            input_entries,
            output_entries,
            elapsed,
        };

        self.next_compaction_id += 1;
        self.compaction_stats.push(stats);
        Some(stats)
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    pub fn take_compaction_stats(&mut self) -> Vec<CompactionStats> {
        std::mem::take(&mut self.compaction_stats)
    }

    fn alloc_seq(&mut self) -> u32 {
        let seq = self.next_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .expect("u32 sequence number exhausted");
        seq
    }

    fn push_entry(&mut self, entry: Entry) {
        self.buf.push(entry);

        if self.buf.len() >= self.buf_capacity {
            self.flush();
        }
    }
}

pub fn compact(tables: Vec<SsTable>) -> SsTable {
    compact_cpu(tables)
}

pub fn compact_with_kind(tables: Vec<SsTable>, kind: CompactionKind) -> SsTable {
    match kind {
        CompactionKind::Cpu => compact_cpu(tables),
        CompactionKind::Gpu => compact_gpu(tables),
    }
}

fn compact_cpu(tables: Vec<SsTable>) -> SsTable {
    let input_entries: usize = tables.iter().map(SsTable::len).sum();
    let mut entries = Vec::with_capacity(input_entries);

    for table in tables {
        entries.extend(table.entries);
    }

    entries.sort_by(|left, right| {
        left.key
            .cmp(&right.key)
            .then_with(|| right.seq.cmp(&left.seq))
    });

    let mut compacted = Vec::new();
    let mut last_key = None;

    for entry in entries {
        let key = entry.key;
        if last_key != Some(key) {
            if entry.value.is_some() {
                compacted.push(entry);
            }
            last_key = Some(key);
        }
    }

    SsTable::new(compacted)
}

fn compact_gpu(tables: Vec<SsTable>) -> SsTable {
    compact_gpu_inner(tables).expect("massively GPU compaction failed")
}

fn compact_gpu_inner(tables: Vec<SsTable>) -> Result<SsTable, massively::Error> {
    let input_entries: usize = tables.iter().map(SsTable::len).sum();

    if input_entries == 0 {
        return Ok(SsTable::default());
    }

    // Convert the CPU-friendly entries into SoA columns for GPU algorithms.
    let mut keys = Vec::with_capacity(input_entries);
    let mut seqs = Vec::with_capacity(input_entries);
    let mut values = Vec::with_capacity(input_entries);
    let mut is_deleted = Vec::with_capacity(input_entries);

    for table in tables {
        for entry in table.entries {
            keys.push(entry.key);
            seqs.push(entry.seq);
            values.push(entry.value.unwrap_or(0));
            is_deleted.push(u32::from(entry.value.is_none()));
        }
    }

    let exec = Executor::<WgpuRuntime>::new(WgpuDevice::DefaultDevice);
    let keys = exec.to_device(&keys);
    let seqs = exec.to_device(&seqs);
    let values = exec.to_device(&values);
    let is_deleted = exec.to_device(&is_deleted);

    // Sort by (key ascending, seq descending), so the newest entry for each key
    // becomes the first element in that key's run.
    let sorted_keys = exec.alloc::<u32>(input_entries);
    let sorted_key_seqs = exec.alloc::<u32>(input_entries);
    let sorted_seqs = exec.alloc::<u32>(input_entries);
    let sorted_values = exec.alloc::<u64>(input_entries);
    let sorted_is_deleted = exec.alloc::<u32>(input_entries);

    massively::sort_by_key(
        &exec,
        zip2(keys.slice(..), seqs.slice(..)),
        zip3(seqs.slice(..), values.slice(..), is_deleted.slice(..)),
        EntryLess,
        zip2(sorted_keys.slice_mut(..), sorted_key_seqs.slice_mut(..)),
        zip3(
            sorted_seqs.slice_mut(..),
            sorted_values.slice_mut(..),
            sorted_is_deleted.slice_mut(..),
        ),
    )?;

    // Keep only the first entry for each key. After the sort above, that entry
    // is the visible state of the key.
    let unique_keys = exec.alloc::<u32>(input_entries);
    let unique_seqs = exec.alloc::<u32>(input_entries);
    let unique_values = exec.alloc::<u64>(input_entries);
    let unique_is_deleted = exec.alloc::<u32>(input_entries);

    let unique_len = massively::unique_by_key(
        &exec,
        sorted_keys.slice(..),
        zip3(
            sorted_seqs.slice(..),
            sorted_values.slice(..),
            sorted_is_deleted.slice(..),
        ),
        EqualU32,
        unique_keys.slice_mut(..),
        zip3(
            unique_seqs.slice_mut(..),
            unique_values.slice_mut(..),
            unique_is_deleted.slice_mut(..),
        ),
    )?;
    let unique_len = unique_len as usize;

    // Drop tombstones. If the latest entry for a key was a delete, the key is
    // absent from the compacted table.
    let output_keys = exec.alloc::<u32>(input_entries);
    let output_seqs = exec.alloc::<u32>(input_entries);
    let output_values = exec.alloc::<u64>(input_entries);
    let output_is_deleted = exec.alloc::<u32>(input_entries);

    let output_len = massively::remove_where(
        &exec,
        zip4(
            unique_keys.slice(..unique_len),
            unique_seqs.slice(..unique_len),
            unique_values.slice(..unique_len),
            unique_is_deleted.slice(..unique_len),
        ),
        unique_is_deleted.slice(..unique_len),
        zip4(
            output_keys.slice_mut(..),
            output_seqs.slice_mut(..),
            output_values.slice_mut(..),
            output_is_deleted.slice_mut(..),
        ),
    )?;
    let output_len = output_len as usize;

    // Return the compacted entries to the CPU-side SSTable representation.
    let output_keys_host = exec.to_host(&output_keys.slice(..output_len))?;
    let output_seqs_host = exec.to_host(&output_seqs.slice(..output_len))?;
    let output_values_host = exec.to_host(&output_values.slice(..output_len))?;

    let entries = output_keys_host
        .into_iter()
        .zip(output_seqs_host)
        .zip(output_values_host)
        .map(|((key, seq), value)| Entry::put(key, seq, value))
        .collect();

    Ok(SsTable::new(entries))
}

struct EntryLess;

#[cubecl::cube]
impl massively::op::BinaryPredicateOp<(u32, u32)> for EntryLess {
    fn apply(lhs: (u32, u32), rhs: (u32, u32)) -> bool {
        lhs.0 < rhs.0 || (lhs.0 == rhs.0 && lhs.1 > rhs.1)
    }
}

struct EqualU32;

#[cubecl::cube]
impl massively::op::BinaryPredicateOp<u32> for EqualU32 {
    fn apply(lhs: u32, rhs: u32) -> bool {
        lhs == rhs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn compact_keeps_latest_put_and_drops_deleted_keys() {
        let tables = vec![
            SsTable::new(vec![
                Entry::put(1, 0, 10),
                Entry::put(2, 1, 20),
                Entry::put(1, 2, 11),
            ]),
            SsTable::new(vec![
                Entry::delete(2, 3),
                Entry::put(3, 4, 30),
                Entry::put(1, 5, 12),
            ]),
        ];

        let compacted = compact(tables);

        assert_eq!(
            compacted.entries(),
            &[Entry::put(1, 5, 12), Entry::put(3, 4, 30)]
        );
    }

    #[test]
    fn kvs_matches_btree_map_for_pseudorandom_commands() {
        let mut kvs = Kvs::new(17, 3);
        let mut reference = BTreeMap::new();
        let mut rng = TestRng::new(0xc001_cafe_f00d_babe);

        for _ in 0..10_000 {
            let key = (rng.next() % 257) as u32;

            match rng.next() % 10 {
                0..=4 => {
                    let value = rng.next();
                    kvs.put(key, value);
                    reference.insert(key, value);
                }
                5..=7 => {
                    kvs.delete(key);
                    reference.remove(&key);
                }
                _ => {
                    assert_eq!(kvs.get(key), reference.get(&key).copied());
                }
            }
        }

        for key in 0..257 {
            assert_eq!(kvs.get(key), reference.get(&key).copied());
        }
    }

    struct TestRng {
        state: u64,
    }

    impl TestRng {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            self.state
        }
    }

    #[test]
    #[ignore = "requires a WGPU device"]
    fn gpu_compaction_matches_cpu_for_small_input() {
        let tables = vec![
            SsTable::new(vec![
                Entry::put(1, 0, 10),
                Entry::put(2, 1, 20),
                Entry::put(1, 2, 11),
            ]),
            SsTable::new(vec![
                Entry::delete(2, 3),
                Entry::put(3, 4, 30),
                Entry::put(1, 5, 12),
            ]),
        ];

        let cpu = compact_with_kind(tables.clone(), CompactionKind::Cpu);
        let gpu = compact_with_kind(tables, CompactionKind::Gpu);

        assert_eq!(gpu, cpu);
    }

    #[test]
    #[ignore = "requires a WGPU device"]
    fn gpu_compaction_matches_reference_for_repeated_keys() {
        let mut rng = TestRng::new(0x1234_5678_9abc_def0);
        let mut reference = BTreeMap::new();
        let mut tables = Vec::new();
        let mut seq = 0;

        for _ in 0..1024 {
            let mut entries = Vec::with_capacity(1024);

            for _ in 0..1024 {
                let key = (rng.next() % 10_000) as u32;

                match rng.next() % 8 {
                    0..=4 => {
                        let value = rng.next();
                        entries.push(Entry::put(key, seq, value));
                        reference.insert(key, value);
                    }
                    _ => {
                        entries.push(Entry::delete(key, seq));
                        reference.remove(&key);
                    }
                }

                seq += 1;
            }

            tables.push(SsTable::new(entries));
        }

        let gpu = compact_with_kind(tables, CompactionKind::Gpu);
        let actual = gpu
            .entries()
            .iter()
            .map(|entry| {
                let value = entry
                    .value
                    .expect("compacted output must not contain deletes");
                (entry.key, value)
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            actual.len(),
            gpu.len(),
            "compacted output has duplicate keys"
        );
        assert_eq!(actual, reference);
    }
}
