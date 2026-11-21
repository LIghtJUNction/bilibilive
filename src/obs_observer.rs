//! A persistent OBS connection; only a short account lease may apply its effects.
use super::*;
use std::sync::{
    Weak,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Observer {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Value>>,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl App {
    pub(super) fn observer_start(&self) -> Result<Value, String> {
        let mut slot = self
            .inner
            .observer
            .lock()
            .map_err(|_| "OBS observer lock failed")?;
        if let Some(observer) = slot.as_ref() {
            return Ok(observer
                .status
                .lock()
                .map_err(|_| "OBS observer status lock failed")?
                .clone());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(json!({"running":true,"connected":false})));
        let weak = Arc::downgrade(&self.inner);
        let worker_stop = stop.clone();
        let worker_status = status.clone();
        std::thread::Builder::new()
            .name("bilibilive-obs-observer".into())
            .spawn(move || run(weak, worker_stop, worker_status))
            .map_err(|e| e.to_string())?;
        *slot = Some(Observer { stop, status });
        Ok(json!({"running":true,"connected":false}))
    }
    pub(super) fn observer_stop(&self) -> Result<Value, String> {
        self.inner
            .observer
            .lock()
            .map_err(|_| "OBS observer lock failed")?
            .take();
        Ok(json!({"running":false,"connected":false}))
    }
    pub(super) fn observer_status(&self) -> Result<Value, String> {
        let slot = self
            .inner
            .observer
            .lock()
            .map_err(|_| "OBS observer lock failed")?;
        match slot.as_ref() {
            Some(observer) => Ok(observer
                .status
                .lock()
                .map_err(|_| "OBS observer status lock failed")?
                .clone()),
            None => Ok(json!({"running":false,"connected":false})),
        }
    }
    fn observer_event(
        &self,
        event: &Value,
        generation: u64,
        endpoint: &str,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        self.with_account_lease(||{
            if stop.load(Ordering::Acquire){return Ok(());}
            let cfg=self.config()?;
            let output=event.pointer("/eventData/outputState").and_then(Value::as_str).unwrap_or("");
            {
                let mut state=self.inner.state.lock().map_err(|_|"State lock failed")?;
                if state.account_generation!=generation||cfg.obs_url!=endpoint{return Ok(());}
                if output=="OBS_WEBSOCKET_OUTPUT_STARTING"&&Instant::now()<state.obs_ignore_start_until{state.obs_ignore_start_until=Instant::now();return Ok(());}
                if output=="OBS_WEBSOCKET_OUTPUT_STOPPED"&&Instant::now()<state.obs_ignore_stop_until{state.obs_ignore_stop_until=Instant::now();return Ok(());}
            }
            if event["eventType"]!="StreamStateChanged"{return Ok(());}
            if output=="OBS_WEBSOCKET_OUTPUT_STARTING"&&cfg.obs_link {
                let mut args=json!({"protocol":cfg.protocol,"strategy":cfg.strategy});
                if cfg.default_area>0{args["area_id"]=json!(cfg.default_area);}
                let value=self.client()?.call("live.start",&args)?;
                if value["status"]!="live"{return Err("Bilibili requires verification before OBS-linked startup; open bilibilive's live controls".into());}
            }else if output=="OBS_WEBSOCKET_OUTPUT_STOPPED"&&cfg.obs_auto_stop {
                self.reset_interaction()?;
                self.client()?.call("live.stop",&json!({}))?;
            }
            Ok(())
        })
    }
}

fn pause(stop: &AtomicBool, millis: u64) {
    for _ in 0..millis.div_ceil(100) {
        if stop.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run(weak: Weak<Inner>, stop: Arc<AtomicBool>, status: Arc<Mutex<Value>>) {
    let mut connection: Option<(crate::obs::ObsClient, u64, String)> = None;
    let mut failures = 0u32;
    while !stop.load(Ordering::Acquire) {
        let Some(inner) = weak.upgrade() else {
            break;
        };
        let app = App {
            inner,
            read_only: false,
        };
        let ready = app.with_account_lease(|| {
            let cfg = app.config()?;
            let generation = app
                .inner
                .state
                .lock()
                .map_err(|_| "State lock failed")?
                .account_generation;
            if connection
                .as_ref()
                .is_some_and(|(_, epoch, url)| *epoch != generation || url != &cfg.obs_url)
            {
                connection = None;
            }
            if connection.is_none() {
                connection = Some((app.obs()?, generation, cfg.obs_url));
            }
            Ok(())
        });
        drop(app);
        if let Err(error) = ready {
            failures = failures.saturating_add(1);
            if let Ok(mut value) = status.lock() {
                *value = json!({"running":true,"connected":false,"reconnect_attempts":failures,"error":error});
            }
            pause(&stop, 1000u64.saturating_mul(u64::from(failures.min(10))));
            continue;
        }
        let Some((client, generation, endpoint)) = connection.as_mut() else {
            continue;
        };
        // No strong App reference or profile lock is held during socket waits.
        match client.status() {
            Ok(output) => {
                failures = 0;
                if let Ok(mut value) = status.lock() {
                    *value = json!({"running":true,"connected":true,"output":output});
                }
                for event in client.pending_events() {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    let app = App {
                        inner,
                        read_only: false,
                    };
                    if let Err(error) = app.observer_event(&event, *generation, endpoint, &stop) {
                        if let Ok(mut value) = status.lock() {
                            value["error"] = json!(error);
                        }
                        app.push_event(json!({"cmd":"BILIBILIVE_OBS_ERROR","message":error}));
                    }
                }
                pause(&stop, 1000);
            }
            Err(error) => {
                connection = None;
                failures = failures.saturating_add(1);
                if let Ok(mut value) = status.lock() {
                    *value = json!({"running":true,"connected":false,"reconnect_attempts":failures,"error":error});
                }
                pause(&stop, 1000u64.saturating_mul(u64::from(failures.min(10))));
            }
        }
    }
    if let Ok(mut value) = status.lock() {
        value["running"] = json!(false);
        value["connected"] = json!(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread};
    use tungstenite::Message;

    #[test]
    fn observes_real_local_output_and_closes_without_a_bilibili_account() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, ready) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .send(Message::Text(
                    json!({"op":0,"d":{"rpcVersion":1}}).to_string().into(),
                ))
                .unwrap();
            socket.read().unwrap();
            socket
                .send(Message::Text(
                    json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["d"]["requestType"], "GetStreamStatus");
            socket.send(Message::Text(json!({"op":7,"d":{"requestId":request["d"]["requestId"],"requestStatus":{"result":true,"code":100},"responseData":{"outputActive":false,"outputReconnecting":false}}}).to_string().into())).unwrap();
            sent.send(()).unwrap();
            assert!(socket.read().is_err());
        });
        let directory = std::env::temp_dir().join(format!(
            "bilibilive-observer-test-{}-{}",
            std::process::id(),
            address.port()
        ));
        let app = App::new(Some(directory.clone())).unwrap();
        app.call(
            "config.set",
            json!({"key":"obs_url","value":format!("ws://{address}")}),
        )
        .unwrap();
        app.call("obs.watch.start", json!({})).unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = app.call("obs.watch.status", json!({})).unwrap();
            if state["connected"] == true {
                assert_eq!(state["output"]["outputActive"], false);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            app.call("obs.watch.stop", json!({})).unwrap()["running"],
            false
        );
        server.join().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
