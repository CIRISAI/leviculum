# lnsd(1)

## NAME

lnsd -- Reticulum network daemon

## SYNOPSIS

**lnsd** [**-c** *dir*] [**--storage** *dir*] [**-s**] [**--exampleconfig**] [**-v**...] [**-q**...]

## DESCRIPTION

**lnsd** runs the Reticulum network stack as a long-lived daemon process. It is a drop-in replacement for Python's **rnsd**: it loads the configs **rnsd** loads, serves the same shared instance IPC (Unix abstract sockets keyed by `instance_name`) with the same RPC handshake and key, including a configured `rpc_key`, and the Python client tools (**rnstatus**, **rnpath**, **rnprobe**, **rncp**) work against it unchanged.

The drop-in does not hold in one place, where the same config file gives a different daemon (docs/src/audits/2026-10-11-shared-instance-rpc.md, D12). A config that omits `share_instance` gets no shared instance under **lnsd** (default `false`, **rnsd** `true`); one that omits `enable_transport` gets transport under **lnsd** (default `true`, **rnsd** `false`); an interface section without `enabled` is live under **lnsd** and skipped by **rnsd**. Configs written by `lnsd --exampleconfig` set the three defaulted keys explicitly.

On startup, **lnsd** reads `config` from the configuration directory, opens all configured interfaces, and begins routing packets. It keeps running until it receives SIGINT or SIGTERM.

Sending SIGUSR1 prints a diagnostic dump of internal state to stderr.

## OPTIONS

**-c**, **--config** *dir*
:   Path to the Reticulum configuration directory, the way `rnsd --config` takes one. The config file is `<dir>/config`. Without this option the default lookup order applies; see FILES.

**--storage** *dir*
:   Storage directory path. Overrides the config file's `storage_path`, which in turn overrides the default `<config_dir>/storage`. It moves the daemon alone — the client tools read `storage_path` from the config — so a storage directory shared with `lnstatus`, `lncp`, `lnpath` or `lnprobe` belongs in the config file. Long-only on purpose: in `rnsd`, `-s` means `--service`, so the short letter stays reserved for that and the storage override is a Leviculum extension.

**-s**, **--service**
:   Declare that the daemon is running as a service, accepted for compatibility with `rnsd -s`. **lnsd** keeps logging to standard output, which journald captures; it does not redirect to a log file.

**--exampleconfig**
:   Print an example configuration to standard output and exit, like `rnsd --exampleconfig`. The output loads through **lnsd**'s own config loader.

**-v**, **--verbose**
:   Increase log verbosity. Once for debug, twice for trace.

**-q**, **--quiet**
:   Decrease log verbosity. Once for warnings only, twice for errors only.

## ENVIRONMENT

**RUST_LOG**
:   Overrides the verbosity flags. See the `tracing-subscriber` documentation for filter syntax.

## FILES

*/etc/reticulum/config*
:   System-wide configuration, used when it exists. The Debian package installs one here, which is also what lets Python clients find the running daemon without extra flags.

*~/.config/reticulum/config*
:   Per-user configuration, used when the system-wide file is absent.

*~/.reticulum/config*
:   Final fallback (INI format, same as Python Reticulum).

*<config_dir>/storage/*
:   Storage directory for identities, known destinations, and cached path state.

The three configuration directories are tried in that order, matching Python-Reticulum's own lookup.

## SIGNALS

**SIGINT**, **SIGTERM**
:   Graceful shutdown.

**SIGUSR1**
:   Dump diagnostic state to stderr; see DIAGNOSTIC DUMP.

## DIAGNOSTIC DUMP

The SIGUSR1 dump lists the estimated size of every tracked table, the `=== Total estimated ===` line, the process RSS, and three sections that say where the rest of the memory sits.

`=== Allocator ===` prints glibc's own balance sheet (`mallinfo2`: arena, ordblks, hblks, hblkhd, uordblks, fordblks, keepcost) and the thread count. The shipped musl-static builds run musl's mallocng, which keeps no such totals, so there the section says it is unavailable and only the thread count is measured.

`=== Mappings ===` buckets the process's anonymous writable mappings by size (under 64 KiB, 64 KiB to 1 MiB, 1 to 8 MiB, 8 to 64 MiB, above) with count and bytes per bucket, then names the ten largest by address. Thread stacks show up as regions of about 2 MiB each, so a growing count in one bucket points at an owner class without a debugger.

The `gap:` line starts from RSS plus swap and subtracts the tracked total, the allocator's free bytes (fordblks + keepcost) and its mmapped bytes (hblkhd) in turn, printing what is left after each. A term the libc cannot report is printed as `n/a` and subtracts nothing; hblkhd counts live large allocations, so it can overlap the tracked total.

## EXAMPLES

Start with default config and verbose logging:

    lnsd -v

Start with a custom config directory:

    lnsd --config /etc/reticulum

## SEE ALSO

**lnstest**(1), **lncp**(1), **lnstatus**(1)
