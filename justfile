# DEVICE = cpu | gpu
run DEVICE:
  cargo run --release -- --compaction={{DEVICE}}
  