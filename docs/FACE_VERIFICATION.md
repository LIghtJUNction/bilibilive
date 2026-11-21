# Face verification and broadcast continuation

bilibilive prepares Bilibili's verification session before presenting a QR
code. A successful phone scan alone does not start the broadcast: the platform
must confirm that the identity check has passed.

**Acceptance status, 2026-11-22 (Asia/Taipei): not passed; account tests are
paused.** During a later reference-flow attempt, the user reported the mobile
message “请求达到上限了” (request limit reached). Both the reference process
and the bilibilive GUI used for these tests were stopped. Local `0.1.3-0.4` is
now installed, without account API requests or starting the real GUI during
installation. The preceding `0.1.3-0.2` candidate validated the saved account
and obtained valid face challenges, but no passed face verification or
successful broadcast has been observed. The session changes below correct a
confirmed difference from StartLive; neither the original phone failure's
cause nor its resolution has been established. No feedback confirming that
the mobile restriction has cleared has been received. The
[session compatibility record](evidence/session-compatibility-2026-11-22.json)
identifies the source, executable and observations.

## What each result establishes

Account authentication, face verification and broadcasting are separate
results. The following values must not be treated as interchangeable:

| Observed result | Meaning |
|---|---|
| `auth.validate`: `authenticated=true` | The saved account credentials are valid |
| V2 `moreInfo`: `code=0`, `realname.status=1` | The challenge can read the account's existing realname information |
| V2 `validatePreCheck`: `data.status=1` | This face challenge has passed |
| V1 `IsUserIdentifiedByFaceAuth`: `is_identified=true` or `1` | This V1 face challenge has passed |
| Resumed `live.start`: `status=live`, then room `live_status=1` | The platform has started the requested live room |

The mobile page can display an error while V2 polling still returns
`data.status=0`. That value means the challenge is still waiting; it does not
authorize broadcast continuation. The shared implementation is in
[`src/bili.rs`](../src/bili.rs), with account and continuation guards in
[`src/app.rs`](../src/app.rs).

## Upstream contract

