# Changelog

## 0.1.3

Release status as of 2026-11-22: **not published; real-account acceptance is
pending**. The local `0.1.3-0.3` acceptance candidate is installed.

- Persist an application device ID independently of Bilibili's SPI device
  cookies, matching StartLive's session model. Reuse the native `buvid`
  header across frontend recreation, restarts and account changes; preserve
  SPI values in the authenticated Cookie header. Anonymous QR requests now
  carry public device metadata without saved account credentials, and APP
  and WEB requests use their respective header profiles.
- Replace the application and default tray logos with a minimal open playback
  mark. The scalable icon and tray drawing remain text source in Git.
- Prepare face verification in the shared broadcast path: initialize missing
  device cookies, register V2 vouchers, and report the original platform
  challenge before displaying its QR code. Preserve the initiating account,
  room, area and protocol when continuing after platform confirmation; cancel,
  expiry and initialization failures do not continue a broadcast.
- Coalesce bounded batches of pointer and resize events, stop redundant
  mouse-move redraws, and cache images at their display size. Crop release
  applies its final pointer position; source pixels remain available for full
  resolution cropping. Area selection copies only visible rows.
- Add the **Live interface configuration** page and shared
  `protocol.status/refresh` operations. Refresh all eight upstream request
  profile fields from the official StartLive JSON, retain validated local
  cache data on failure, and keep an immutable profile for each active
  client. Automatic protocol and release checks share the saved preference
  and `--noupdate`; demo and read-only modes do not refresh the cache.
- Redraw immediately after navigating through in-page controls, so the
  destination page appears without requiring another mouse or keyboard event.
- Add release packaging for Windows `Setup.exe`, macOS `Setup.pkg` and `.app`,
  complete Velopack portable archives, and separate platform/architecture
  update feeds. A complete portable installation retains its update helper
  and metadata; a bare executable or ordinary archive does not gain that
  managed-update identity.
- Add the desktop **Updates and components** page and shared
  `updates.check/status/download/apply` operations. Startup checks follow the
  saved preference and `--noupdate`; demo and read-only startup skip them.
  Download and apply each require an explicit action and a verified staged
  identity. Cargo and AUR installations retain their package-manager update
  path, and newer development versions are not offered a downgrade.
- Persist validated update receipts so separate CLI calls can check, download
  synchronously, and apply. Background downloads require a persistent host.
  Before applying, retire local observation and media sessions, then exit
  normally after the successful reply is flushed. OBS output and the live
  room retain their current state. A lost apply reply can be replayed with
  matching parameters within the helper's wait period without launching a
  second helper.
- Require `updates.apply` to be the last expanded step of a zuse program.
  Propagate terminal success and partial shutdown failures through the script
  receipt so GUI, HTTP and MCP hosts make the same exit decision. Avoid
  blocking that exit on a contended diagnostic log.
- Add verified local OBS plugin status and explicit installation from the
  release bundle. Existing files are preserved unless replacement is
  requested; package-managed Linux plugins remain under package-manager
  ownership. Installation receipts describe files and request an OBS
  restart; they do not claim the module has loaded.
- Fix Windows OBS plugin installation on canonical drive paths by joining
  the drive prefix and root before checking directories. Reject drive-relative
  paths and retain per-directory symlink and reparse-point checks. Add Windows
  installation and junction-rejection regressions, and run the installation
  gate before native media compilation.
- Add Windows/macOS payload builds for the OBS plugin, FFmpeg, FFplay,
  libdatachannel and their non-system runtime dependencies and licenses.
  The software media path includes H.264, AV1 and Opus. Linux continues to
  use distribution-provided media libraries. Windows builds require all six
  H.264/AV1 encoder backends for NVIDIA NVENC, AMD AMF and Intel QSV; macOS
  enables VideoToolbox. Compiled encoder availability is recorded separately
  from execution on a compatible device and driver.
- Add native packaging gates for archive identity, bundled-file hashes,
  software media round trips and a private Velopack download/apply/restart
  probe. The probe updates a uniquely named fixture, then checks the new
  process version and executable hash; helper launch alone is not a pass.
- Repair native dependency builds by backporting Intel's reviewed oneVPL
  MinGW compiler guard into an isolated build copy, with source and patched
  header checksums, and using VideoLAN's official GitHub mirror for dav1d.
- Disable FFplay terminal status output explicitly with `-nostats` in media
  smoke checks. Error-level reporting and rejection of actual error output
  remain enabled. The prior failure contained only six status-line control
  bytes whose SHA-256 matched the original CI log; the
  [regression fixture](scripts/test_native_runtime.py) checks both this case
  and rejection of an actual window-creation error.
- Refresh the development lock to the public zuse-computer 0.5.0 source.
  Registry staging continues to require a matching published package and
  complete source verification.
- Schedule grouped Dependabot checks for Rust dependencies and release
  actions. Project Z Git declarations remain on their default branches;
  dependency updates retain the local validation and explicit release gates.

