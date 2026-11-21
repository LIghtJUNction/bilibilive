use bilibilive::{
    app::{App, redact},
    schema,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use zclap::{Arg, Command, Event, Style};

const CLI: Command = Command::new("bilibilive")
    .about("Native Bilibili studio · Project Z · CLI / MCP / zgpui / zuse")
    .args(&[
        Arg::flag("help")
            .short('h')
            .help("Show commands and options"),
        Arg::flag("version").short('V').help("Print version"),
        Arg::flag("json").help("Print compact JSON"),
        Arg::flag("read-only").help("Disable operations that change state"),
        Arg::flag("show-secrets").help("Explicitly print returned stream keys"),
        Arg::flag("demo").help("Open the native GUI with offline sample data"),
        Arg::flag("check").help("Preflight a zuse script without executing it"),
        Arg::flag("compat").help("Enable unauthenticated legacy start/stop routes on loopback"),
        Arg::flag("noupdate").help("Disable startup release checks"),
        Arg::flag("squirrel-firstrun").help("Accept the legacy Windows first-run marker"),
        Arg::value("config-dir").help("Use an isolated preferences directory"),
        Arg::value("endpoint")
            .help("Call an existing loopback HTTP studio; authenticates with BILIBILIVE_API_TOKEN"),
        Arg::value("cookie-file")
            .help("Read session cookies from a file or - (stdin); do not save them"),
        Arg::value("args").help("Operation arguments as a JSON object, or - for stdin"),
        Arg::value("arg")
            .repeat()
            .help("Add an operation argument as key=value"),
        Arg::value("listen").help("Loopback HTTP address (default 127.0.0.1:18779)"),
        Arg::value("host")
            .alias("web.host")
            .help("Legacy loopback HTTP host"),
        Arg::value("port")
            .alias("web.port")
            .help("Legacy HTTP port"),
        Arg::value("name").help("Account or converted scene collection name"),
        Arg::value("title").help("Room title"),
        Arg::value("area").help("Live category ID"),
        Arg::value("room").help("Room ID"),
        Arg::value("protocol").help("rtmp, srt or srt-only"),
        Arg::value("strategy").help("fallback or only"),
        Arg::value("message").help("Chat message or verification result"),
        Arg::value("announcement").help("Room announcement"),
        Arg::value("path").help("Local cover or input path"),
        Arg::value("input").help("LiveHime input JSON file"),
        Arg::value("output").help("New OBS scene collection JSON file"),
        Arg::value("uid").help("Exact target account UID"),
        Arg::value("session-id").help("Invitation session ID from its event"),
        Arg::value("source").help("Invitation source from its event"),
        Arg::value("seconds").help("Stream delay in seconds"),
        Arg::value("mode").help("classic, video or video_link"),
        Arg::positional("words").repeat().help("Command and action"),
    ]);

fn help() -> Result<(), String> {
    let mut text = String::new();
    Style::plain()
        .help(&CLI, &mut text)
        .map_err(|e| e.to_string())?;
    println!(
        "{text}\nCommands:\n  gui [--demo] [--listen ADDR]   Native zgpui studio; optional shared HTTP session\n  login [--name NAME]            Scan a login QR code\n  status / start / stop / stream Room and broadcast control\n  room / area / account / obs    Shared named operations\n  pk / rtc / danmaku / convert   Interaction, media and scene tools\n  rtc preview-local             Keep a local OBS preview open until Ctrl-C\n  call METHOD --args JSON        Any operation listed by schema\n  schema                        Full command and MCP schemas\n  mcp                           MCP server over stdio\n  serve                         Persistent loopback MCP and control API\n  zuse run FILE                  Run native .zuse functions/modules\n  zuse check FILE                Preflight without effects\n  zuse eval SCRIPT               Run an inline native program\n  doctor                        Local diagnostics\n\nExamples:\n  bilibilive room update --title 'Today' --area 235\n  bilibilive start --protocol srt --strategy fallback\n  bilibilive call obs.configure --args -\n  bilibilive gui --listen 127.0.0.1:18779\n  bilibilive --endpoint http://127.0.0.1:18779 rtc status\n  bilibilive --read-only mcp\n\nCredentials: OS keyring, BILIBILIVE_COOKIE, or --cookie-file.\nHTTP authentication: BILIBILIVE_API_TOKEN (24+ characters).\nWithout --endpoint, media/event connections belong to this CLI process and close when it exits.\nUse `bilibilive schema` for exact argument types and advanced operations."
    );
    Ok(())
}

fn stdin_text() -> Result<String, String> {
    let mut data = String::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > 1024 * 1024 {
        return Err("Input exceeds 1 MiB".into());
    }
    Ok(data)
}
fn scalar(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!(text))
}
fn print_result(mut value: Value, compact: bool, secrets: bool) -> Result<(), String> {
    let complete = bilibilive::app::is_success(&value);
    if !secrets {
        redact(&mut value);
    }
    println!(
        "{}",
        if compact {
            value.to_string()
        } else {
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        }
    );
    if complete {
        Ok(())
    } else {
        Err("Operation did not complete; see its structured result".into())
    }
}

