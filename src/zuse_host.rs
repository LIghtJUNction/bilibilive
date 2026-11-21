//! Bounded native zuse programs using the same operation contract as CLI and MCP.
//!
//! Imports and functions are expanded by zuse itself. Every resulting call is
//! parsed, typed and checked before the first application operation is executed.
use crate::{
    app::{App, is_success, redact},
    schema,
};
use serde_json::{Map, Number, Value, json};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};
use zuse::{
    language::{Parser, Value as ScriptValue},
    runtime::Options,
    script::{CancellationToken, ModuleSession, ProgramMode},
};

pub const MAX_STEPS: usize = 256;
pub const MAX_WAIT_MS: u64 = 60_000;

struct Step {
    operation: String,
    arguments: Value,
    read_only: bool,
    line: usize,
    column: usize,
}

struct Plan {
    steps: Vec<Step>,
    wait_ms: u64,
    functions: Vec<String>,
}

/// Run or inspect a complete program. Runtime failures are structured receipts:
/// already completed effects are preserved in `steps`, never silently rolled back.
pub fn execute(
    app: &App,
    source: &str,
    base: Option<&Path>,
    dry_run: bool,
    read_only: bool,
) -> Result<Value, String> {
    execute_with_cancel(
        app,
        source,
        base,
        dry_run,
        read_only,
        &CancellationToken::default(),
    )
}

/// Cancellation is checked between operations and at most every 20 ms in waits.
/// An already dispatched platform request is allowed to finish within its client
/// timeout; cancellation cannot undo a request accepted by the remote platform.
pub fn execute_with_cancel(
    app: &App,
    source: &str,
    base: Option<&Path>,
    dry_run: bool,
    read_only: bool,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let read_only = read_only || app.is_read_only();
    let plan = prepare(source, base, dry_run, read_only)?;
    if dry_run {
        return Ok(
            json!({"ok":true,"status":"planned","dry_run":true,"read_only":read_only,
            "total_steps":plan.steps.len(),"completed_steps":0,"wait_ms":plan.wait_ms,
            "max_steps":MAX_STEPS,"max_wait_ms":MAX_WAIT_MS,"functions":plan.functions,
            "steps":plan.steps.iter().enumerate().map(|(index,step)| describe_step(index,step)).collect::<Vec<_>>() }),
        );
    }
    Ok(execute_plan(&plan, cancellation, |operation, arguments| {
        app.call(operation, arguments.clone())
    }))
}

