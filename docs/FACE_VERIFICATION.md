# Face verification and broadcast continuation

bilibilive prepares Bilibili's verification session before presenting a QR
code. A successful phone scan alone does not start the broadcast: the platform
must confirm that the identity check has passed.

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

Passport QR login can return valid account cookies without the device cookies
needed by subsequent broadcast requests. Before the first start, the shared
App initializes missing or empty `buvid3`/`buvid4` through the existing official
session-refresh path and persists the result in the same credential store.
It then creates the broadcast client from the saved cookie. Later polling
clients use that same saved device identity. Existing complete device cookies
are retained.

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

The [Linux validation record](VALIDATION.md)
identifies the tested runtime commit and the 139 passing tests. These tests
use synthetic credentials. Phone-side identity verification and a real
authenticated broadcast remain untested for this fix.

To check the installed application:

1. Close the old verification window and reopen the updated application.
2. Request a fresh start, then scan the new QR code with the Bilibili mobile
   application.
3. Complete the steps requested by Bilibili. The desktop continues only after
   the platform confirms verification.
4. If the phone still reports an error, record its visible message and the
   desktop operation result. Keep cookies, QR tokens and stream keys out of
   the report.
