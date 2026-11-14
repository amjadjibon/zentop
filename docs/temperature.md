# Temperature monitoring

The **Temperature** resource card shows the hottest readable hardware sensor and a two-minute trend on a −40…150°C scale. Open its detail sheet to see individual sensor names, current readings, session minimum/maximum, source, critical limit (when reported), and the selected sensor’s trend. Search the compact sensor list, click a name to inspect it, or enable **Show unavailable sensors** to inspect missing readings. In the TUI, press **7** for details.

All readings use Celsius. A missing, invalid or unsupported reading is **Unavailable**, never zero. CPU, GPU and storage categories remain visible even when no sensor is exposed. Hardware labels are preserved: a PMU sensor is not assumed to be a CPU sensor. The overview can therefore show a battery, storage or other sensor as the hottest reading.

## Availability

Zentop uses system sensors provided by `sysinfo`, together with temperatures from its existing GPU collectors. Availability depends on hardware, OS, drivers and permissions. No privileged helper is installed. On Apple silicon, PMU, NAND and battery sensors may be readable while explicitly identified CPU/GPU temperatures remain unavailable. Linux and Windows likewise depend on what the system and GPU drivers expose; these platforms have not been hardware-tested for this feature.

Minimum/maximum values cover this running sampler session. A sensor that disappears loses its range; a temporarily missing value from a still-present sensor preserves the range. Restarting clears ranges and history. Paused, stale and missing samples leave gaps. The hottest-sensor trend may switch between sensors; individual trends keep the named sensor identity.

Collection retains at most 128 sensor records plus three unavailable category placeholders. Session ranges are bounded by the live sensor list, and history uses the existing two-minute / 160-sample limit. Non-finite values and readings outside −40…150°C are rejected.

## Heat alerts

Enable **Temperature** in **Settings → Alerts**, or press **a** in the TUI. The default threshold is **90°C**, with the shared 30-second sustained duration and five-minute repeat cooldown. The rule starts off. Thresholds are configurable from 1 to 150°C in the shared settings file; GUI presets cover common values and TUI arrows adjust them.

A heat alert requires the same hottest sensor to stay at or above the threshold for the selected duration. A change of hottest sensor, missing readings, pauses or long sampling gaps restarts the hold. Alerts use the existing recent history and optional desktop notification settings. These are user-selected monitoring thresholds, not manufacturer safety limits or fan controls.

## CLI

```sh
cargo run -p zentop-cli -- temperature
cargo run -p zentop-cli -- temperature --json
```

Text output includes current and session minimum/maximum. JSON includes sensor identity, kind, source, current reading, ranges and critical limit, using `null` for unavailable values. `system --json` also includes the sensor list. CLI reports take two samples 1.5 seconds apart; ranges therefore cover only those observations.