const MAX_API_BODY: u64 = 16 * 1024 * 1024;

struct Endpoint {
    client: reqwest::blocking::Client,
    address: SocketAddr,
    token: String,
}

fn endpoint_address(input: &str) -> Result<SocketAddr, String> {
    let url = reqwest::Url::parse(input).map_err(|_| "--endpoint must be an HTTP loopback URL")?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("--endpoint accepts only an HTTP loopback origin without credentials, path, query or fragment".into());
    }
    let host = url
        .host_str()
        .ok_or("--endpoint needs a literal loopback IP")?;
    let ip: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(
            |_| "--endpoint needs a literal loopback IP, for example http://127.0.0.1:18779",
        )?;
    if !ip.is_loopback() {
        return Err("--endpoint only connects to loopback addresses".into());
    }
    let port = url
        .port_or_known_default()
        .filter(|port| *port > 0)
        .ok_or("--endpoint needs a valid port")?;
    Ok(SocketAddr::new(ip, port))
}

impl Endpoint {
    fn new(input: &str, token: String) -> Result<Self, String> {
        let address = endpoint_address(input)?;
        if token.len() < 24 || token.bytes().any(|byte| byte.is_ascii_control()) {
            return Err("BILIBILIVE_API_TOKEN must contain at least 24 characters without control characters".into());
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| format!("Cannot create the local API client: {e}"))?;
        Ok(Self {
            client,
            address,
            token,
        })
    }

