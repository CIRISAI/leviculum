# lnsd(1)

## NAME

lnsd -- Reticulum network daemon

## SYNOPSIS

**lnsd** [**-c** *dir*] [**-s** *dir*] [**-v**...] [**-q**...]

## DESCRIPTION

**lnsd** runs the Reticulum network stack as a long-lived daemon process. It is a drop-in replacement for Python's **rnsd**. Other programs connect to it via shared instance IPC (Unix abstract socket).

On startup, **lnsd** reads `config` from the configuration directory, opens all configured interfaces, and begins routing packets. It keeps running until it receives SIGINT or SIGTERM.

Sending SIGUSR1 prints a diagnostic dump of internal state to stderr.

## OPTIONS

**-c**, **--config** *dir*
:   Path to the Reticulum configuration directory. Defaults to `~/.reticulum`. The config file is `<dir>/config`.

**-s**, **--storage** *dir*
:   Storage directory path. Defaults to `<config_dir>/storage`.

**-v**, **--verbose**
:   Increase log verbosity. Once for debug, twice for trace.

**-q**, **--quiet**
:   Decrease log verbosity. Once for warnings only, twice for errors only.

## ENVIRONMENT

**RUST_LOG**
:   Overrides the verbosity flags. See the `tracing-subscriber` documentation for filter syntax.

## FILES

*~/.reticulum/config*
:   Default configuration file (INI format, same as Python Reticulum).

*~/.reticulum/storage/*
:   Default storage directory for identities, known destinations, and cached path state.

## SIGNALS

**SIGINT**, **SIGTERM**
:   Graceful shutdown.

**SIGUSR1**
:   Dump diagnostic state to stderr.

## EXAMPLES

Start with default config and verbose logging:

    lnsd -v

Start with a custom config directory:

    lnsd --config /etc/reticulum

## SEE ALSO

**lnstest**(1), **lncp**(1), **lnstatus**(1)