The audited reference is
[`Radekyspec/StartLive@0b5a9830a8305d9cd6ac15378e46bee05b894c11`](https://github.com/Radekyspec/StartLive/tree/0b5a9830a8305d9cd6ac15378e46bee05b894c11).
[StartLive issue #21](https://github.com/Radekyspec/StartLive/issues/21)
reported a phone-side system error during the normal face-verification flow.
The reporter confirmed that 0.8.5 resolved it after the project added the
verification-report event when opening the QR window. The relevant change is
[`7f4ea0f`](https://github.com/Radekyspec/StartLive/commit/7f4ea0fa744872b9309c4b990632a574741f3d97).

Both verification versions use the original `startLive` response code and
message when calling `ReportFaceRecognition`. The report signs the public
LiveHime profile fields first, then appends the room, area, challenge fields
and CSRF. StartLive starts its report worker when opening the QR window.
bilibilive completes the required registration and report before returning a
ready QR code, so initialization errors reach the desktop before the phone
handoff.

| Stage | V1 (`60024`) | V2 (`60043`) |
|---|---|---|
| Challenge | Original `data.qr` URL | Original `data.risk_extra.v_voucher` |
| Registration | No additional registration | Register voucher, decode response, construct the official realname URL |
| Report | Original code/message, room and area | Original code/message, room and area, after registration |
| Poll | `IsUserIdentifiedByFaceAuth` | `validatePreCheck`, using this registration's obfuscated token |
| Confirmed result | `is_identified` is true or 1 | `status` is 1 |
| Continuation | Original account UID, room, area, protocol and strategy | Original account UID, room, area, protocol and strategy |

The request sequence is implemented in `BiliClient::start_live`, shared by
the GUI, CLI, MCP, HTTP and embedded zuse. The GUI does not issue a second
registration or replace the original report message with `verified`.
The low-level `face.register`, `face.report` and `face.poll` operations remain
available for callers that need the individual protocol operations.

## Device and account continuity

StartLive maintains an application device ID separately from Bilibili's
browser device cookies. Its
[`gen_buvid`](https://github.com/Radekyspec/StartLive/blob/0b5a9830a8305d9cd6ac15378e46bee05b894c11/startlive/core/sign/gen_buvid.py)
and
[`create_session`](https://github.com/Radekyspec/StartLive/blob/0b5a9830a8305d9cd6ac15378e46bee05b894c11/startlive/core/app_state/__init__.py)
use a persistent application value for the `buvid` header, while account
cookies can contain a different SPI-issued `buvid3`. The earlier bilibilive
client used the account's `buvid3` for both purposes.

The corrected App stores its application device ID once in the profile's
`device.json`. Creation is protected by the profile lock and an atomic write.
The same value is passed to every CLI, GUI and other shared-App client,
including after a restart or account change. Preference writes do not rotate
it, and public configuration snapshots omit it. The implementation is in
[`src/config.rs`](../src/config.rs) and [`src/app.rs`](../src/app.rs).

| Value | Source | Request use |
|---|---|---|
| Application device ID | Generated once per profile, in StartLive's UUID/PID format | Stable `buvid` header; fallback `buvid3` when no account device cookie exists |
| Browser device cookies | Bilibili's official SPI/session refresh | Authenticated Cookie header, including `buvid3` and `buvid4` |

Passport QR login can return valid account cookies without the device cookies
needed by subsequent broadcast requests. Before the first start, the shared
App initializes missing or empty `buvid3`/`buvid4` through the existing official
session-refresh path and persists the result in the same credential store.
It then creates the broadcast client from the saved cookie. Later polling
clients reuse those cookies and the separate application device ID. Existing
complete device cookies are retained.

Anonymous login QR requests carry public application/device metadata, as in
StartLive, without inheriting saved account credentials. APP and WEB requests
use their respective protocol header profiles; APP requests no longer receive
additional browser Origin and Referer headers.

Initialization and persistence finish before any delay change or start
request. If either fails, the operation stops and reports the error.
The internal `_private_cookie` handoff is removed from the public result.

The ready challenge includes `verification.start_args`, with the resolved
room and area, the requested protocol and selection strategy, and an
`expected_uid` guard. After the platform confirms verification, the GUI submits
these exact arguments. The shared App checks `expected_uid` while holding its
account lease, before performing further effects. If another frontend selected
a different UID, the continuation stops and asks for a new start using the
selected account.

## Failure and cancellation

A registration or report failure returns an error before a ready QR code is
shown. A pending response keeps the existing challenge visible. An expired
or failed V2 response ends automatic polling and removes the expired QR.
Cancelling clears the challenge; a late polling response cannot continue it.
A new start also clears the old challenge before it is queued.

Automatic polling is bounded to three minutes. A polling request is queued
only while the GUI operation queue is idle. Transport errors appear in the
verification panel and never count as platform confirmation. After the time
limit, the user can explicitly check again or cancel and request a new
challenge.

## Verification evidence and user check

The regression suite exercises real HTTP request serialization against local
fixture servers. It checks V1/V2 order, original report fields, the signature
scope, registration/report errors, successful and unsuccessful polling, and
device continuity after reconstructing clients from refreshed cookies. GUI
fixtures cover cancellation, old responses and continuation with changed
on-screen room preferences. An App fixture checks account changes before a
verified retry reaches any network request.

The preceding `0.1.3-0.2` Linux candidate passed **142 tests** with Rust 1.100.0
and `cargo test --locked --offline`: 126 library tests, 3 CLI tests and 13
integration tests. Formatting and the optimized release build also passed.
The new checks cover persistent application identity, concurrent first
creation, account/anonymous isolation, and native-header continuity after SPI
refresh and frontend recreation. These automated checks use synthetic
credentials. The earlier 139-test result in
[VALIDATION.md](VALIDATION.md) belongs to the preceding runtime snapshot.

A later GUI timeout-boundary patch passed **144 tests**: 128 library tests,
3 CLI tests and 13 integration tests. Its formatting check and optimized
release build also passed. These checks ran on `8cc0ca5` plus the `src/gui.rs`
patch subsequently committed as `8b81e0f`. That artifact was installed as
local package `0.1.3-0.3`, with its package-manager version and executable
SHA-256 confirmed. Those checks and the installation identity remain in the
JSON receipt as historical evidence. Installation made no account API
requests, and the real GUI was not started; phone verification and
broadcasting were not accepted on that candidate.

The current `0.1.3-0.4` candidate was built from clean source `66c7346` and
passed **144 tests**: 128 library tests, 3 CLI tests and 13 integration tests.
The test stage completed in 11.99 seconds and the optimized release build in
56.54 seconds. Its installed executable SHA-256 matches the build artifact,
and `/usr/share/doc/bilibilive/build-receipt.json` matches the build receipt.
Installation made no account API requests and did not start the real GUI.
The earlier GUI and reference processes remain stopped, and the expired
reference QR image has been removed. Real account authentication, phone
verification and broadcasting remain untested on `0.1.3-0.4`; these local
checks do not establish that the original phone failure has been resolved.

The following real-account observations have a narrower scope than a complete
acceptance pass. Relative times are measured from the captured QR image.

| Installed build | Observation | Conclusion |
|---|---|---|
| `0.1.3-0.1` | Saved account authenticated; `live.start` requested V2 (`60043`) verification | Account authentication worked; opening the room still required verification |
| `0.1.3-0.1` | Fresh `moreInfo` returned `code=0`, existing realname status `1`, and a challenge account matching the saved account | The challenge initialized successfully |
| `0.1.3-0.1` | User reported entering the phone's camera/face screen, followed by authentication failure; at 44.61 s, `moreInfo` still succeeded and polling remained `status=0` | This attempt did not fail because its QR had already expired; platform verification had not completed |
| `0.1.3-0.2` | Running executable matched the recorded build; `auth.validate` returned `authenticated=true` | The updated installation could authenticate the saved account |
| `0.1.3-0.2` | At 0.78 s, `moreInfo` succeeded and polling returned `status=0` | The corrected session obtained a valid new challenge |
| `0.1.3-0.2` | No phone result was received before the check at 125.07 s; `moreInfo` then reported expiry and polling returned `status=2` | The challenge expired while awaiting feedback; the candidate's phone-verification outcome remains unconfirmed |
| `0.1.3-0.2` | At about 20:02 UTC, the user again reported “测试了，不行！” (tested it; still does not work) | Acceptance remains unsuccessful. This report was not clearly associated with a freshly created and sampled challenge, so its cause cannot be attributed to QR expiry |

### Direct StartLive-function diagnostic

At about 20:07 UTC, an independent diagnostic directly invoked the original
session, signing, start, registration, report, polling and codec functions
from the fixed StartLive commit cited above. It reused the same saved account
and native application device ID. This exercised upstream protocol functions;
it did not run or validate the complete StartLive GUI.

The first reference attempt received HTTP 200 for start (`60043`),
registration (`0`) and `ReportFaceRecognition` (`0`). Polling remained at
`status=0` through 109.8 seconds and returned `status=2` at 121.22 seconds;
`verified` remained false. The user later reported that the displayed white
reference QR was expired. There was no confirmation of a completed phone
attempt with that code, so this is neither a reproduction of the original
mobile authentication error nor a successful reference acceptance.

A second reference QR was then displayed. Start, registration and report
again returned the same codes. Polling returned `status=0` at 0.74, 16.40,
32.07, 47.83, 63.49, 79.21 and 94.87 seconds. During this attempt, the user
explicitly reported “请求达到上限了”. The reference job was cancelled at about
100.5 seconds, its QR viewer was closed, and the bilibilive GUI used for the
tests was stopped. No platform verification pass or successful broadcast was
observed. Times in this paragraph and the preceding paragraph are elapsed
times reported by their respective diagnostic runs.

The limit is a phone-side user report; the recorded polling responses still
showed a pending challenge. Its recovery time is unknown. Account-side tests
are paused: no further account requests, replacement QR codes or device-ID
changes are being used to continue these attempts. Offline code and packaging
validation can continue. Real-account acceptance must resume with the user
after the restriction has cleared.

These observations do not establish that mixing device identities caused the
original phone failure, or that separating them has resolved it. A valid new
challenge, passing fixtures, and an expired unanswered challenge do not
replace the required real-account result.

### Remaining acceptance

After the restriction has cleared, check the installed application:

1. Confirm that the running executable matches the candidate receipt and that
   the selected account validates successfully.
2. When the phone is ready, cancel any expired challenge and request a fresh
   start. Scan it with the Bilibili mobile application using the intended
   account, then complete the requested verification.
3. Confirm a passed challenge from the platform. For V2 this requires
   `data.status=1`; for V1 it requires `is_identified=true` or `1`.
4. Observe the desktop automatically continue the original start request.
   Check `live.start` success and the room's actual `live_status=1`, then
   obtain the user's confirmation that login and broadcasting worked.

Acceptance passes only after all four steps. Formal publication also requires
the native packaging gates to pass; a local installation or a workflow with
`publish=false` is not a published release.

Packaging results below are snapshots, separate from real-account acceptance;
older preview observations remain in the JSON receipt. In preview
[`44946465336`](https://github.com/LIghtJUNction/bilibilive/actions/runs/44946465336)
at source `ed807c2`, Linux and macOS ARM jobs completed successfully, including
ARM installation and update/restart checks. Intel's native payload step passed
at 20:58:38 UTC, while its later Rust gates were incomplete at the recorded
observation. That run's Windows source still contained the installation path
bug, so this was not an all-platform pass.

In preview
[`44949165435`](https://github.com/LIghtJUNction/bilibilive/actions/runs/44949165435)
at clean source `66c7346`, the 21:08:57 UTC snapshot showed the early OBS
installation integration step passing on all four platforms. Windows passed
all 14 tests, including ordinary and verbatim paths, rejection of a bare
drive prefix, and junction rejection. Linux's full job had succeeded; the
other three platforms were in native payload step 15. This `publish=false`
run had not completed every platform's gates at that snapshot. There is no
formal `v0.1.3` release in this record.

The final outcome of that full preview was **failure**. Linux and both macOS
jobs succeeded. Windows passed its native media build, application tests and
optimized build, and installer/GUI/media gate, but the update/restart probe
failed at 21:51:52 UTC; the Windows job ended in failure at 21:52:06 UTC.
Its containment check compared ordinary and extended Windows paths as
different identities and stopped before the downloaded-package hash check. The
official helper log showed application of fixture version `0.0.2` and launch
with `--confirm`; the original probe result remains a failure.

Commit `2c4cf63` corrected the probe's path comparison. Focused workflow
[`44954438960`](https://github.com/LIghtJUNction/bilibilive/actions/runs/44954438960)
completed successfully on Windows, macOS ARM and macOS Intel, exercising
packaging fixtures and the private SDK update/restart probe. All three
receipts were checked: SDK `1.2.161`, old process exit code `0`, replacement
version `0.0.2`, a changed process ID, an executable SHA-256 matching its
package, and all private processes exited. These probes did not use
application accounts. The final focused job completed at 22:03:44 UTC.
The Windows fixture log recorded 25 passing tests in 3.506 seconds with no
skips, including both Windows-specific checks.

This follow-up covered the update verification tooling and did not rerun the
complete release workflow. Application sources and build inputs are unchanged
from `66c7346`, so the installed candidate remains `0.1.3-0.4`. Account tests
remain paused without confirmation that the mobile restriction has cleared.
The original phone failure has not been shown to be resolved, and real login,
face verification and broadcasting still require the user's acceptance.
At 22:05 UTC, the latest formal release remained `v0.1.2`, and the remote
`v0.1.3` tag did not exist.

If the phone reports an error, record its visible wording and whether the
camera/face screen opened, together with the current sanitized platform
status. Do not include cookies, QR URLs or tokens, stream keys, or realname
response bodies: `moreInfo` can contain personal identity information. An
expired challenge should be replaced only when the user is ready to complete
the next attempt.