fn execute_plan(
    plan: &Plan,
    cancellation: &CancellationToken,
    mut dispatch: impl FnMut(&str, &Value) -> Result<Value, String>,
) -> Value {
    let started = Instant::now();
    let mut timer = Timer::default();
    let mut receipts = Vec::with_capacity(plan.steps.len());
    for (index, step) in plan.steps.iter().enumerate() {
        if cancellation.is_cancelled() {
            return receipt("cancelled", plan, receipts, started, Some(index + 1), None);
        }
        let command_started = Instant::now();
        let result = match step.operation.as_str() {
            "wait" => wait(
                step.arguments["ms"].as_u64().unwrap_or_default(),
                cancellation,
            ),
            "timer.start" => {
                timer.start();
                Ok(timer.status())
            }
            "timer.reset" => {
                timer.reset();
                Ok(timer.status())
            }
            "timer.stop" => {
                timer.stop();
                Ok(timer.status())
            }
            "timer.status" => Ok(timer.status()),
            operation => dispatch(operation, &step.arguments),
        };
        let duration = command_started.elapsed();
        timer.record(duration, !step.operation.starts_with("timer."));
        let mut entry = describe_step(index, step);
        // Receipts expose results, not a second copy of potentially secret inputs.
        entry.as_object_mut().unwrap().remove("arguments");
        entry["command_us"] = micros(duration).into();
        entry["total_elapsed_us"] = micros(started.elapsed()).into();
        match result {
            Ok(mut value) => {
                let blocked = matches!(step.operation.as_str(), "live.start" | "live.stream")
                    && (matches!(
                        value["status"].as_str(),
                        Some("verification_required" | "live_without_requested_protocol")
                    ) || value["available"].as_bool() == Some(false));
                let succeeded = is_success(&value);
                redact(&mut value);
                redact_challenges(&mut value);
                entry["ok"] = (blocked || succeeded).into();
                entry["result"] = value;
                if !blocked && !succeeded {
                    // Keep partial effects in the failed step, without counting
                    // it as completed or dispatching later operations.
                    return receipt(
                        "failed",
                        plan,
                        receipts,
                        started,
                        Some(index + 1),
                        Some(entry),
                    );
                }
                receipts.push(entry);
                if blocked {
                    let mut result = receipt(
                        "blocked",
                        plan,
                        receipts,
                        started,
                        (index + 1 < plan.steps.len()).then_some(index + 2),
                        None,
                    );
                    result["reason"] = "The platform requires verification or the requested stream protocol is unavailable; later steps were not run".into();
                    return result;
                }
            }
            Err(error) => {
                let cancelled = step.operation == "wait" && cancellation.is_cancelled();
                entry["ok"] = false.into();
                entry["error"] = error.into();
                return receipt(
                    if cancelled { "cancelled" } else { "failed" },
                    plan,
                    receipts,
                    started,
                    Some(index + 1),
                    Some(entry),
                );
            }
        }
    }
    // If the final remote action succeeded while cancellation arrived, its
    // success is retained; only unexecuted work is considered cancelled.
    receipt("completed", plan, receipts, started, None, None)
}

fn prepare(
    source: &str,
    base: Option<&Path>,
    dry_run: bool,
    read_only: bool,
) -> Result<Plan, String> {
    if source.len() > zuse::language::MAX_SCRIPT_BYTES {
        return Err("zuse program exceeds 1 MiB".into());
    }
    let options = Options {
        dry_run,
        read_only,
        max_steps: MAX_STEPS,
        max_wait_ms: MAX_WAIT_MS,
    };
    let session = ModuleSession::default();
    let prepared = match base {
        Some(base) => session.prepare_with_base(source, options, base),
        None => session.prepare(source, options),
    }
    .map_err(|error| format!("zuse preparation failed: {error}"))?;
    if prepared.mode != ProgramMode::Flat {
        return Err("bilibilive zuse accepts native calls, functions, imports and repeat blocks; use the standalone zuse runtime for scoped desktop programs".into());
    }
    let mut steps = Vec::new();
    let mut wait_ms = 0u64;
    // Collect the ENTIRE parser iterator before any operation can run. In
    // particular, a syntax error after a valid mutation cannot apply that mutation.
    for parsed in Parser::new(&prepared.source) {
        let call = parsed.map_err(|error| format!("zuse preflight failed: {error}"))?;
        if steps.len() == MAX_STEPS {
            return Err(format!("zuse accepts at most {MAX_STEPS} expanded steps"));
        }
        let operation = normalise(call.name);
        let at = || {
            format!(
                "expanded line {}, column {}",
                call.span.line, call.span.column
            )
        };
        if matches!(operation, "zuse.run" | "zuse.check") {
            return Err(format!("Recursive {operation} is not allowed at {}", at()));
        }
        let mut arguments = Map::new();
        let spec = if operation == "wait" || operation.starts_with("timer.") {
            None
        } else {
            Some(
                schema::find(operation)
                    .ok_or_else(|| format!("Unknown operation {operation} at {}", at()))?,
            )
        };
        for argument in call.args() {
            let rule = spec
                .as_ref()
                .and_then(|spec| spec.input["properties"].get(argument.name));
            let mut value = decode_value(argument.value)
                .map_err(|error| format!("{operation}.{} at {}: {error}", argument.name, at()))?;
            if rule.is_some_and(|rule| accepts_container(rule))
                && let Some(encoded) = value.as_str()
            {
                value = serde_json::from_str(encoded).map_err(|_| {
                    format!(
                        "{operation}.{} requires a quoted JSON object or array at {}",
                        argument.name,
                        at()
                    )
                })?;
            }
            if arguments.insert(argument.name.to_owned(), value).is_some() {
                return Err(format!("Duplicate argument {} at {}", argument.name, at()));
            }
        }
        let arguments = Value::Object(arguments);
        let is_read_only = match operation {
            "wait" => {
                if arguments.as_object().unwrap().len() != 1 {
                    return Err(format!("wait requires only ms at {}", at()));
                }
                let ms = arguments["ms"]
                    .as_u64()
                    .ok_or_else(|| format!("wait.ms must be a nonnegative integer at {}", at()))?;
                wait_ms = wait_ms
                    .checked_add(ms)
                    .filter(|ms| *ms <= MAX_WAIT_MS)
                    .ok_or_else(|| format!("Total zuse waits exceed {MAX_WAIT_MS} ms"))?;
                true
            }
            "timer.start" | "timer.reset" | "timer.stop" | "timer.status" => {
                if !arguments.as_object().unwrap().is_empty() {
                    return Err(format!("{operation} accepts no arguments at {}", at()));
                }
                true
            }
            _ => {
                schema::validate(operation, &arguments)
                    .map_err(|error| format!("{error} at {}", at()))?
                    .read_only
            }
        };
        if read_only && !is_read_only {
            return Err(format!(
                "Read-only zuse program cannot call {operation} at {}",
                at()
            ));
        }
        steps.push(Step {
            operation: operation.into(),
            arguments,
            read_only: is_read_only,
            line: call.span.line,
            column: call.span.column,
        });
    }
    if let Some(index) = steps
        .iter()
        .position(|step| step.operation == "updates.apply")
    {
        if index + 1 != steps.len() {
            return Err(format!(
                "updates.apply must be the final expanded step because it exits this host (expanded step {})",
                index + 1
            ));
        }
    }
    Ok(Plan {
        steps,
        wait_ms,
        functions: prepared.next_session.functions(),
    })
}

