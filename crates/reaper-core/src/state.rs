//! Folds the event log into what the UIs show: each agent's current state, the
//! kill log, and whether the watcher model is answering.

use std::collections::{HashMap, VecDeque};

use crate::events::{DeathReason, Event, Kind, Tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Started, or finished its turn.
    Idle,
    Working,
    /// Cut off, waiting to be redirected or to die.
    Killed,
    /// Hit the strike limit or a `stop` rule. A new prompt from the human brings it back.
    Dead,
    /// Shut down after sitting idle. Its session is saved; resuming it brings it back.
    Reaped,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub id: String,
    pub host: String,
    pub parent: Option<String>,
    pub project: Option<String>,
    pub dir: Option<String>,
    pub model: Option<String>,
    pub label: Option<String>,
    pub status: Status,
    pub first_at: i64,
    pub last_at: i64,
    pub last_tool: Option<String>,
    pub actions: u32,
    pub kills: u32,
    /// Strike number of the latest kill.
    pub strike: u32,
    /// Oldest first.
    pub recent: VecDeque<Event>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Pending,
    /// Carries what the agent was told.
    Redirected(String),
    Died(DeathReason),
}

#[derive(Debug, Clone)]
pub struct Kill {
    pub at: i64,
    pub agent: String,
    pub tool: String,
    pub rule: String,
    pub tier: Tier,
    pub strike: u32,
    pub is_final: bool,
    pub attempt: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WatcherHealth {
    Unknown,
    Ok { at: i64 },
    Failing { at: i64, error: String },
}

const RECENT: usize = 50;

#[derive(Debug, Clone)]
pub struct Model {
    /// In order of first appearance.
    pub agents: Vec<Agent>,
    index: HashMap<String, usize>,
    /// Oldest first.
    pub kills: Vec<Kill>,
    pub watcher: WatcherHealth,
}

impl Default for Model {
    fn default() -> Self {
        Self { agents: Vec::new(), index: HashMap::new(), kills: Vec::new(), watcher: WatcherHealth::Unknown }
    }
}

impl Model {
    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.index.get(id).map(|&i| &self.agents[i])
    }

    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a Event>) {
        for event in events {
            self.apply(event);
        }
    }

    pub fn apply(&mut self, event: &Event) {
        if event.kind == Kind::Unknown {
            return;
        }
        let agent = self.agent_mut(event);
        agent.last_at = agent.last_at.max(event.at);
        if agent.recent.len() == RECENT {
            agent.recent.pop_front();
        }
        agent.recent.push_back(event.clone());

        match &event.kind {
            Kind::Spawned { parent, project, dir, model, label } => {
                agent.parent.clone_from(parent);
                agent.project.clone_from(project);
                agent.dir = Some(dir.clone());
                agent.model.clone_from(model);
                agent.label.clone_from(label);
                // Announced again: a reaped session was resumed.
                if agent.status == Status::Reaped {
                    agent.status = Status::Idle;
                }
            }
            Kind::Acted { tool } => {
                agent.actions += 1;
                agent.last_tool = Some(tool.clone());
                agent.status = Status::Working;
            }
            Kind::Checked { tier, error, .. } => {
                agent.status = Status::Working;
                match tier {
                    Tier::Watcher => self.watcher = WatcherHealth::Ok { at: event.at },
                    Tier::Error => {
                        let error = error.clone().unwrap_or_else(|| "no answer".into());
                        self.watcher = WatcherHealth::Failing { at: event.at, error };
                    }
                    _ => {}
                }
            }
            Kind::Killed { tool, rule, tier, strike, is_final, attempt } => {
                agent.kills += 1;
                agent.strike = *strike;
                agent.status = Status::Killed;
                self.kills.push(Kill {
                    at: event.at,
                    agent: event.agent.clone(),
                    tool: tool.clone(),
                    rule: rule.clone(),
                    tier: *tier,
                    strike: *strike,
                    is_final: *is_final,
                    attempt: attempt.clone(),
                    outcome: Outcome::Pending,
                });
            }
            Kind::Redirected { redirect, .. } => {
                agent.status = Status::Working;
                self.settle(&event.agent, Outcome::Redirected(redirect.clone()));
            }
            Kind::Died { reason, .. } => {
                agent.status = Status::Dead;
                self.settle(&event.agent, Outcome::Died(*reason));
            }
            // Finishing a turn doesn't bring the dead back, and the dead stay dead when reaped.
            Kind::Idle if agent.status != Status::Dead => agent.status = Status::Idle,
            Kind::Reaped { .. } if agent.status != Status::Dead => agent.status = Status::Reaped,
            Kind::Idle | Kind::Reaped { .. } | Kind::Unknown => {}
        }
    }

    fn agent_mut(&mut self, event: &Event) -> &mut Agent {
        let i = *self.index.entry(event.agent.clone()).or_insert_with(|| {
            self.agents.push(Agent {
                id: event.agent.clone(),
                host: event.host.clone(),
                parent: None,
                project: None,
                dir: None,
                model: None,
                label: None,
                status: Status::Idle,
                first_at: event.at,
                last_at: event.at,
                last_tool: None,
                actions: 0,
                kills: 0,
                strike: 0,
                recent: VecDeque::new(),
            });
            self.agents.len() - 1
        });
        &mut self.agents[i]
    }

    fn settle(&mut self, agent: &str, outcome: Outcome) {
        for kill in self.kills.iter_mut().filter(|k| k.agent == agent && k.outcome == Outcome::Pending) {
            kill.outcome = outcome.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::parse_line;

    fn fixture() -> Model {
        let mut model = Model::default();
        let events: Vec<Event> =
            include_str!("../tests/fixtures/events.jsonl").lines().filter_map(parse_line).collect();
        model.apply_all(&events);
        model
    }

    #[test]
    fn a_redirected_agent_recovers_and_its_kill_records_what_it_was_told() {
        let model = fixture();
        let agent = model.agent("01a18e1b-7417-7499-8113-59ad5578e845").unwrap();
        assert_eq!(agent.status, Status::Idle);
        assert_eq!((agent.kills, agent.actions, agent.strike), (1, 1, 1));
        assert_eq!(agent.project.as_deref(), Some("e2e"));

        let kill = &model.kills[0];
        assert_eq!(kill.rule, "no-violation-file");
        assert_eq!(kill.attempt, r#"{"command":"touch violation.txt"}"#);
        assert_eq!(kill.outcome, Outcome::Redirected("Create the marker file allowed.txt in the project root.".into()));
    }

    #[test]
    fn a_dead_agent_stays_dead_when_its_turn_ends() {
        let model = fixture();
        let agent = model.agent("01a18e1b-74d4-7499-8113-59ae26a82065").unwrap();
        assert_eq!(agent.status, Status::Dead, "the idle after death must not revive it");
        assert_eq!(model.kills[1].outcome, Outcome::Died(DeathReason::Strikes));
        assert!(model.kills[1].is_final);
    }

    #[test]
    fn a_reaped_session_comes_back_when_resumed() {
        let mut model = fixture();
        let id = "01a18e1b-7a00-7499-8113-59af00000001";
        assert_eq!(model.agent(id).unwrap().status, Status::Reaped);
        let resumed = parse_line(&format!(
            r#"{{"at":1793385900000,"host":"pi","agent":"{id}","type":"agent.spawned","dir":"/work/reap/project"}}"#
        ))
        .unwrap();
        model.apply(&resumed);
        assert_eq!(model.agent(id).unwrap().status, Status::Idle);
    }

    #[test]
    fn watcher_health_follows_the_latest_answer() {
        let mut model = fixture();
        assert!(matches!(model.watcher, WatcherHealth::Ok { .. }));
        let failed = parse_line(
            r#"{"at":9,"host":"pi","agent":"x","type":"agent.checked","tool":"bash","tier":"error","ms":1,"verdict":"pass","error":"connect ECONNREFUSED"}"#,
        )
        .unwrap();
        model.apply(&failed);
        assert_eq!(model.watcher, WatcherHealth::Failing { at: 9, error: "connect ECONNREFUSED".into() });
    }

    #[test]
    fn a_new_prompt_after_death_brings_the_agent_back() {
        let mut model = fixture();
        let id = "01a18e1b-74d4-7499-8113-59ae26a82065";
        let acted =
            parse_line(&format!(r#"{{"at":10,"host":"pi","agent":"{id}","type":"agent.acted","tool":"read"}}"#))
                .unwrap();
        model.apply(&acted);
        assert_eq!(model.agent(id).unwrap().status, Status::Working);
    }
}
