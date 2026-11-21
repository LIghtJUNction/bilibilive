# Validation record — 2026-11-21

This record identifies what was actually exercised and which source snapshot
each result covers. The machine-readable
[validation receipt](evidence/validation-2026-11-21.json) contains commands,
lock and source digests, screenshot hashes and the four background samples.
The [dependency receipt](evidence/dependencies-2026-11-21.json) records the public
Project Z default heads and the stable versions returned by crates.io.

## Update and component GUI fixtures

The GUI update work based on `6a680149` passed
`cargo test --locked --lib gui::tests` in the Linux sandbox: 21 tests, including
11 new update/component controller fixtures. The default GUI feature compiled
and linked the real zgpui native C/OpenGL backend and tray implementation. The
tests exercised request/response state without opening a window or performing
an actual download, installation, OBS action or broadcast. The
[GUI receipt](evidence/gui-components-2026-11-21.json) identifies the tested
source hash and dependency lock.

The new cases cover one startup check, saved/command-line suppression,
read-only and demo boundaries, staged update identity, explicit download and
apply, progress polling, failed apply without quitting or reconnecting retired
media, and opt-in plugin replacement. File-installation labels explicitly
distinguish the state from OBS module loading.

The subsequent lifecycle hook uses the integrated App's update-exit signal for
HTTP/MCP-triggered restarts and stops unstarted UI requests during shutdown.
The earlier 21 tests are not attributed to these later changes. The merged
runtime gate below covers the final lifecycle, plugin-status freshness and
main argument-parsing changes. The new page still needs its own native window
review; the historical screenshots below retain their original snapshot
attribution.

## Integrated update and component gate

Commit `535d6f36a440d5e34d6747f8f88405cfc87b5fe5` passed
`cargo test --locked --all-targets` with the default GUI feature in the Linux
sandbox: 100 library tests, 3 CLI tests and 13 OBS component installation
fixtures, for 116 tests in total. The library tests include 22 GUI controller
cases. The actual zgpui native C/OpenGL backend linked against Debian
development and runtime libraries; no window was opened for this test gate.
The [integration receipt](evidence/update-integration-2026-11-21.json) records
the exact source commit, lock and log hashes.

These tests cover the shared update lifecycle, terminal zuse receipts,
retrying a successfully launched apply after a lost reply, acknowledgement
after MCP output, and a contended diagnostics lock during shutdown. OBS
installation tests use private local fixtures. This gate does not establish
Windows/macOS installer upgrades, corresponding OBS loading, actual account
permissions, broadcasts or two-party PK media; those require their own native
or online receipts.

## Final native publication gate

Commit `ae04b1640ab3a78bba3a5a000c1eea551a55c5dc`, using the final dependency
lock, passed the following command on the Linux development host:

```sh
python scripts/validate.py --release --obs-plugin
```

The command exited successfully after format checking, all-target/all-feature
checking, 65 library and 2 CLI tests, the headless check, the default native
release build and the OBS plugin build. The all-feature check and tests include
`linux-software`; this is build and test evidence, not a screenshot of that
rendering backend.

The same runtime also passed an independent installation-prefix smoke check:
the operation catalogue contained 94 operations, the read-only MCP catalogue
contained 35 tools, stdio MCP initialization succeeded, a three-step zuse
program completed, and the local HTTP API accepted its authenticated request
while refusing an anonymous request. No actual broadcast was started.

This record attributes those results to `ae04b164`. The following release
revision changes only the Windows workflow's shell selection and validation
documentation; its Rust runtime and dependency lock remain identical. It does
not claim that those 67 tests were executed a second time for a documentation
change.

## Dependency refresh and headless tests

`cargo update` refreshed five Project Z sources to their public default branches:
zconv 0.4.1, zcall 0.4.1, zmcpio 0.5.0, zuse-computer 0.4.0 and zgpui 0.1.6.
Development declarations remain pure Git dependencies. The observed latest
stable versions of all 16 direct external dependencies match their declared
lower bounds; `libc` was tightened from `0.2` to `0.2.191` without changing its
already-current locked version.

The sandbox ran these commands against the refreshed dependency graph:

```sh
cargo fmt --all -- --check
cargo test --locked --no-default-features --all-targets --no-fail-fast
```

Both commands succeeded. The test command passed 57 library tests and 2 CLI
tests, including all five `zuse_host` tests. Those tests cover whole-program
preflight, imported functions and repeats, typed parameters, cancellation,
partial failure receipts and redaction. Real loopback HTTP and OBS fixtures
also passed. This particular run reported no permission errors; it did not
compile or exercise the optional desktop module.

The receipt records the exact tested lock and a digest of `src/**`. The final
lock then moved zuse from `a5b645f` to `0016f96` after its default branch received
one documentation correction. A Git comparison found only
`docs/development.md` changed; the source tree, manifest and upstream lock
objects are identical. `cargo update -p zuse-computer` changed only that
package's locked commit. The receipt records this adjustment separately and
does not present it as a new test run.

## Linux native snapshot

The earlier native run used the source overlays and dependency-lock digest
listed in the receipt. Its commands were:

```sh
cargo test --locked --all-targets
cargo build --locked
bilibilive call rtc.self-test --args '{}' --json
cmake --build dist/obs-plugin --parallel 4
```

The default desktop build and all 65 library plus 2 CLI tests passed. The local
RTC check connected two libdatachannel peers and exercised ICE, DTLS, SCTP data
channel 101, Protobuf request/reply and SRTP/RTP traffic. Separate
`rtc.encoder.probe` calls used 128 × 128, 10 fps, eight-frame synthetic input.
H.264 returned 39 RTP packets in 70 ms; AV1 returned 33 packets in 403 ms.
These timings include process startup and reception and establish encoder
availability for that input, not sustained production throughput.

### Window and image rendering

An isolated 1280 × 900 Xvfb session contained the entire 1160 × 800 application
window. Captures used the observed window origin, so the reviewed images include
all four window edges. The overview and room screenshots show Chinese glyphs
and were manually inspected without observed text or layout overflow.

The mapped graphics libraries identify Mesa's software implementation of
OpenGL. This validates the application's OpenGL rendering path in that private
display. It does not measure a hardware GPU and does not establish acceptance
of the separately compiled `linux-software` backend.

A second empty-account profile used a generated four-quadrant background,
stretch mode, zero blur and 60% opacity. All four samples matched the expected
color direction and alpha composition within the five-level channel tolerance:

| Window point | Expected RGB | Observed RGB |
| --- | --- | --- |
| 235, 100 | 139, 44, 46 | 138, 44, 46 |
| 1145, 100 | 43, 121, 75 | 42, 121, 75 |
| 235, 700 | 43, 73, 142 | 42, 73, 142 |
| 1145, 700 | 144, 121, 46 | 143, 121, 46 |

Screenshots and native binaries remain outside Git. Their hashes in the receipt
identify the reviewed files. These observations supplement the native image
crop tests; they do not establish a successful platform cover upload.

### OBS module loading

The plugin built against the local OBS and Qt SDK. An independent OBS 33
instance, started with `--multi` and private XDG configuration and D-Bus,
listed `obs-bilibilive-pk.so` in its loaded modules. `/proc` mapped the exact
plugin copy placed in that private profile, with SHA-256
`989f5698c7cae5e1fcd78c98ec94e926156d34782782af34d7084d3e710cb28d`.

This check loaded the module without starting capture or streaming and without
modifying the user's OBS instance. It proves native
module loading in that OBS environment. It does not prove a complete OBS
capture-to-platform PK exchange. The private fixture sessions reported that
their child processes had exited and their private configuration was removed.

## Remaining acceptance boundaries

The final native gate above uses the refreshed lock; the older screenshot and
media probes retain their original snapshot attribution. Formal release results
should identify their own source commit. Authenticated Bilibili permissions,
broadcast mutations, online
two-party PK, direct/relay transport tickets, macOS and Windows desktop/plugin
runtime behavior, and published-package installation each require their own
receipts. [COMPATIBILITY.md](COMPATIBILITY.md) describes the complete audited
scope and [STARTLIVE_AUDIT.md](STARTLIVE_AUDIT.md) maps the reference project.