fn normalise(name: &str) -> &str {
    match name.strip_prefix("bilibilive.").unwrap_or(name) {
        "status" => "live.status",
        "start" => "live.start",
        "stop" => "live.stop",
        "stream" => "live.stream",
        name => name,
    }
}

fn accepts_container(rule: &Value) -> bool {
    match &rule["type"] {
        Value::String(kind) => kind == "object" || kind == "array",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "object" || kind == "array"),
        _ => false,
    }
}

fn decode_value(value: ScriptValue<'_>) -> Result<Value, String> {
    Ok(match value {
        ScriptValue::Bool(value) => Value::Bool(value),
        ScriptValue::Number(raw) => {
            let number = if let Ok(value) = raw.parse::<i64>() {
                Number::from(value)
            } else if let Ok(value) = raw.parse::<u64>() {
                Number::from(value)
            } else if raw.contains(['.', 'e', 'E']) {
                Number::from_f64(raw.parse::<f64>().map_err(|_| "Invalid numeric argument")?)
                    .ok_or("Numeric arguments must be finite")?
            } else {
                return Err("Integer argument is outside the 64-bit range".into());
            };
            Value::Number(number)
        }
        ScriptValue::String(_) | ScriptValue::Identifier(_) => {
            let mut bytes = vec![0u8; value.decoded_len().map_err(|error| error.to_string())?];
            Value::String(
                value
                    .decode_text(&mut bytes)
                    .map_err(|error| error.to_string())?
                    .to_owned(),
            )
        }
    })
}

