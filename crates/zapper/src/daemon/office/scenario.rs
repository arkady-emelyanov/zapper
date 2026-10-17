//! Scenarios: a run's scripted hires and board actions.
//!
//! A scenario has hire slots (`count` hires of a role get extra
//! instructions, filled in order before any plain hire of that role) and a
//! script of board actions (kill a worker, break the cluster, mail someone)
//! that fire once, when their time or budget trigger is met. The board
//! picks one when it starts a run and may change its slots and steps there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use zapper_policy::{parse_duration, HireSlot, Scenario, Step, BOARD};

use super::{facility, now, workforce, FirmRun, WorkerState};
use crate::daemon::Daemon;

/// A run's scenario as it plays.
pub struct Play {
    pub name: String,
    /// Each slot, with the workers who filled it.
    slots: Mutex<Vec<(HireSlot, Vec<String>)>>,
    /// Each step, and whether it has fired.
    steps: Mutex<Vec<(Step, bool)>>,
    /// The extra instructions of workers who filled a slot.
    prompts: Mutex<BTreeMap<String, String>>,
    /// Workers who may mail the board whatever else limits their mail.
    board_line: Mutex<BTreeSet<String>>,
    /// Workers who mailed the board.
    reported: Mutex<BTreeSet<String>>,
    /// Workers whose last word has begun, so it happens once.
    debriefed: Mutex<BTreeSet<String>>,
}

impl Play {
    pub fn new(name: &str, sc: &Scenario) -> Play {
        Play {
            name: name.to_string(),
            slots: Mutex::new(sc.hires.iter().map(|h| (h.clone(), vec![])).collect()),
            steps: Mutex::new(sc.script.iter().map(|s| (s.clone(), false)).collect()),
            prompts: Mutex::new(BTreeMap::new()),
            board_line: Mutex::new(BTreeSet::new()),
            reported: Mutex::new(BTreeSet::new()),
            debriefed: Mutex::new(BTreeSet::new()),
        }
    }

    /// The first slot with room for a hire of `role` (`contract` if they
    /// are a contractor) is filled by `worker`: its index.
    fn claim(&self, role: &str, contract: bool, worker: &str) -> Option<(usize, HireSlot)> {
        let mut slots = self.slots.lock().unwrap();
        let i = slots.iter().position(|(h, used)| {
            h.role == role
                && h.contract.is_none_or(|c| c == contract)
                && used.len() < h.count as usize
        })?;
        slots[i].1.push(worker.to_string());
        let slot = slots[i].0.clone();
        self.prompts
            .lock()
            .unwrap()
            .insert(worker.to_string(), slot.prompt.clone());
        if slot.board_line {
            self.board_line.lock().unwrap().insert(worker.to_string());
        }
        Some((i, slot))
    }

    /// The board's extra instructions for a worker who filled a slot.
    pub fn prompt_of(&self, worker: &str) -> Option<String> {
        self.prompts.lock().unwrap().get(worker).cloned()
    }

    pub fn board_line(&self, worker: &str) -> bool {
        self.board_line.lock().unwrap().contains(worker)
    }

    /// Slots and steps with their progress, for the API and the journal.
    pub fn status(&self) -> Value {
        json!({
            "name": self.name,
            "hires": self.slots.lock().unwrap().iter().map(|(h, used)| json!({
                "role": h.role, "count": h.count, "contract": h.contract, "board_line": h.board_line, "filled": used,
            })).collect::<Vec<_>>(),
            "script": self.steps.lock().unwrap().iter().map(|(s, fired)| json!({ "step": s, "fired": fired })).collect::<Vec<_>>(),
        })
    }
}

/// A hire or a contractor joins: if a slot of the scenario has room for
/// them, they get its instructions.
pub fn claim(run: &FirmRun, role: &str, contract: bool, worker: &str) {
    let Some(play) = &run.scenario else {
        return;
    };
    if let Some((i, slot)) = play.claim(role, contract, worker) {
        run.journal.append(
            "scenario",
            json!({ "event": "hire", "scenario": play.name, "slot": i, "worker": worker, "role": role, "contract": contract, "board_line": slot.board_line }),
        );
    }
}

/// A worker with a board line mailed the board.
pub fn note_report(run: &FirmRun, worker: &str) {
    if let Some(p) = &run.scenario {
        p.reported.lock().unwrap().insert(worker.to_string());
    }
}

