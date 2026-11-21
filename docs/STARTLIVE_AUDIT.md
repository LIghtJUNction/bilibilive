# StartLive compatibility audit

## Baseline

The reference is [Radekyspec/StartLive](https://github.com/Radekyspec/StartLive),
not an OBS encoder or a general-purpose live-stream player. The audited default
branch is `dev`, commit
[`0b5a9830a8305d9cd6ac15378e46bee05b894c11`](https://github.com/Radekyspec/StartLive/tree/0b5a9830a8305d9cd6ac15378e46bee05b894c11),
dated 2026-10-30. The observed `master` revision is
`bc26236aa36d037d98f7df82a0552f27ff898270`; tag `1.2.1` resolves to
`84d9d383cfd15311b83b93d0dac398f98668f11b`. Both current branches include PK and
configuration conversion. Comparing only the old release's basic streaming
panel would omit current upstream functionality.

This document records what upstream implements. It is a coverage target, not a
claim that every row is implemented or verified in bilibilive. Actual release
coverage belongs in `COMPATIBILITY.md` and must distinguish offline protocol
tests, native UI checks, and live platform verification.

## License and assets

Upstream's `pyproject.toml` declares `GPL-3.0-only AND
LicenseRef-StartLive-Artwork`. Source-code adaptations therefore retain GPL
attribution. Its logo has a separate reserved-rights license in
`LICENSE-ARTWORK`; bilibilive must use original artwork. The audited source and
protocol mappings should be credited without copying the restricted logo.

## User-visible feature matrix

Python package paths below are relative to the audited upstream `startlive/`
directory. `StartLive.py`, `.github/` and `plugins/` paths are relative to the
repository root.

| Area | Upstream behavior to preserve | Evidence |
|---|---|---|
| Account login | Generate a login QR, poll once per second, distinguish waiting/scanned/expired/success, allow cancellation | `core/workers/login/fetch_qr.py`, `fetch_login.py` |
| Account persistence | Store multiple accounts in the OS keyring, list names and UID, select/remove an account, clear credentials | `core/credentials/store.py`, `PySide/widgets/sl_menu_bar.py` |
| Account validation | Check `/x/web-interface/nav`; match returned UID to stored identity; distinguish permanent invalidity from transient network failure | `core/workers/credentials/credential_manager.py` |
| Account concurrency | Capture an account generation per job; reject late results after account switching | `core/identity.py`, `core/app_state/__init__.py` |
| Device/ticket | Refresh expired `bili_ticket`; obtain `buvid3`/`buvid4`; send the client device profile | `core/workers/login/buvid_ticket.py`, `core/sign/bili_ticket.py` |
| Room bootstrap | Read room ID, area, live status and pre-live title/cover/schedule | `core/workers/pre_live/fetch_pre_live.py` |
| Room eligibility | Check `GetRoomPreLiveStatus` before opening the room | `core/workers/pre_live/fetch_room_status.py` |
| Live start | Signed app request containing room, area and type 2; distinguish success from face-verification challenges | `core/workers/live/start_live.py` |
| Live stop | Signed app request with CSRF added after signing; stop linked OBS output when enabled | `core/workers/live/stop_live.py`, `PySide/window/stream_config.py` |
| Stream endpoint | Show/copy stream address and key; preserve RTMP, SRT with RTMP fallback, and SRT-only policies | `core/constant/__init__.py`, `core/workers/live/start_live.py` |
| Existing live room | Upstream refreshes its key by issuing a duplicate start request; bilibilive's explicit stream lookup should document its different side-effect behavior | `core/workers/pre_live/fetch_pre_live.py` |
| Face verification v1 | Present the server-issued QR; report the challenge and poll `IsUserIdentifiedByFaceAuth` | `core/workers/face_auth/` |
| Face verification v2 | Register `v_voucher`; decode the returned challenge; present Bilibili's realname URL; poll `validatePreCheck` | `core/sign/captcha_codec.py`, `core/workers/face_auth/face_captcha.py` |
| Title | Update title, display returned audited title, retain the five most recent titles per account | `core/workers/title/` |
| Title audit | Read `GetRoomContentAuditInfo`; display platform rejection reason rather than silently starting with the wrong title | `core/workers/room/room_content_audit.py` |
| Areas | Fetch hierarchical area choices, searchable picker, recent areas, change current room's area | `core/workers/area/`, `PySide/window/area_picker.py` |
| Announcement | Read and update the room announcement | `core/workers/announce/` |
| Cover | Load preview, select/crop image, upload multipart data, apply image URL, show rejection/pending/approved state | `core/workers/cover/`, `PySide/window/cover_crop.py` |
| Cover audit | Poll the cover audit state while pending | `core/workers/cover/cover_state_update.py` |
| Stream delay | Read current/minimum/maximum delay and update within platform limits | `core/workers/live_delay/` |
| OBS connection | Connect using host, port and password; auto-connect preference; clear saved settings | `core/workers/obs_ws/`, `PySide/window/stream_config.py` |
| OBS automation | Apply `SetStreamServiceSettings`, optionally `StartStream`/`StopStream`, observe actual output status and reconnecting status | `core/workers/obs_ws/obs_daemon.py`, `PySide/window/stream_config.py` |
| Local HTTP control | `POST /api/startLive` and `POST /api/stopLive`; configured with `--web.host` and `--web.port` | `PySide/web_server/__init__.py`, `StartLive.py` |
| Tray/window | Single-instance activation; tray restore, current-account label, start/stop and quit; close handling | `PySide/classes/single_instance_window.py`, `PySide/window/main_window.py` |
| Preferences | None/system/custom proxy; custom font, tray icon/hint, background image/mode/blur/opacity; reset sections | `PySide/window/settings_page.py`, `core/app_state/__init__.py` |
| Logging | View logs, open log directory, sanitize credentials and sensitive API fields | `PySide/widgets/log_viewer.py`, `core/log/` |
| Protocol profile refresh | Read cached constants, fetch the reference JSON, update request profiles and save the cache | `core/workers/const/const_update.py`, `PySide/window/main_window.py` |
| Version notification | Check the latest GitHub release during UI setup and indicate a newer version in the title | `core/workers/const/version_checker.py`, `PySide/window/main_window.py` |
| Application update | Package-manager-aware Velopack check, download/progress and apply/restart on supported installations | `StartLive.py`, `core/runtime.py`, `PySide/updater/` |
| Installer and plugin distribution | Windows setup executable, macOS installer package, architecture-specific updates and native OBS plugin builds | `.github/workflows/release-package.yml`, `.github/actions/build-obs-pk/action.yml` |
| Scene conversion | Discover LiveHime scene JSON, preview conversion, optional canvas override and scene-name prefix filtering, export an OBS scene collection | `core/config_conversion/livehime.py`, `PySide/window/config_conversion_dialog.py` |
| Scene conversion safety | Read-only source; 32 MiB limit; UTF-8 BOM/UTF-16; finite numbers; allowlisted source/filter/transition IDs; warnings for unsupported plugins/missing assets | `core/config_conversion/livehime.py`, `core/constant/config_conversion.py` |
| PK modes | Classic PK, video PK and video link | `core/constant/pk.py`, `PySide/window/pk_window.py` |
| PK matching | Platform precheck, random match/cancel, continuous PK status/update | `core/workers/pk/`, `PySide/controllers/pk/match_controller.py` |
| PK invitations | Search candidates by UID/room; retain server `label_type`; invite/cancel; receive and accept/reject invitations with timeout and exact peer/session/source context | `core/pk/candidates.py`, `core/pk/parameters.py`, `PySide/controllers/pk/invitation_lifecycle.py` |
| PK settings | Accept PK, following restrictions, classic voice, video connection permissions, join/apply/invite switches | `core/constant/pk_user_settings.py`, `core/pk/user_settings.py` |
| PK rounds | Query classic/modern info; display opponent, timing, score/result; end round; distinguish ending PK from disconnecting a link | `core/pk/round_state.py`, `PySide/controllers/pk/round_controller.py` |
| Video link | Panel bootstrap, recommended candidates, connect, convert connection to PK, ordered exit/quit | `core/workers/pk/video_link.py` |
| PK event channel | Authenticated room push connection, heartbeat, reconnect/token refresh, compressed packet decode and sequence ACK | `core/danmaku/`, `core/workers/danmaku/`, `core/pk/push_protocol.py` |
| PK RTC signaling | Decode JOIN tickets, establish token-derived ICE/DTLS/SCTP connection, binary RPC signaling, user join/leave and media negotiation | `core/pk/join.py`, `rtc_connection.py`, `rtc_signaling.py`, `rtc_wire.py` |
| PK transport | Honor relay-only tickets, test supported TURN transports, reject unsupported direct ICE-TCP; do not silently weaken transport policy | `core/pk/rtc_connection.py`, `rtc_relay.py` |
| PK audio/video | Publish OBS audio/video, subscribe to remote media, monitor local audio, mute/hide local or remote streams | `core/pk/rtc_media.py`, `PySide/window/pk_window.py` |
| PK codecs | H.264/AV1 negotiation and packetization; platform hardware encoders/CPU fallback; encoder preset/AQ/scale settings | `core/pk/rtc_h264.py`, `rtc_av1.py`, `video_encoder_backend.py`, `states/encoder.py` |
| PK preview | Local and remote previews, crop in original-frame coordinates, asynchronous conversion, bounded buffers | `PySide/widgets/pk_media_view.py`, `pk_crop_selector.py`, `core/pk/video_frames.py` |
| OBS PK plugin | Same-user local IPC; per-connection audio/video selection; 48 kHz stereo s16 PCM; NV12/BGRA shared-memory frames | `plugins/obs-startlive-pk/`, `core/pk/obs_transport.py`, `shared_video.py` |
| Session teardown | Cancel outstanding jobs; retire invitation/round ownership; stop local capture/RTC on account change, room stop or window close | `PySide/controllers/pk/session_controller.py`, `rtc_lifecycle.py`, `PySide/window/pk_window.py` |

Account switching belongs to the upstream menu bar's account menu, implemented
by `PySide/widgets/sl_menu_bar.py`. The tray displays the selected account; it
has no account-switch submenu in this audited revision. Bilibilive provides
selection in its Accounts page and the corresponding shared operations.

## Update triggers and compatibility choices

The reference has three distinct update jobs. `ConstantUpdateWorker` changes
the cached Bilibili protocol profile, including request headers and signing
fields. `VersionCheckerWorker` checks release metadata and displays a version
hint. UI setup starts these two jobs together, unless `--noupdate` suppresses
that branch or its constants work was already completed.

`VelopackUpdateController` starts separately through a one-second timer in the
main-window constructor. Its worker skips package-managed and Linux installs;
supported installations check for an application package, download it, quit
the UI and apply the update. The reference's `--noupdate` branch does not
control this separate timer. These triggers are visible in
`PySide/window/main_window.py`, `PySide/updater/update_controller.py` and
`PySide/updater/update_worker.py`.

Bilibilive checks the stable application release once when the saved
`update_check` preference permits it. `--noupdate` suppresses that launch's
automatic check. Download and apply remain explicit operations; Cargo and AUR
ownership is retained, and an installer package is applied only after its
identity and contents are verified. Bilibili protocol profiles remain compiled
and reviewed with the application source; the upstream remote constant-refresh
mechanism is intentionally not reproduced. [UPDATES.md](UPDATES.md) describes
the current user flow and installation-channel boundaries.

The current upstream PK system spans roughly 12,000 lines across its core,
worker and controller modules, plus the OBS capture plugin. Registering HTTP
endpoints or displaying a PK button does not reproduce this media pipeline.

## REST contracts

The app signer sorts fields lexicographically and hashes the encoded payload
plus the public LiveHime app secret using MD5. The ordinary request encoding
uses form-style `+` for spaces. Native PK requests use `%20` and preserve
`!'()*~`. These encodings must not be interchanged after signing.

| Operation | Method and path | Signing details |
|---|---|---|
| Login QR | GET `passport.bilibili.com/x/passport-login/web/qrcode/generate` | `source=live_pc`, `go_url`, `web_location=0.0`; do not attach another account |
| Login poll | GET `passport.bilibili.com/x/passport-login/web/qrcode/poll` | `qrcode_key`, source, web_location; consume successful Set-Cookie privately |
| Account | GET `api.bilibili.com/x/web-interface/nav` | Cookie account, check isLogin and UID |
| Ticket | POST `api.bilibili.com/bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket` | HMAC-SHA256 ticket parameters; query fields |
| Device IDs | GET `api.bilibili.com/x/frontend/finger/spi` | Read `b_3`, `b_4` |
| Room | GET `/xlive/app-blink/v1/room/GetInfo` | Signed `uId` |
| Pre-live | GET `/xlive/app-blink/v1/preLive/PreLive` | Signed area/cover/coverVertical/liveDirectionType/mobi_app/schedule/title |
| Eligibility | GET `/xlive/app-blink/v1/index/GetRoomPreLiveStatus` | Signed, omit access_key |
| Start | POST `/room/v1/Room/startLive` | Sign area_v2, room_id, type=2, csrf, csrf_token |
| Stop | POST `/room/v1/Room/stopLive` | Sign room_id first; append csrf and csrf_token afterward |
| Stream lookup | POST `/xlive/app-blink/v1/live/FetchWebUpStreamAddr` | Signed backup_stream=0, then CSRF; deprecated in upstream |
| Title | POST `/xlive/app-blink/v1/preLive/UpdatePreLiveInfo` | Signed query; title/room_id/mobi_app/CSRF body |
| Cover upload | POST `api.bilibili.com/x/upload/web/image` | CSRF query; multipart bucket=live, dir=new_room_cover, file |
| Cover apply | POST `/xlive/app-blink/v1/preLive/UpdatePreLiveInfo` | cover URL, coverVertical, direction, CSRF body |
| Areas | GET `/xlive/app-blink/v1/preLive/GetAreaListForLive` | Signed; parse `area_v1_info[].list[]` |
| Recent areas | GET `/room/v1/Area/getMyChooseArea` | Signed `roomid` |
| Change area | POST `/xlive/app-blink/v2/room/AnchorChangeRoomArea` | Signed query, area_id/room_id/build/platform/CSRF body |
| Announcement read | GET `/xlive/app-blink/v1/room/AnnounceInfo` | Signed; content under `announces["1"]` |
| Announcement write | POST `/xlive/app-blink/v1/room/AnnounceCommit` | Sign base fields; append content/type=1/CSRF |
| Title audit | GET `/xlive/app-blink/v1/room/GetRoomContentAuditInfo` | Sign CSRF; append content_type=1 and room_id |
| Delay read/write | GET `.../upStreamConfig/GetAnchorSelfStreamTimeShift`; POST `.../SetAnchorSelfStreamTimeShift` | Signed room_id/CSRF; write time_shift |
| Face v1 poll | POST `/xlive/app-blink/v1/preLive/IsUserIdentifiedByFaceAuth` | room_id, face_auth_code=60024, CSRF, visit_id |
| Face v2 register | POST `api.bilibili.com/x/gaia-vgate/v2/register` | v_voucher, dm_track, CSRF |
| Face v2 poll | POST `api.bilibili.com/x/gaia-vgate/v2/validatePreCheck` | Obfuscated token, dm_track, CSRF |
| Face report | POST `/xlive/app-blink/v1/preLive/ReportFaceRecognition` | Sign base fields; append account/area/challenge/CSRF |
| Push bootstrap | GET `/xlive/app-room/v1/index/getDanmuInfo` | Exact native app fields and account identity |
| Push ACK | POST `/xlive/open-interface/v1/dm/message_ack` | Exact native sequence ACK encoding |

Unqualified REST paths use `https://api.live.bilibili.com`. The 24 PK REST
operations and their exact post-sign allowlists are copied as a reviewed
protocol table into `src/bili.rs::PK_OPERATIONS`; upstream evidence is
`core/constant/pk_api.py` and `core/pk/api.py`. In particular, PK join, cancel
and continuous-mode updates use GET despite changing server state. They must
not receive automatic retry or read-only classifications.

## Verification obligations

Protocol fixtures must test the exact signed bytes, both CSRF orderings,
PK post-sign exceptions, malformed server responses and sensitive-error
redaction. QR fixtures must verify that response cookie attributes never become
cookies and credentials never leak into public output. Account-switch tests
must reject stale job completions. Stream-policy fixtures must acknowledge a
room that already started even when SRT-only cannot obtain its requested
endpoint.

Real platform validation requires an explicitly controlled account and a
known room. Routine build/tests must not start a real broadcast, send chat,
invite other users or change a live room. A successful HTTP response to a PK
request is evidence of that REST operation only; RTC/media coverage requires
audio/video receipt and teardown evidence. Upstream's HTTP server itself
returns `OK` on signal dispatch, before the broadcast result exists.

## Review

- Contribution: this audit fixes a reproducible upstream snapshot and separates
  REST control from the media pipeline.
- Clarity: every matrix row has a concrete upstream source path.
- Evidence: the matrix was read from source; no real broadcaster account was
  used during the audit.
- Completeness: core controls, preferences, distribution, conversion and the
  optional PK subsystem are included.
- Design: public interfaces should share one dispatcher, retain account/job
  ownership and report actual per-operation completion.