    fn call(&self, method: &str, arguments: Value) -> Result<Value, String> {
        let body = serde_json::to_vec(&json!({"method":method,"arguments":arguments}))
            .map_err(|e| e.to_string())?;
        if body.len() > 1024 * 1024 {
            return Err("Local API request exceeds 1 MiB".into());
        }
        let response = self.client.post(format!("http://{}/api/call", self.address))
            .header(reqwest::header::HOST, self.address.to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .bearer_auth(&self.token).body(body).send()
            .map_err(|e| format!("Local API request failed: {e}. Check the target session before retrying an operation that changes state."))?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_API_BODY)
        {
            return Err("Local API response exceeds 16 MiB".into());
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_API_BODY + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_API_BODY {
            return Err("Local API response exceeds 16 MiB".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            format!(
                "Local API returned non-JSON data (HTTP {})",
                status.as_u16()
            )
        })?;
        // An incomplete operation keeps its structured result and a failing CLI
        // exit status. A verification challenge is not a transport failure.
        if status.is_success() || status == reqwest::StatusCode::CONFLICT {
            return Ok(value);
        }
        let reason = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Request rejected");
        Err(format!("Local API HTTP {}: {reason}", status.as_u16()))
    }
}

enum Caller {
    Local(App),
    Remote { endpoint: Endpoint, read_only: bool },
}

impl Caller {
    fn call(&self, method: &str, mut arguments: Value) -> Result<Value, String> {
        match self {
            Self::Local(app) => app.call(method, arguments),
            Self::Remote {
                endpoint,
                read_only,
            } => {
                let operation = schema::validate(method, &arguments)?;
                if *read_only {
                    if method == "zuse.run" {
                        arguments["read_only"] = true.into();
                    } else if !operation.read_only {
                        return Err(format!("{method} is disabled in read-only mode"));
                    }
                }
                endpoint.call(method, arguments)
            }
        }
    }
    fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

fn ephemeral_note(caller: &Caller, method: &str, result: &Value) {
    if caller.is_remote() {
        return;
    }
    let transient = |name: &str| {
        matches!(
            name,
            "rtc.connect"
                | "rtc.publish"
                | "rtc.subscribe"
                | "rtc.capture.start"
                | "rtc.preview"
                | "rtc.local.preview"
                | "danmaku.connect"
                | "obs.watch.start"
                | "pk.match"
                | "pk.invite"
                | "pk.accept"
                | "pk.link.connect"
                | "pk.link.start"
        )
    };
    if transient(method)
        || (method == "zuse.run"
            && result["steps"].as_array().is_some_and(|steps| {
                steps
                    .iter()
                    .any(|step| step["operation"].as_str().is_some_and(transient))
            }))
    {
        eprintln!(
            "This local CLI process owns its RTC, preview and event connections; they close on exit. Use --endpoint with a running `serve` or `gui --listen` session for subsequent commands, or `rtc preview-local` for a foreground preview."
        );
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn interrupt_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

#[cfg(windows)]
unsafe extern "system" fn interrupt_console(event: u32) -> i32 {
    if matches!(event, 0 | 1) {
        INTERRUPTED.store(true, Ordering::Relaxed);
        1
    } else {
        0
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

struct InterruptGuard {
    #[cfg(unix)]
    previous: [libc::sigaction; 2],
}

impl InterruptGuard {
    fn install() -> Result<Self, String> {
        INTERRUPTED.store(false, Ordering::Relaxed);
        #[cfg(unix)]
        {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = interrupt_signal as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            let mut previous: [libc::sigaction; 2] = unsafe { std::mem::zeroed() };
            for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
                if unsafe { libc::sigaction(signal, &action, &mut previous[index]) } != 0 {
                    if index > 0 {
                        unsafe {
                            libc::sigaction(libc::SIGINT, &previous[0], std::ptr::null_mut());
                        }
                    }
                    return Err(format!(
                        "Cannot install shutdown handler: {}",
                        io::Error::last_os_error()
                    ));
                }
            }
            return Ok(Self { previous });
        }
        #[cfg(windows)]
        if unsafe { SetConsoleCtrlHandler(Some(interrupt_console), 1) } == 0 {
            return Err(format!(
                "Cannot install shutdown handler: {}",
                io::Error::last_os_error()
            ));
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }
    fn cancelled(&self) -> bool {
        INTERRUPTED.load(Ordering::Relaxed)
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
            unsafe {
                libc::sigaction(signal, &self.previous[index], std::ptr::null_mut());
            }
        }
        #[cfg(windows)]
        unsafe {
            SetConsoleCtrlHandler(Some(interrupt_console), 0);
        }
    }
}

fn foreground_preview(caller: &Caller, arguments: Value, compact: bool) -> Result<(), String> {
    let interrupt = InterruptGuard::install()?;
    print_result(caller.call("rtc.local.preview", arguments)?, compact, false)?;
    eprintln!("Local OBS preview is running. Press Ctrl-C to stop this preview.");
    while !interrupt.cancelled() {
        std::thread::sleep(Duration::from_millis(100));
    }
    print_result(
        caller.call("rtc.local.preview.stop", json!({}))?,
        compact,
        false,
    )
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut parser = CLI.parse(&argv);
    let mut flags = std::collections::BTreeSet::new();
    let mut values = BTreeMap::new();
    let mut words = Vec::new();
    let mut extra = Vec::new();
    while let Some(event) = parser.next().map_err(|e| e.to_string())? {
        match event {
            Event::Flag { arg: "help" } => return help(),
            Event::Flag { arg: "version" } => {
                println!("bilibilive {}", bilibilive::VERSION);
                return Ok(());
            }
            Event::Flag { arg } => {
                flags.insert(arg.to_owned());
            }
            Event::Value {
                arg: "words",
                value,
            } => words.push(value.to_owned()),
            Event::Value { arg: "arg", value } => extra.push(value.to_owned()),
            Event::Value { arg, value } => {
                values.insert(arg.to_owned(), value.to_owned());
            }
            _ => (),
        }
    }
    parser.finish().map_err(|e| e.to_string())?;
    let command = words.first().map(String::as_str).unwrap_or("gui");
    if command == "schema" {
        return print_result(schema::catalogue(), flags.contains("json"), true);
    }
    if command == "help" {
        return help();
    }
    if command == "version" {
        println!("bilibilive {}", bilibilive::VERSION);
        return Ok(());
    }
    #[cfg(feature = "gui")]
    let legacy_web = values.contains_key("host") || values.contains_key("port");
    let listen = values.get("listen").cloned().unwrap_or_else(|| {
        format!(
            "{}:{}",
            values
                .get("host")
                .map(String::as_str)
                .unwrap_or("127.0.0.1"),
            values.get("port").map(String::as_str).unwrap_or("18779")
        )
    });
    let caller = if let Some(endpoint) = values.get("endpoint") {
        if matches!(command, "gui" | "mcp" | "serve") {
            return Err("--endpoint connects CLI operations to an existing session; gui, mcp and serve create their own local session".into());
        }
        if values.contains_key("config-dir") {
            return Err(
                "--endpoint uses the server's profile; pass --config-dir when starting that server"
                    .into(),
            );
        }
        let importing = command == "auth.import"
            || (command == "auth" && words.get(1).map(String::as_str) == Some("import"))
            || (command == "call" && words.get(1).map(String::as_str) == Some("auth.import"));
        if values.contains_key("cookie-file") && !importing {
            return Err("--cookie-file with --endpoint is only available for explicit auth import; the endpoint otherwise uses its selected account".into());
        }
        let token = std::env::var("BILIBILIVE_API_TOKEN").map_err(
            |_| "Set BILIBILIVE_API_TOKEN to the token of the target serve or gui --listen session",
        )?;
        Caller::Remote {
            endpoint: Endpoint::new(endpoint, token)?,
            read_only: flags.contains("read-only"),
        }
    } else {
        let app = App::new(values.get("config-dir").map(PathBuf::from))?
            .with_read_only(flags.contains("read-only"));
        if let Some(path) = values.get("cookie-file") {
            let cookie = if path == "-" {
                stdin_text()?
            } else {
                std::fs::read_to_string(path).map_err(|e| format!("Cookie file: {e}"))?
            };
            app.set_cookie_override(cookie.trim().to_owned())?;
        }
        Caller::Local(app)
    };
    if command == "mcp" {
        let Caller::Local(app) = caller else {
            unreachable!()
        };
        return bilibilive::mcp::stdio(app);
    }
    if command == "serve" {
        if flags.contains("demo") {
            return Err("Demo mode cannot start a network listener".into());
        }
        let Caller::Local(app) = caller else {
            unreachable!()
        };
        return bilibilive::http::serve(
            app,
            &listen,
            std::env::var("BILIBILIVE_API_TOKEN").ok(),
            flags.contains("compat"),
        );
    }
    if command == "gui" {
        #[cfg(feature = "gui")]
        {
            let Caller::Local(app) = caller else {
                unreachable!()
            };
            if legacy_web || values.contains_key("listen") {
                if flags.contains("demo") {
                    return Err("Demo mode cannot start a network listener".into());
                }
                bilibilive::http::spawn(
                    app.clone(),
                    &listen,
                    std::env::var("BILIBILIVE_API_TOKEN").ok(),
                    flags.contains("compat"),
                )?;
            }
            return bilibilive::gui::run(app, flags.contains("demo"));
        }
        #[cfg(not(feature = "gui"))]
        return Err(
            "This build has no native GUI; install the default build or use CLI/MCP commands"
                .into(),
        );
    }
    if command == "zuse" {
        let action = words.get(1).map(String::as_str).unwrap_or("schema");
        if action == "schema" {
            return print_result(schema::catalogue(), flags.contains("json"), true);
        }
        let input = words
            .get(2)
            .ok_or("zuse run/check needs a path; zuse eval needs source text")?;
        let (source, base) = match action {
            "run" | "check" => {
                let path = std::path::absolute(input).map_err(|e| e.to_string())?;
                (
                    std::fs::read_to_string(&path).map_err(|e| e.to_string())?,
                    path.parent().map(PathBuf::from),
                )
            }
            "eval" => (input.to_owned(), None),
            _ => return Err("Use zuse run, check, eval or schema".into()),
        };
        let method = if action == "check" || flags.contains("check") {
            "zuse.check"
        } else {
            "zuse.run"
        };
        let mut arguments = json!({"script":source,"read_only":flags.contains("read-only")});
        if let Some(base) = base {
            arguments["base"] = json!(base);
        }
        let result = caller.call(method, arguments)?;
        ephemeral_note(&caller, method, &result);
        return print_result(
            result,
            flags.contains("json"),
            flags.contains("show-secrets"),
        );
    }
    if command == "login" {
        let qr = caller.call("auth.qr.create", json!({}))?;
        let url = qr["url"].as_str().ok_or("QR response omitted url")?;
        let key = qr["qrcode_key"]
            .as_str()
            .ok_or("QR response omitted qrcode_key")?;
        eprintln!("Scan with the Bilibili app: {url}");
        #[cfg(feature = "gui")]
        if let Ok(code) = qrcodegen::QrCode::encode_text(url, qrcodegen::QrCodeEcc::Low) {
            for y in (-2..code.size() + 2).step_by(2) {
                let mut line = String::new();
                for x in -2..code.size() + 2 {
                    line.push(match (code.get_module(x, y), code.get_module(x, y + 1)) {
                        (true, true) => ' ',
                        (false, false) => '█',
                        (false, true) => '▀',
                        (true, false) => '▄',
                    });
                }
                eprintln!("{line}");
            }
        }
        for _ in 0..90 {
            let value=caller.call("auth.qr.poll",json!({"qrcode_key":key,"name":values.get("name").map(String::as_str).unwrap_or("default")}))?;
            if value["status"] == "authenticated" {
                return print_result(value, flags.contains("json"), false);
            }
            if value["status"] == "expired" {
                return Err("Login QR expired; run login again".into());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        return Err("Login timed out".into());
    }
    if command == "obs" && words.get(1).map(String::as_str) == Some("watch") {
        let interrupt = InterruptGuard::install()?;
        if let Caller::Local(app) = &caller {
            return app.watch_obs(&INTERRUPTED);
        }
        print_result(
            caller.call("obs.watch.start", json!({}))?,
            flags.contains("json"),
            false,
        )?;
        eprintln!("Watching OBS in the target session. Press Ctrl-C to stop its observer.");
        while !interrupt.cancelled() {
            std::thread::sleep(Duration::from_millis(100));
        }
        return print_result(
            caller.call("obs.watch.stop", json!({}))?,
            flags.contains("json"),
            false,
        );
    }
    if command == "danmaku" && words.get(1).map(String::as_str) == Some("watch") {
        let interrupt = InterruptGuard::install()?;
        caller.call("danmaku.connect", json!({}))?;
        let result = (|| {
            while !interrupt.cancelled() {
                let event = caller.call("danmaku.read", json!({"limit":1}))?;
                if event["events"].as_array().is_some_and(|v| !v.is_empty()) {
                    print_result(event, true, false)?;
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Ok(())
        })();
        if !caller.is_remote() {
            let _ = caller.call("danmaku.disconnect", json!({}));
        }
        return result;
    }
    let method = match command {
        "call" => words
            .get(1)
            .cloned()
            .ok_or("call requires an operation name")?,
        "status" => "live.status".into(),
        "start" => "live.start".into(),
        "stop" => "live.stop".into(),
        "stream" => "live.stream".into(),
        "doctor" => "doctor".into(),
        name if name.contains('.') => name.into(),
        name => format!(
            "{name}.{}",
            words
                .get(1)
                .ok_or("Command requires an action; see --help")?
        ),
    };
    let mut args = if let Some(raw) = values.get("args") {
        let raw = if raw == "-" {
            stdin_text()?
        } else {
            raw.to_owned()
        };
        serde_json::from_str::<Value>(&raw).map_err(|e| format!("--args: {e}"))?
    } else {
        json!({})
    };
    let object = args.as_object_mut().ok_or("--args must be a JSON object")?;
    for name in [
        "name",
        "title",
        "area",
        "room",
        "protocol",
        "strategy",
        "message",
        "announcement",
        "path",
        "input",
        "output",
        "uid",
        "session-id",
        "source",
        "seconds",
        "mode",
    ] {
        if let Some(value) = values.get(name) {
            let key = match name {
                "area" => "area_id",
                "room" => "room_id",
                "session-id" => "session_id",
                _ => name,
            };
            let value = if ["area", "room", "uid", "source", "seconds"].contains(&name) {
                scalar(value)
            } else {
                json!(value)
            };
            object.insert(key.into(), value);
        }
    }
    for item in extra {
        let (key, value) = item.split_once('=').ok_or("--arg must use key=value")?;
        object.insert(key.into(), scalar(value));
    }
    if method == "auth.import" && !object.contains_key("cookie") {
        let cookie = values
            .get("cookie-file")
            .ok_or("auth import requires --cookie-file or a cookie in --args")?;
        if cookie == "-" {
            return Err("Use --args - to import a Cookie header from stdin".into());
        }
        object.insert(
            "cookie".into(),
            json!(
                std::fs::read_to_string(cookie)
                    .map_err(|e| e.to_string())?
                    .trim()
            ),
        );
    }
    if command == "rtc" && words.get(1).map(String::as_str) == Some("preview-local") {
        return foreground_preview(&caller, args, flags.contains("json"));
    }
    let result = caller.call(&method, args)?;
    ephemeral_note(&caller, &method, &result);
    print_result(
        result,
        flags.contains("json"),
        flags.contains("show-secrets"),
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bilibilive: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    #[test]
    fn endpoint_accepts_only_literal_loopback_origins() {
        assert_eq!(
            endpoint_address("http://127.0.0.1:18779/").unwrap(),
            "127.0.0.1:18779".parse().unwrap()
        );
        assert_eq!(
            endpoint_address("http://[::1]:18779").unwrap(),
            "[::1]:18779".parse().unwrap()
        );
        for input in [
            "https://127.0.0.1:18779",
            "http://user:secret@127.0.0.1:18779",
            "http://192.168.1.2:18779",
            "http://localhost:18779",
            "http://127.0.0.1:18779/api/call",
            "http://127.0.0.1:18779/?token=hidden",
            "http://127.0.0.1:18779/#fragment",
        ] {
            assert!(
                endpoint_address(input).is_err(),
                "unexpected endpoint: {input}"
            );
        }
    }

    #[test]
    fn endpoint_routes_authenticated_calls_and_preserves_partial_results() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let token = "local-fixture-token-0123456789";
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "POST /api/call HTTP/1.1\r\n");
            let mut headers = BTreeMap::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.trim_end().split_once(':').unwrap();
                headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
            }
            assert_eq!(headers["authorization"], format!("Bearer {token}"));
            assert_eq!(headers["host"], address.to_string());
            let mut bytes = vec![0; headers["content-length"].parse::<usize>().unwrap()];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({"method":"live.start","arguments":{"protocol":"srt","strategy":"only"}})
            );
            drop(reader);
            let body =
                json!({"status":"live_without_requested_protocol","available":false,"room_id":42})
                    .to_string();
            write!(stream,"HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let endpoint = Endpoint::new(&format!("http://{address}"), token.into()).unwrap();
        let value = endpoint
            .call("live.start", json!({"protocol":"srt","strategy":"only"}))
            .unwrap();
        assert_eq!(value["room_id"], 42);
        assert_eq!(value["available"], false);
        assert!(!bilibilive::app::is_success(&value));
        worker.join().unwrap();
    }
}