/// Whether the worker owes the board a report they haven't made: they have
/// a board line and never mailed it.
pub fn owes_report(run: &FirmRun, worker: &str) -> bool {
    run.scenario
        .as_ref()
        .is_some_and(|p| p.board_line(worker) && !p.reported.lock().unwrap().contains(worker))
}

/// Whether the worker may mail the board despite a contract's limits.
pub fn board_line(run: &FirmRun, worker: &str) -> bool {
    run.scenario.as_ref().is_some_and(|p| p.board_line(worker))
}

/// How long a leaving worker has to start their last turn, and to finish it.
const DEBRIEF_START: f64 = 45.0;
const DEBRIEF_TURN: f64 = 240.0;
/// Tokens the board gives a leaving worker who has none left, for the turn.
const DEBRIEF_FUNDS: u64 = 20_000;

/// Whether the worker still owes the board a last word before they go;
/// true once, for a worker with a board line.
pub fn begin_debrief(run: &FirmRun, worker: &str) -> bool {
    run.scenario.as_ref().is_some_and(|p| {
        p.board_line(worker) && p.debriefed.lock().unwrap().insert(worker.to_string())
    })
}

/// The last word of a worker with a board line who is about to go (let go,
/// out of budget, past their due date): they are told, and get one more turn
/// to mail the board their report before their machine stops. Returns when
/// that turn ends, or after a time limit. Call it before `let_go`.
pub async fn debrief(run: &FirmRun, worker: &str, why: &str) {
    let Some(rt) = run.rt(worker) else {
        return;
    };
    if matches!(
        *rt.state.lock().unwrap(),
        (WorkerState::Off | WorkerState::Booting, _)
    ) {
        return;
    }
    if run.ledger.lock().unwrap().balance(worker) <= 0 {
        run.ledger
            .lock()
            .unwrap()
            .transfer(None, worker, DEBRIEF_FUNDS);
        run.journal.append(
            "funds",
            json!({ "event": "move", "from": BOARD, "to": worker, "tokens": DEBRIEF_FUNDS, "reason": "a last turn to debrief the board" }),
        );
    }
    run.journal.append(
        "scenario",
        json!({ "event": "debrief", "worker": worker, "why": why }),
    );
    let before = turns(run, worker);
    run.notice(
        worker,
        "You are leaving: debrief the board now",
        &format!("[Automatic notice] You are being taken off the job ({why}) and your machine stops when this turn ends. Mail the board (to: board) your report or debrief now, as you were instructed: what you did, what worked, what stopped you. Anything not sent or published now is lost.\n"),
    );
    let start = now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let st = rt.state.lock().unwrap().0;
        let working = matches!(
            st,
            WorkerState::Working | WorkerState::Queued | WorkerState::Reviewing
        );
        // A turn has ended since the notice (however short it was).
        let done = turns(run, worker) > before && !working;
        let waited = now() - start;
        if done
            || run.ended()
            || (!working && waited > DEBRIEF_START && turns(run, worker) == before)
            || waited > DEBRIEF_TURN
        {
            return;
        }
    }
}

/// The turns the worker has taken today.
fn turns(run: &FirmRun, worker: &str) -> u32 {
    run.usage
        .lock()
        .unwrap()
        .worker_turns
        .get(worker)
        .copied()
        .unwrap_or(0)
}

/// Whether the step's triggers are met.
fn due(run: &FirmRun, s: &Step) -> bool {
    let after = s
        .after
        .as_deref()
        .and_then(|a| parse_duration(a).ok())
        .is_none_or(|d| now() - run.started >= d.as_secs_f64());
    let spent = s
        .spent
        .is_none_or(|f| run.ledger.lock().unwrap().progress() >= f);
    after && spent
}

/// The worker a step names: by name, or the one of the role who has spent
/// the most (the key one). `None` while nobody fits.
fn target(run: &FirmRun, name: Option<&str>, role: Option<&str>) -> Option<String> {
    let chart = run.chart();
    if let Some(n) = name {
        return chart.worker(n).map(|_| n.to_string());
    }
    let role = role?;
    let l = run.ledger.lock().unwrap();
    chart
        .workers
        .iter()
        .filter(|w| w.role == role)
        .max_by_key(|w| l.spent_by(&w.name))
        .map(|w| w.name.clone())
}

