use clap::{Parser, ValueEnum};
use lsm_compaction::{CompactionKind, CompactionStats, Kvs};
use std::collections::BTreeMap;

const BUF_CAPACITY: usize = 1_024;
const COMPACTION_THRESHOLD: usize = 10_000;
const KEY_SPACE: u32 = 10_000;

fn main() {
    let args = Args::parse();
    let compaction_kind = args.compaction.into();
    let mut kvs = Kvs::new_with_compaction(args.buffer, args.compaction_threshold, compaction_kind);
    let mut reference = BTreeMap::new();
    let stream = CommandStream::new(0x1234_5678_9abc_def0, args.key_space);

    println!(
        "config: compaction={:?}, buffer={} entries, compaction_threshold={} sstables, key_space={}",
        compaction_kind, args.buffer, args.compaction_threshold, args.key_space
    );

    for (step, command) in stream.enumerate() {
        match command {
            Command::Put { key, value } => {
                kvs.put(key, value);
                reference.insert(key, value);
            }
            Command::Delete { key } => {
                kvs.delete(key);
                reference.remove(&key);
            }
            Command::Get { key } => {
                let left = kvs.get(key);
                let right = reference.get(&key).copied();

                assert_eq!(
                    left, right,
                    "GET mismatch at step {step}: key={key}, kvs={left:?}, btree={right:?}"
                );
            }
        }

        for stats in kvs.take_compaction_stats() {
            print_compaction(step, stats);
        }
    }
}

fn print_compaction(step: usize, stats: CompactionStats) {
    println!(
        "compaction #{:03} {:?} at step {}: input_entries={}, output_entries={}, elapsed_seconds={:.6}",
        stats.id,
        stats.kind,
        step,
        stats.input_entries,
        stats.output_entries,
        stats.elapsed.as_secs_f64()
    );
}

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, value_enum, default_value_t = CompactionArg::Cpu)]
    compaction: CompactionArg,

    #[arg(long, default_value_t = BUF_CAPACITY)]
    buffer: usize,

    #[arg(long, default_value_t = COMPACTION_THRESHOLD)]
    compaction_threshold: usize,

    #[arg(long, default_value_t = KEY_SPACE)]
    key_space: u32,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CompactionArg {
    Cpu,
    Gpu,
}

impl From<CompactionArg> for CompactionKind {
    fn from(value: CompactionArg) -> Self {
        match value {
            CompactionArg::Cpu => Self::Cpu,
            CompactionArg::Gpu => Self::Gpu,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Command {
    Put { key: u32, value: u64 },
    Get { key: u32 },
    Delete { key: u32 },
}

struct CommandStream {
    rng: Rng,
    key_space: u32,
}

impl CommandStream {
    fn new(seed: u64, key_space: u32) -> Self {
        assert!(key_space > 0);
        Self {
            rng: Rng::new(seed),
            key_space,
        }
    }
}

impl Iterator for CommandStream {
    type Item = Command;

    fn next(&mut self) -> Option<Self::Item> {
        let key = (self.rng.next() % u64::from(self.key_space)) as u32;

        let command = match self.rng.next() % 10 {
            0..=4 => Command::Put {
                key,
                value: self.rng.next(),
            },
            5..=7 => Command::Delete { key },
            _ => Command::Get { key },
        };

        Some(command)
    }
}

struct Rng {
    state: u64,
}

impl Rng {
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