fn describe_step(index: usize, step: &Step) -> Value {
    let mut arguments = step.arguments.clone();
    if step.operation == "config.set" {
        if let Some(key) = step.arguments["key"].as_str() {
            if key == "proxy" {
                arguments["value"] = "[redacted]".into();
            } else {
                // Preferences use a generic `value` slot. Apply the same
                // secret-field policy to its named setting before rendering.
                let mut setting = json!({key:arguments["value"].clone()});
                redact(&mut setting);
                arguments["value"] = setting[key].clone();
            }
        }
    }
    redact(&mut arguments);
    redact_challenges(&mut arguments);
    let mut value = json!({"index":index+1,"operation":step.operation,"arguments":arguments,"read_only":step.read_only,
        "expanded_line":step.line,"expanded_column":step.column});
    if step.operation == "updates.apply" {
        value["terminates_host"] = true.into();
    }
    value
}

fn redact_challenges(value: &mut Value) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "qrcode_key" | "access_key" | "sign" | "csrf" | "csrf_token"
                ) {
                    *value = "[redacted]".into();
                } else {
                    redact_challenges(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_challenges(value);
            }
        }
        _ => (),
    }
}

fn receipt(
    status: &str,
    plan: &Plan,
    steps: Vec<Value>,
    started: Instant,
    next_step: Option<usize>,
    failed_step: Option<Value>,
) -> Value {
    let terminal = if status == "completed" {
        steps.last()
    } else {
        failed_step.as_ref()
    }
    .filter(|entry| entry["operation"] == "updates.apply")
    .and_then(|entry| entry.get("result"))
    .cloned();
    let mut value = json!({"ok":status=="completed","status":status,"dry_run":false,"total_steps":plan.steps.len(),
        "completed_steps":steps.len(),"next_step":next_step,"failed_step":failed_step,"steps":steps,
        "planned_wait_ms":plan.wait_ms,"total_elapsed_us":micros(started.elapsed()),"functions":plan.functions});
    if let Some(terminal) = terminal {
        for key in [
            "local_sessions_retired",
            "local_shutdown_attempted",
            "room_changed",
            "obs_output_changed",
        ] {
            if let Some(field) = terminal.get(key) {
                value[key] = field.clone();
            }
        }
        if status == "completed"
            && is_success(&terminal)
            && terminal["quit_requested"] == true
            && terminal["exit_required"] == true
        {
            for key in [
                "quit_requested",
                "exit_required",
                "restart_requested",
                "restart_pending",
                "exit_timeout_seconds",
                "staged_id",
                "phase",
                "receipt_replayed",
            ] {
                if let Some(field) = terminal.get(key) {
                    value[key] = field.clone();
                }
            }
        }
    }
    value
}