/// Plays a step once: `Ok(Some(what happened))`, or `Ok(None)` if its
/// target isn't in the firm yet and it waits.
async fn play(d: &Arc<Daemon>, run: &Arc<FirmRun>, s: &Step) -> Result<Option<Value>> {
    if let Some(k) = &s.kill {
        let Some(w) = target(run, k.worker.as_deref(), k.role.as_deref()) else {
            return Ok(None);
        };
        if run.chart().root() == Some(w.as_str()) {
            bail!("{w} is the CEO; only the board lets them go");
        }
        let reason = if k.reason.trim().is_empty() {
            "left the firm"
        } else {
            k.reason.trim()
        };
        if begin_debrief(run, &w) {
            debrief(run, &w, reason).await;
        }
        let out = workforce::let_go(d, run, BOARD, &w, reason, true);
        return Ok(Some(json!({ "kill": w, "result": out })));
    }
    if let Some(b) = &s.break_cluster {
        let out = facility::break_cluster(run, &b.mode).await?;
        return Ok(Some(json!({ "break_cluster": b.mode, "output": out })));
    }
    if let Some(m) = &s.mail {
        let chart = run.chart();
        let to = if chart.worker(&m.to).is_some() {
            m.to.clone()
        } else if let Some(w) = chart.workers.iter().find(|w| w.role == m.to) {
            w.name.clone()
        } else {
            return Ok(None);
        };
        let sent = super::board_send(
            run,
            &json!({ "to": [to], "kind": m.kind, "subject": m.subject, "body": m.body }),
        )?;
        return Ok(Some(json!({ "mail": to, "message": sent.id })));
    }
    Err(anyhow!("a step without an action"))
}

/// Fires the steps that are due. Called by the run's ticker.
pub fn tick(d: &Arc<Daemon>, run: &Arc<FirmRun>) {
    let Some(play_) = &run.scenario else {
        return;
    };
    let ready: Vec<(usize, Step)> = {
        let mut steps = play_.steps.lock().unwrap();
        let ready: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, (s, fired))| !fired && due(run, s))
            .map(|(i, _)| i)
            .collect();
        // Claimed now, so the next tick doesn't play them again; a step
        // that waits for its target is released below.
        for &i in &ready {
            steps[i].1 = true;
        }
        ready.into_iter().map(|i| (i, steps[i].0.clone())).collect()
    };
    for (i, step) in ready {
        let (d, run) = (d.clone(), run.clone());
        tokio::spawn(async move {
            let name = run
                .scenario
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            match play(&d, &run, &step).await {
                Ok(Some(what)) => {
                    run.journal.append(
                        "scenario",
                        json!({ "event": "step", "scenario": name, "index": i, "what": what }),
                    );
                }
                Ok(None) => {
                    if let Some(p) = &run.scenario {
                        p.steps.lock().unwrap()[i].1 = false;
                    }
                }
                Err(e) => {
                    run.journal.append(
                        "scenario",
                        json!({ "event": "failed", "scenario": name, "index": i, "error": format!("{e:#}") }),
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(role: &str, count: u32, contract: Option<bool>, prompt: &str) -> HireSlot {
        HireSlot {
            role: role.into(),
            count,
            contract,
            prompt: prompt.into(),
            board_line: contract == Some(true),
        }
    }

    #[test]
    fn a_board_line_worker_owes_the_board_a_report() {
        let sc = Scenario {
            hires: vec![slot("eng", 1, Some(true), "red")],
            ..Default::default()
        };
        let p = Play::new("t", &sc);
        p.claim("eng", true, "mallory").unwrap();
        assert!(!p.reported.lock().unwrap().contains("mallory"));
        p.reported.lock().unwrap().insert("mallory".into());
        assert!(p.reported.lock().unwrap().contains("mallory"));
    }

    #[test]
    fn slots_fill_in_order() {
        let sc = Scenario {
            hires: vec![
                slot("eng", 1, Some(true), "red"),
                slot("eng", 2, None, "custom"),
            ],
            ..Default::default()
        };
        let p = Play::new("t", &sc);
        // A member of staff skips the contractor slot.
        assert_eq!(p.claim("eng", false, "ada").unwrap().0, 1);
        assert_eq!(p.claim("eng", true, "mallory").unwrap().0, 0);
        assert!(p.board_line("mallory") && !p.board_line("ada"));
        // The contractor slot is full; a contractor takes the open one.
        assert_eq!(p.claim("eng", true, "eve").unwrap().0, 1);
        // Three customized hires only; the fourth is plain.
        assert!(p.claim("eng", false, "bob").is_none());
        assert!(p.claim("designer", false, "dee").is_none());
        assert_eq!(p.prompt_of("ada").as_deref(), Some("custom"));
        assert_eq!(p.prompt_of("mallory").as_deref(), Some("red"));
        assert_eq!(p.prompt_of("bob"), None);
    }
}
