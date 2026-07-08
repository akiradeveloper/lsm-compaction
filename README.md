# tiny-lsm

`tiny-lsm` is a small demo of LSM compaction running on the GPU with
[`massively`](https://github.com/akiradeveloper/massively).

The goal is not to build a full production LSM tree. The goal is to make the
core compaction step small, readable, and executable: take many in-memory
SSTables, compact them on the GPU, and compare the resulting key-value store
against `BTreeMap` on an infinite stream of random commands.

All data lives in memory, and the LSM has only one level.

## What This Demonstrates

The central operation is:

```text
Vec<SsTable> -> SsTable
```

Compaction merges many SSTables containing PUT and DELETE entries into one
SSTable. If a key appears multiple times, only the entry with the newest
sequence number remains. If that newest entry is a DELETE tombstone, the key is
removed from the compacted SSTable.

The GPU implementation uses `massively` primitives in this shape:

```text
sort_by_key((key asc, seq desc))
-> unique_by_key(key)
-> remove_where(is_deleted)
```

The CPU-side KVS intentionally uses a simple representation for readability.
SSTables are stored as `Vec<Entry>`. During GPU compaction, entries are converted
to a structure-of-arrays layout, processed on the GPU, and converted back to the
CPU-side SSTable representation.

## Running

CPU compaction:

```bash
cargo run --release -- --compaction=cpu
```

GPU compaction:

```bash
cargo run --release -- --compaction=gpu
```

The program generates an infinite stream of random PUT/GET/DELETE commands and
checks `tiny-lsm` against `BTreeMap`. A GET mismatch causes a panic.

Every compaction prints the input size, output size, and elapsed time:

```text
config: compaction=Gpu, buffer=1024 entries, compaction_threshold=10000 sstables, key_space=10000
compaction #000 Gpu at step 12800435: input_entries=10240000, output_entries=6233, elapsed_seconds=...
```

The program runs forever. Stop it with `Ctrl-C`.

## Options

```bash
cargo run --release -- --help
```

Main options:

- `--compaction=cpu|gpu`: choose the compaction implementation.
- `--buffer <N>`: number of entries in the memtable buffer. When it fills, it is flushed as an SSTable.
- `--compaction-threshold <N>`: compact when this many SSTables have accumulated.
- `--key-space <N>`: key range used by the random command stream.

## Implementation Notes

- Keys are `u32`, values are `u64`.
- Each entry stores `key`, `seq`, and `value`.
- DELETE is represented as a tombstone with `value = None`.
- `Kvs` creates a new SSTable whenever the buffer fills.
- When the SSTable count reaches the threshold, all SSTables are compacted into one.
- The GPU implementation uses `massively v0.59`.

## Tests

Run the normal test suite:

```bash
cargo test
```

Run a small GPU compaction test:

```bash
cargo test gpu_compaction_matches_cpu_for_small_input -- --ignored
```

The GPU test expects an environment where a WGPU device is available.
