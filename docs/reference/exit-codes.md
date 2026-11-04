# Exit codes

All Cognitora binaries follow the same exit-code convention so
systemd, Kubernetes, and shell pipelines can react sensibly.

| Code | Meaning            | Examples                                                 |
|-----:|--------------------|----------------------------------------------------------|
| `0`  | clean exit         | `SIGTERM` or Ctrl-C handled, in-flight work drained or timed out |
| `1`  | any failure        | config error, port in use, TLS error, runtime error      |
| `2`  | bad invocation     | unknown flag (from `clap`)                               |

Signals that kill a process before it installs its handlers give the usual
`128 + n`. The cause of a failure is in the log, not the exit code.

`cgn_core::exit_code` maps errors to finer codes, but no binary calls it
yet, so don't branch on them. Exit code `10` is reserved in the shipped
systemd units and is never produced.

## How to use this in systemd

The shipped units already declare:

```ini
Restart=on-failure
SuccessExitStatus=10 130 143
```

so a clean drain (`0`) or a `systemctl stop` won't trigger a restart, but a real crash (`1`) or a config
error (`3`) will.

## How to use this in Kubernetes

The Helm chart's Deployment spec uses
`terminationMessagePolicy: FallbackToLogsOnError` so a non-zero exit
plus the last log line lands in `kubectl describe pod`. Combined
with the table above this gives a one-glance diagnosis.

## Tests

`rust/libraries/cgn-core/tests/exit_codes.rs` exercises the matrix
above against `cgn_core::exit_code(&Error)`. Every binary's `main()`
funnels its returned `Error` through that function before exiting, so
the systemd `SuccessExitStatus` lists and the Kubernetes runbook stay
in lockstep with the table above.
