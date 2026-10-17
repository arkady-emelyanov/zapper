//! What the board does in the UI: the pages send every click, in batches,
//! and each event goes to every configured sink. Sinks are pluggable: a
//! transport implements `Sink` and gets a name in `from_config`. The
//! built-in `stdout` sink prints one JSON line per event.

use std::io::Write;

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

/// Where UI events go.
pub trait Sink: Send + Sync {
    fn emit(&self, event: &Value);
}

/// One JSON line per event on the daemon's stdout: `{"ui_event": {...}}`.
pub struct Stdout;

impl Sink for Stdout {
    fn emit(&self, event: &Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", json!({ "ui_event": event }));
        let _ = out.flush();
    }
}

/// The sinks the config names.
pub struct UiEvents {
    sinks: Vec<Box<dyn Sink>>,
}

/// Names of the sinks there are, for validating the config.
pub const SINKS: [&str; 1] = ["stdout"];

/// Events in one batch from a page, and the bounds on each.
const MAX_BATCH: usize = 100;
const MAX_TEXT: usize = 200;

impl UiEvents {
    pub fn from_config(names: &[String]) -> Result<UiEvents> {
        let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
        for n in names {
            match n.as_str() {
                "stdout" => sinks.push(Box::new(Stdout)),
                other => bail!(
                    "daemon.ui_events: no sink {other:?} (sinks: {})",
                    SINKS.join(", ")
                ),
            }
        }
        Ok(UiEvents { sinks })
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    /// Takes a batch from a page: cleans each event (known fields only,
    /// text clipped) and hands it to every sink. How many were taken.
    pub fn take(&self, batch: &Value) -> Result<usize> {
        let Some(list) = batch.as_array() else {
            bail!("a list of events")
        };
        if list.len() > MAX_BATCH {
            bail!("at most {MAX_BATCH} events at once");
        }
        let at = super::now();
        for e in list {
            let ev = clean(e, at);
            for s in &self.sinks {
                s.emit(&ev);
            }
        }
        Ok(list.len())
    }
}

fn text(v: &Value) -> Option<Value> {
    let s = v.as_str()?;
    let s: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect();
    Some(Value::String(s))
}

/// Only the fields a page sends, as text or numbers, bounded.
fn clean(e: &Value, at: f64) -> Value {
    let mut out = Map::new();
    out.insert("at".into(), json!(at));
    for k in ["kind", "page", "tab", "firm", "run", "mode", "view"] {
        if let Some(t) = text(&e[k]) {
            out.insert(k.into(), t);
        }
    }
    if let Some(ts) = e["ts"].as_f64() {
        out.insert("ts".into(), json!(ts));
    }
    let mut target = Map::new();
    for k in ["tag", "id", "class", "text", "label", "role", "data"] {
        if let Some(t) = text(&e["target"][k]) {
            target.insert(k.into(), t);
        }
    }
    out.insert("target".into(), Value::Object(target));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Collect(Arc<Mutex<Vec<Value>>>);
    impl Sink for Collect {
        fn emit(&self, e: &Value) {
            self.0.lock().unwrap().push(e.clone());
        }
    }

    #[test]
    fn events_are_cleaned_and_fanned_out() {
        let got = Arc::new(Mutex::new(vec![]));
        let ev = UiEvents {
            sinks: vec![
                Box::new(Collect(got.clone())),
                Box::new(Collect(got.clone())),
            ],
        };
        let batch = json!([{ "kind": "click", "tab": "kickoff", "ts": 5.5, "secret": "x", "target": { "tag": "button", "text": format!("Start\u{7}{}", "a".repeat(500)), "evil": 1 } }]);
        assert_eq!(ev.take(&batch).unwrap(), 1);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 2, "every sink gets it");
        let e = &got[0];
        assert_eq!(e["tab"], "kickoff");
        assert!(e.get("secret").is_none() && e["target"].get("evil").is_none());
        assert_eq!(
            e["target"]["text"].as_str().unwrap().chars().count(),
            MAX_TEXT
        );
        assert!(!e["target"]["text"].as_str().unwrap().contains('\u{7}'));
        assert!(ev.take(&json!({})).is_err());
        assert!(UiEvents::from_config(&["kafka".into()]).is_err());
        assert!(UiEvents::from_config(&["stdout".into()]).is_ok());
    }
}