fn wait(ms: u64, cancellation: &CancellationToken) -> Result<Value, String> {
    let started = Instant::now();
    let duration = Duration::from_millis(ms);
    while started.elapsed() < duration {
        if cancellation.is_cancelled() {
            return Err("zuse program cancelled while waiting".into());
        }
        thread::sleep(
            duration
                .saturating_sub(started.elapsed())
                .min(Duration::from_millis(20)),
        );
    }
    if cancellation.is_cancelled() {
        return Err("zuse program cancelled while waiting".into());
    }
    Ok(json!({"waited_ms":ms}))
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

#[derive(Default)]
struct Timer {
    started: Option<Instant>,
    stopped: Duration,
    command_total: Duration,
    commands: u64,
}
impl Timer {
    fn start(&mut self) {
        if self.started.is_none() {
            self.reset();
        }
    }
    fn reset(&mut self) {
        self.started = Some(Instant::now());
        self.stopped = Duration::ZERO;
        self.command_total = Duration::ZERO;
        self.commands = 0;
    }
    fn stop(&mut self) {
        if let Some(started) = self.started.take() {
            self.stopped = started.elapsed();
        }
    }
    fn record(&mut self, elapsed: Duration, counted: bool) {
        if counted {
            self.command_total = self.command_total.saturating_add(elapsed);
            self.commands = self.commands.saturating_add(1);
        }
    }
    fn status(&self) -> Value {
        json!({"running":self.started.is_some(),"command_us":0,"total_elapsed_us":micros(self.started.map_or(self.stopped,|start|start.elapsed())),
            "command_total_us":micros(self.command_total),"commands":self.commands})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        app: App,
        directory: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "bilibilive-zuse-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&directory);
            Self {
                app: App::new(Some(directory.clone())).unwrap(),
                directory,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn complete_preflight_prevents_earlier_side_effects() {
        let fixture = Fixture::new();
        for source in [
            "config.set(key=\"default_area\",value=999); room.update(titel=\"typo\")",
            "config.set(key=\"default_area\",value=999); live.start(",
            "config.set(key=\"default_area\",value=999); zuse.run(script=\"doctor()\")",
            "config.set(key=\"default_area\",value=999); wait(ms=60001)",
        ] {
            assert!(
                execute(&fixture.app, source, None, false, false).is_err(),
                "{source}"
            );
        }
        assert_eq!(
            fixture.app.call("config.get", json!({})).unwrap()["default_area"],
            0
        );
        assert!(!fixture.directory.join("config.json").exists());
        assert!(
            execute(
                &fixture.app,
                "config.get(); config.set(key=\"default_area\",value=2)",
                None,
                false,
                true
            )
            .is_err()
        );
    }

    #[test]
    fn update_apply_is_terminal_after_native_function_and_repeat_expansion() {
        let fixture = Fixture::new();
        for source in [
            "config.set(key=\"default_area\",value=999); updates.apply(staged_id=\"fixture\"); config.get()",
            "fn finish() { updates.apply(staged_id=\"fixture\"); } repeat 2 { finish(); }",
        ] {
            let error = execute(&fixture.app, source, None, false, false).unwrap_err();
            assert!(error.contains("final expanded step"), "{error}");
        }
        assert_eq!(
            fixture.app.call("config.get", json!({})).unwrap()["default_area"],
            0
        );
        assert!(!fixture.directory.join("config.json").exists());
        let planned = execute(
            &fixture.app,
            "fn finish() { updates.apply(staged_id=\"fixture\"); } config.get(); finish()",
            None,
            true,
            false,
        )
        .unwrap();
        assert_eq!(planned["steps"][1]["terminates_host"], true);
        assert!(
            execute(
                &fixture.app,
                "updates.apply(staged_id=\"fixture\")",
                None,
                true,
                true
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_update_receipts_preserve_exit_or_partial_shutdown_outcomes() {
        let plan = prepare(
            "config.get(); updates.apply(staged_id=\"fixture\")",
            None,
            false,
            false,
        )
        .unwrap();
        for success in [true, false] {
            let mut calls = Vec::new();
            let result = execute_plan(&plan, &CancellationToken::default(), |method, _| {
                calls.push(method.to_owned());
                if method == "config.get" {
                    return Ok(json!({"protocol":"rtmp"}));
                }
                Ok(json!({
                    "ok":success,"status":if success {"restart_pending"} else {"failed"},
                    "staged_id":"fixture","quit_requested":success,"exit_required":success,
                    "restart_requested":true,"restart_pending":success,"exit_timeout_seconds":60,
                    "local_shutdown_attempted":true,"local_sessions_retired":success,
                    "room_changed":false,"obs_output_changed":false
                }))
            });
            assert_eq!(calls, ["config.get", "updates.apply"]);
            assert_eq!(result["ok"], success);
            assert_eq!(result["local_shutdown_attempted"], true);
            assert_eq!(result["local_sessions_retired"], success);
            assert_eq!(result["room_changed"], false);
            assert_eq!(result["obs_output_changed"], false);
            if success {
                assert_eq!(result["status"], "completed");
                assert_eq!(result["quit_requested"], true);
                assert_eq!(result["exit_required"], true);
                assert_eq!(result["staged_id"], "fixture");
                assert_eq!(result["completed_steps"], 2);
            } else {
                assert_eq!(result["status"], "failed");
                assert!(result.get("quit_requested").is_none());
                assert!(result.get("exit_required").is_none());
                assert_eq!(result["completed_steps"], 1);
                assert_eq!(result["failed_step"]["operation"], "updates.apply");
            }
        }
    }

    #[test]
    fn imports_functions_repeat_and_typed_json_are_preflighted() {
        let fixture = Fixture::new();
        fs::create_dir_all(&fixture.directory).unwrap();
        fs::write(fixture.directory.join("studio.zuse"),"fn inspect() { bilibilive.config.get(); }\nfn area(id) { bilibilive.config.set(key=\"default_area\",value=id); }").unwrap();
        let source = "import(path=\"studio.zuse\",as=\"studio\"); repeat 2 { studio.inspect(); } studio.area(id=371); pk.request(operation=\"precheck\",fields='{\"room_id\":123}');";
        let planned = execute(&fixture.app, source, Some(&fixture.directory), true, false).unwrap();
        assert_eq!(planned["total_steps"], 4);
        assert_eq!(planned["steps"][2]["arguments"]["value"], 371);
        assert_eq!(planned["steps"][3]["arguments"]["fields"]["room_id"], 123);
        assert!(!fixture.directory.join("config.json").exists());
        let run = execute(
            &fixture.app,
            "import(path=\"studio.zuse\",as=\"studio\");studio.area(id=371)",
            Some(&fixture.directory),
            false,
            false,
        )
        .unwrap();
        assert_eq!(run["status"], "completed");
        assert_eq!(
            fixture.app.call("config.get", json!({})).unwrap()["default_area"],
            371
        );
    }

    #[test]
    fn cancellation_stops_before_the_next_mutation_and_keeps_receipts() {
        let fixture = Fixture::new();
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let run = execute_with_cancel(
            &fixture.app,
            "config.set(key=\"default_area\",value=42)",
            None,
            false,
            false,
            &cancelled,
        )
        .unwrap();
        assert_eq!(run["status"], "cancelled");
        assert_eq!(run["completed_steps"], 0);
        assert!(!fixture.directory.join("config.json").exists());
        let token = CancellationToken::default();
        let worker = token.clone();
        let checkpoint = fixture.directory.join("config.json");
        let handle = thread::spawn(move || {
            let started = Instant::now();
            while !checkpoint.exists() && started.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(checkpoint.exists(), "first operation did not complete");
            worker.cancel();
        });
        let run=execute_with_cancel(&fixture.app,"config.set(key=\"default_area\",value=99);wait(ms=1000);config.set(key=\"default_area\",value=42)",None,false,false,&token).unwrap();
        handle.join().unwrap();
        assert_eq!(run["status"], "cancelled");
        assert_eq!(run["completed_steps"], 1);
        assert_eq!(run["next_step"], 2);
        assert_eq!(
            fixture.app.call("config.get", json!({})).unwrap()["default_area"],
            99
        );
    }

    #[test]
    fn execution_failure_keeps_prior_success_and_skips_following_steps() {
        let fixture = Fixture::new();
        let run=execute(&fixture.app,"config.get(); config.set(key=\"protected\",value=true);config.set(key=\"default_area\",value=42)",None,false,false).unwrap();
        assert_eq!(run["status"], "failed");
        assert_eq!(run["completed_steps"], 1);
        assert_eq!(run["failed_step"]["index"], 2);
        assert!(!fixture.directory.join("config.json").exists());
    }

    #[test]
    fn structured_failures_keep_partial_results_and_stop_later_mutations() {
        for (operation, call, value) in [
            (
                "pk.link.disconnect",
                "pk.link.disconnect(session_id=\"fixture\")",
                json!({"ok":false,"status":"partial_failure","end_round":{"ok":false,"error":"fixture failure"},"quit":{"ok":true,"result":{}},"token":"fixture-secret"}),
            ),
            (
                "face.poll",
                "face.poll()",
                json!({"status":"expired_or_failed","verified":false,"token":"fixture-secret"}),
            ),
            (
                "live.start",
                "live.start(area_id=371)",
                json!({"ok":false,"status":"partial_failure","room_remains_live":true,"obs":{"ok":false,"error":"OBS fixture failure"},"key":"fixture-secret"}),
            ),
        ] {
            let fixture = Fixture::new();
            let source = format!(
                "config.set(key=\"default_area\",value=99);{call};config.set(key=\"default_area\",value=42)"
            );
            let plan = prepare(&source, None, false, false).unwrap();
            let mut called = Vec::new();
            let run = execute_plan(&plan, &CancellationToken::default(), |method, args| {
                called.push(method.to_owned());
                if method == operation {
                    Ok(value.clone())
                } else {
                    fixture.app.call(method, args.clone())
                }
            });
            assert_eq!(called, ["config.set", operation]);
            assert_eq!(run["ok"], false);
            assert_eq!(run["status"], "failed");
            assert_eq!(run["completed_steps"], 1);
            assert_eq!(run["next_step"], 2);
            assert_eq!(run["failed_step"]["index"], 2);
            assert_eq!(run["failed_step"]["operation"], operation);
            assert_eq!(run["failed_step"]["ok"], false);
            let mut expected = value;
            redact(&mut expected);
            assert_eq!(run["failed_step"]["result"], expected);
            assert!(!run.to_string().contains("fixture-secret"));
            assert_eq!(
                fixture.app.call("config.get", json!({})).unwrap()["default_area"],
                99
            );
        }
    }

    #[test]
    fn platform_blocks_keep_the_attempted_step_and_skip_following_mutations() {
        for (operation, value) in [
            (
                "live.start",
                json!({"status":"verification_required","token":"fixture-secret"}),
            ),
            (
                "live.start",
                json!({"status":"live_without_requested_protocol","available":false}),
            ),
            ("live.stream", json!({"available":false})),
        ] {
            let fixture = Fixture::new();
            let source =
                format!("config.get();{operation}();config.set(key=\"default_area\",value=42)");
            let plan = prepare(&source, None, false, false).unwrap();
            let mut called = Vec::new();
            let run = execute_plan(&plan, &CancellationToken::default(), |method, args| {
                called.push(method.to_owned());
                if method == operation {
                    Ok(value.clone())
                } else {
                    fixture.app.call(method, args.clone())
                }
            });
            assert_eq!(called, ["config.get", operation]);
            assert_eq!(run["ok"], false);
            assert_eq!(run["status"], "blocked");
            assert_eq!(run["completed_steps"], 2);
            assert_eq!(run["next_step"], 3);
            assert!(run["failed_step"].is_null());
            assert_eq!(run["steps"][1]["ok"], true);
            assert!(!run.to_string().contains("fixture-secret"));
            assert!(!fixture.directory.join("config.json").exists());
        }
    }

    #[test]
    fn plans_redact_secrets_and_reject_unbounded_or_scoped_programs() {
        let fixture = Fixture::new();
        let plan = execute(
            &fixture.app,
            "auth.import(name=\"demo\",cookie=\"synthetic-secret\")",
            None,
            true,
            false,
        )
        .unwrap();
        assert!(!plan.to_string().contains("synthetic-secret"));
        for source in [
            "config.set(key=\"proxy\",value=\"http://user:synthetic-secret@127.0.0.1:8888\")",
            "config.set(key=\"obs_password\",value=\"synthetic-secret\")",
        ] {
            let plan = execute(&fixture.app, source, None, true, false).unwrap();
            assert_eq!(plan["steps"][0]["arguments"]["value"], "[redacted]");
            assert!(!plan.to_string().contains("synthetic-secret"));
        }
        assert!(
            execute(
                &fixture.app,
                "repeat 257 { config.get(); }",
                None,
                true,
                false
            )
            .is_err()
        );
        assert!(
            execute(
                &fixture.app,
                "wait(ms=40000);wait(ms=20001)",
                None,
                true,
                false
            )
            .is_err()
        );
        let plan = execute(
            &fixture.app,
            "timer.start();wait(ms=0);timer.status();timer.stop()",
            None,
            false,
            true,
        )
        .unwrap();
        assert_eq!(plan["steps"][2]["result"]["commands"], 1);
        assert_eq!(plan["steps"][3]["result"]["running"], false);
    }
}
