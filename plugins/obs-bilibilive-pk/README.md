# Bilibilive OBS PK capture

This native OBS plugin sends the current program video and 48 kHz stereo PCM to
bilibilive through a private local connection. Video frames use a locked shared
memory mailbox; audio uses the SLPK v2 stream. The protocol is compatible with
StartLive's capture plugin.

## Install the bundled plugin

The application's **Components** page installs the plugin that belongs to the
same application build. The CLI and MCP use the same backend:

```sh
bilibilive call obs.plugin.status --args '{}'
bilibilive call obs.plugin.install --args '{}'
```

The installer verifies the bundle manifest, package version, architecture and
every plugin file before writing. It stages the new directory beside the target,
keeps the previous directory as a backup, and restores its name if the commit
fails. Replacing an existing unrecorded or modified plugin requires an explicit
`{"replace":true}` request. A successful result includes `backup_path` when a
previous installation was retained.

| Platform | Default plugin directory |
| --- | --- |
| Windows | `%ProgramData%\obs-studio\plugins\obs-bilibilive-pk\`, containing `obs-bilibilive-pk.dll` |
| macOS | `~/Library/Application Support/obs-studio/plugins/obs-bilibilive-pk.plugin` |
| Linux | `$XDG_CONFIG_HOME/obs-studio/plugins/obs-bilibilive-pk/`, or `~/.config/obs-studio/plugins/obs-bilibilive-pk/` |

Windows uses OBS's current flat plugin layout. Writing to ProgramData can require
administrator access; the application does not elevate privileges. A permission
failure returns the path for manual installation. For a supported portable OBS
installation, the advanced `destination` argument selects its containing plugins
directory. Linux installations detected in `/usr/lib/obs-plugins` or
`/usr/lib64/obs-plugins` remain managed by the system package manager.

After installation, restart OBS yourself and enable **Bilibilive PK capture**
under **Tools**. The status operation checks files and reports
`loaded_in_obs: null`; it does not inspect or control a running OBS instance.
The installation receipt is kept outside the plugin directory so a signed macOS
`.plugin` retains its sealed contents.

System media dependencies remain available when no bundle is present. An
application built without a bundled-manifest digest refuses an adjacent plugin
payload unless its digest is explicitly supplied from a trusted source.
An advanced local installation can provide `bundle_root` and an independently
trusted `manifest_sha256`; all version, architecture and file checks still apply.

See OBS's [current plugin layout](https://obsproject.com/kb/legacy-plugin-developer-info)
and [plugin installation guide](https://obsproject.com/kb/plugins-guide).

## Build from the installed OBS SDK

Use the current installed OBS development SDK, SIMDe headers, CMake and Qt 6 Widgets. On Arch Linux these build prerequisites are provided by `obs-studio`, `simde`, `cmake` and `qt6-base`. The OBS SDK package exports the SIMDe dependency through `libobsConfig.cmake`:

```sh
cmake -S plugins/obs-bilibilive-pk -B dist/obs-plugin \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
cmake --build dist/obs-plugin --parallel
```

On Linux the module is `dist/obs-plugin/obs-bilibilive-pk.so`. Install it into
OBS's plugin directory, restart OBS, and enable **Bilibilive PK capture** under
**Tools**. A selected capture stream is negotiated before resources are
allocated. Only one media consumer connects at a time.

The native Rust media worker reads BGRA or NV12 video, feeds the selected FFmpeg
H.264 or AV1 encoder and Opus audio encoder, then sends RTP through the active
libdatachannel DTLS/SRTP session. HDR output requires an explicit SDR conversion
in OBS before capture. The capture ceiling is 60 FPS.

Linux/macOS use a private socket under `/tmp/startlive-pk-<uid>/`; Windows uses
`\\.\pipe\StartLive.PkMedia.v2`. These compatibility names are intentional.
Do not load both this plugin and the upstream StartLive PK plugin in one OBS
instance because they use the same capture endpoints and source identifiers.

## Prepare a release payload

`scripts/prepare-native-runtime.py` builds the plugin before the application.
Use a fresh output directory and keep both output and cache outside the packaged
source tree, or under the repository's `dist` directory:

```sh
python scripts/prepare-native-runtime.py \
  --target x86_64-apple-darwin \
  --cache /absolute/native-build-cache \
  --output /absolute/native-payload
```

Supported native targets are Linux x86_64, Windows x86_64, macOS arm64 and macOS
x86_64. Windows also requires `--msys2-root`. The build reads the latest official
stable OBS release's SDK checksums and records the actual OBS/Qt identities.
FFmpeg, libdatachannel, OpenSSL, SVT-AV1, Opus, dav1d and SDL2 are built from
recorded official source commits. x264 uses an explicitly recorded official
master snapshot because upstream publishes its current source without numbered
release tags. Linux uses its installed OBS SDK and distribution media packages.

Windows builds include H.264 and AV1 encoders for NVIDIA NVENC, AMD AMF and Intel
QSV. The builder installs official NVENC/AMF headers and builds the oneVPL
dispatcher DLL. GPU driver implementations remain supplied by the corresponding
display driver. macOS enables VideoToolbox. The manifest records which encoders
were compiled and marks hardware execution as unverified until a device probe
succeeds; the current driver's requirements are recorded with the SDK source.

The software path has a build gate: H.264/AV1 encode and decode real synthetic
frames, Opus preserves the expected decoded sample count, and FFplay opens raw
NV12, PCM, H.264 and AV1 using silent dummy devices. The final application can
also exercise its selected runtime and RTP muxer without an account or OBS:

```sh
bilibilive call rtc.encoder.probe --args '{"codec":"h264","encoder":"libx264"}'
bilibilive call rtc.encoder.probe --args '{"codec":"av1","encoder":"libsvtav1"}'
```

The completed payload contains `bundle-manifest.json`,
`resources/obs-plugin/` and `runtime/{bin,lib,licenses}`. The final application
build receives the manifest digest through
`BILIBILIVE_BUNDLE_MANIFEST_SHA256`. Windows/Linux place the payload beside the
application executable; macOS places the same tree at
`bilibilive.app/Contents/Resources/bilibilive/`. macOS runtime files and the
plugin are signed before hashing. Packaging must preserve those bytes when it
signs the outer application. The application executable is excluded from this
manifest to keep the build order finite.

The builder verifies native architecture, dependency closure, license files,
software media tests and libdatachannel exports before producing a successful
receipt. Windows and macOS release jobs perform the corresponding native build;
the offline fixtures in `scripts/test_native_runtime.py` cover input boundaries
and packaging failure cases. GPU execution and OBS loading require their own
device and application checks.

See [UPSTREAM.md](UPSTREAM.md) for copyright and source provenance.
