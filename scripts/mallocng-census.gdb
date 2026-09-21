# A per-size-class census of musl's mallocng, read out of a running
# heap-gap-bench.
#
# Why this exists: `live_bytes` says what the program asked for and
# /proc/self/statm says what the kernel charges us, and neither can say
# WHICH size class the difference sits in. mallocng keeps that in
# `struct malloc_context ctx` and in its meta areas, and a musl-static
# binary carries both as local symbols, so gdb can read them without any
# instrumentation in our code. This is what named the 2026-09-21 step:
# 17 933 live objects moved from the 192-byte class to the 240-byte class
# in one window and mallocng charged 3.7 MB of resident set for it.
#
# Usage (see scripts/mallocng-census.py for decoding the dumps):
#
#   mkdir -p /tmp/mallocng-census && rm -f /tmp/mallocng-census/*
#   gdb -batch -x scripts/mallocng-census.gdb \
#       --args target/x86_64-unknown-linux-musl/release/heap-gap-bench \
#       --repeats 11 --tick-every 500 --progress 2000
#   scripts/mallocng-census.py /tmp/mallocng-census <binary>
#
# It stops at every `HEAPGAP_PROGRESS` line (heap-gap-bench calls
# `map_count` exactly once per progress line and once more for the final
# result line) and dumps `ctx.usage_by_class` and `ctx.bounces` every
# time; every `$dump_every`-th stop it also dumps the raw meta areas, from
# which the decoder recovers per-group occupancy. Set `$dump_every` before
# sourcing to change the stride.
#
# The binary must NOT carry debug info: with DWARF present gdb resolves
# the hidden `__malloc_context` against the current frame's unit and
# reads zeros. `cargo build --release` (the profile strips debuginfo)
# gives a binary this works on; `CARGO_PROFILE_RELEASE_STRIP=none` does
# not. Offsets below are the x86_64 layout of musl 1.2.5's
# `struct malloc_context` (src/malloc/mallocng/meta.h); the decoder
# refuses to run if the symbol's size is no longer 0x3a0, which is what
# would change if the layout did.

set pagination off
set confirm off
set print elements 0
init-if-undefined $dump_every = 10
set $stage = 0

# The crate disambiguator in the mangled name moves with every build, so
# match on the demangled name rather than on the mangled symbol.
rbreak ^heap_gap_bench::map_count$

commands
silent
set $stage = $stage + 1
printf "CTXUSAGE stage=%d", $stage
set $i = 0
while $i < 48
  printf " %lu", *(unsigned long *)((char *)&__malloc_context + 464 + $i*8)
  set $i = $i + 1
end
printf "\n"
printf "CTXBOUNCE stage=%d", $stage
set $i = 0
while $i < 32
  printf " %u", *(unsigned char *)((char *)&__malloc_context + 880 + $i)
  set $i = $i + 1
end
printf "\n"
if $stage % $dump_every == 0
  set $ma = *(unsigned long *)((char *)&__malloc_context + 56)
  set $n = 0
  while $ma != 0
    eval "dump binary memory /tmp/mallocng-census/s%d_%d.bin %lu %lu", $stage, $n, $ma, $ma+4096
    set $n = $n + 1
    set $ma = *(unsigned long *)($ma + 8)
  end
  printf "CTXAREAS stage=%d areas=%d\n", $stage, $n
end
continue
end

run
quit
