# Configuration

Leviculum reads the same INI-style configuration file as Python Reticulum, located at `~/.reticulum/config` by default.

Use `--config` to specify an alternative configuration directory.

## Example

```ini
[reticulum]
  enable_transport = yes
  share_instance = yes

[interfaces]
  [[Default Interface]]
    type = AutoInterface
    enabled = yes
```

See the [Reticulum documentation](https://reticulum.network/manual/) for the full configuration reference. Leviculum accepts the same options.

## Leviculum-specific options

These keys are ignored by Python Reticulum and optional in Leviculum:

```ini
[reticulum]
  # Seconds between periodic storage flushes (crash protection only,
  # normal shutdown always flushes). Default: 3600. Battery-powered or
  # SD-card deployments may want a longer or shorter interval.
  flush_interval = 3600
```
