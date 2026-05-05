# cgn-power

[![crates.io](https://img.shields.io/crates/v/cgn-power.svg)](https://crates.io/crates/cgn-power)
[![docs.rs](https://docs.rs/cgn-power/badge.svg)](https://docs.rs/cgn-power)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Power readers feeding Cognitora's energy-aware scheduling.

Three sources, in preference order:

1. **Redfish** — vendor-neutral DC out-of-band API, reports total chassis
   power and per-PSU draw. Best signal when available.
2. **IPMI** — `ipmitool sdr` parsed by a small shell-out helper. Used as
   a fallback when Redfish isn't reachable.
3. **NVML / DCGM** — per-GPU power draw via `nvml-wrapper`. Always read
   when an NVIDIA GPU is present, blended with the chassis number to
   derive `gpu_share`.

[`cgn-metrics`](https://crates.io/crates/cgn-metrics) polls these readers
on a configurable interval and exports `cgn_power_watts{component=...}`
plus derived gauges that the router consumes through its `power` score
component.

## Use

```toml
[dependencies]
cgn-power = "0.1"
```

```rust
use cgn_power::{redfish, nvml, PowerReader};

let r = redfish::RedfishReader::new(&cfg.power.redfish)?;
let sample = r.read().await?;
println!("{} W on {}", sample.watts, sample.component);
```

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