The preceding `0.1.3-0.2` session-compatibility candidate passed **142 tests**:
126 library tests, 3 CLI tests and 13 integration tests, using Rust 1.100.0 and
`cargo test --locked --offline`. Formatting and the optimized release build
also passed. That candidate authenticated the saved account and
obtained a valid new face challenge. That challenge expired while awaiting
the user's phone result, so successful phone verification and broadcasting
remain unconfirmed. The
[session compatibility record](docs/evidence/session-compatibility-2026-11-22.json)
identifies the tested executable and the separate observations from the
preceding package. [FACE_VERIFICATION.md](docs/FACE_VERIFICATION.md) defines
the remaining acceptance steps.

The current `0.1.3-0.3` candidate includes the GUI timeout-boundary correction
and passed **144 tests**, formatting and an optimized release build. Its
installed executable matches the validated build. Installation made no
account API requests and did not start the real GUI; phone verification and
broadcasting have not been accepted on this candidate.

Real-account acceptance remains unsuccessful, and account tests are paused
after a user-reported mobile request limit. The recorded native packaging
preview also has unresolved macOS gates; see the session compatibility record
for the separate observations and validation scopes.

The preceding Linux regression gate for the face-verification and interaction
fixes passed **139 tests**: 123 library tests, 3 CLI tests and 13 integration tests,
using Rust 1.100.0 and `cargo test --locked --offline --all-features`.
The default GUI build and headless check also passed. Private demo-window
checks confirmed immediate area selection, crop movement to the final pointer
position, and stable image coordinates after resizing the window.
[VALIDATION.md](docs/VALIDATION.md) records the tested source commit and the
scope of each check; installer and update behavior also have their own records
in [UPDATES.md](docs/UPDATES.md). No frame-rate or interaction-latency benchmark
was performed.
The reported phone-side authentication failure was reproduced on the preceding
package while its challenge remained valid. The current device-identity
correction has not yet been shown to resolve that failure. Two-party PK and
physical GPU encoding require their own runtime acceptance results.

The default packaging configuration does not provide Windows Authenticode
signatures or Apple Developer ID/notarization. macOS application and runtime
files use ad-hoc signatures; its installer package remains unsigned. Optional
macOS signing inputs are supported by the packaging script, and an actual release's
receipt records which signing steps were used.

## 0.1.2

- Use a warm-gray and terracotta-orange desktop palette while preserving
  the existing layout and operation behavior.
- Skip automatic OBS observer requests during read-only desktop startup,
  and avoid sending a stop request before observation has been enabled.
- Report automatic OBS streaming failures after a successful room start as
  partial failures. Preserve the live room state, stop later zuse mutations,
  and show guidance for checking OBS without repeating the room start.
- Show failed embedded zuse runs as failures in the desktop, retaining their
  execution receipts without retrying operations.
- Refresh zuse-computer to the 0.4.1 default-branch source, including its
  isolated Num Lock regression fixtures. Its runtime source is unchanged.

## 0.1.1

- Stop embedded zuse programs when a shared operation reports a structured
  failure, including partial PK teardown and expired verification. Preserve
  completed steps and redacted failure details, and skip later mutations.
- Exclude root Git administration files from dependency publication scope
  while retaining complete source and registry archive comparisons.
- Build Windows releases with the initialized native compiler environment,
  and support release retries against an unchanged, verified tag.
- Keep native C/C++ archives linkable in AUR builds by disabling makepkg's
  injected LTO flags; Cargo's configured Rust release optimization remains.

## 0.1.0

The first Rust implementation uses one operation dispatcher for the zgpui desktop,
zclap CLI, zmcpio MCP service, local HTTP API and embedded zuse programs.

- Bilibili account login and switching, room metadata, RTMP/SRT selection,
  cover preview and 16:9 cropping, recent titles and areas, audit polling,
  delay control and platform verification handoff.
- OBS WebSocket configuration, output observation and live-state linking;
  PK signalling, invitation events and native libdatachannel/FFmpeg media;
  the attributed OBS capture plugin is distributed as source.
- Scene conversion, typed operation schemas, bounded zuse execution,
  operating-system credential storage, font/background preferences, tray
  integration and per-profile single-instance activation.
- `--endpoint` lets CLI commands share the state of an existing local GUI or
  service. The foreground `rtc preview-local` command keeps a local preview
  alive until cancellation.
- Default-branch Project Z development dependencies, a committed dependency
  receipt, verified registry staging, formal release builds and separate
  AUR application/plugin packages.

The dependency refresh uses zconv 0.4.1, zcall 0.4.1, zmcpio 0.5.0,
zuse-computer 0.4.0 and zgpui 0.1.6. Headless tests passed after this update.
Earlier native validation also covered a complete Chinese-language window,
four background pixel samples, local RTC/encoding and isolated OBS 33 module
loading. Snapshot identifiers and the scope of each result are in
[VALIDATION.md](docs/VALIDATION.md).

Authenticated platform broadcasts, two-party PK media and macOS/Windows native
runtime acceptance are still separate gates. See
[COMPATIBILITY.md](docs/COMPATIBILITY.md) for the audited StartLive scope.
