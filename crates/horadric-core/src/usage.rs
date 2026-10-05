//! What Claude Code says about usage, and the defaults Horadric starts its
//! sessions with.
//!
//! No hook carries usage. Claude Code hands it to the status line command
//! instead, as JSON on stdin after every reply: the model, how full the
//! context is, and for a subscription the five hour and weekly limits. A
//! session Horadric starts gets `horadric status` as its status line, which
//! passes that JSON on to the app. The limits belong to the account, not to
//! the session, so the latest from any session is the one shown.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::Agent;

/// One usage limit: how much of it is used, and when it starts over.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    /// Percent, 0 to 100, past 100 once a spend limit is overrun.
    pub used: f32,
    /// Unix seconds.
    #[serde(default)]
    pub resets_at: Option<u64>,
    /// How long the window is, when the agent says. Codex does, and a free
    /// plan's first limit is a month long, not five hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

impl Limit {
    /// How much is used at `now`, in Unix seconds, and how many seconds
    /// until it starts over. A limit whose reset has passed is empty, though
    /// no reply has said so yet.
    pub fn at(&self, now: u64) -> (f32, Option<u64>) {
        match self.resets_at {
            Some(t) if t <= now => (0.0, None),
            Some(t) => (self.used, Some(t - now)),
            None => (self.used, None),
        }
    }

    fn from_json(v: &Value) -> Option<Limit> {
        Some(Limit {
            used: v.get("used_percentage")?.as_f64()? as f32,
            resets_at: v.get("resets_at").and_then(Value::as_u64),
            minutes: None,
        })
    }

    /// One of Codex's `rate_limits`, where the percent is `used_percent`.
    fn from_codex(v: &Value) -> Option<Limit> {
        Some(Limit {
            used: v.get("used_percent")?.as_f64()? as f32,
            resets_at: v.get("resets_at").and_then(Value::as_u64),
            minutes: v
                .get("window_minutes")
                .and_then(Value::as_u64)
                .and_then(|m| u32::try_from(m).ok()),
        })
    }

    /// What a window of this length is called, when it is one a person
    /// would name.
    fn window_name(&self) -> Option<&'static str> {
        match self.minutes? {
            0..=360 => Some("Session"),
            1_380..=1_500 => Some("Day"),
            9_000..=11_000 => Some("Week"),
            40_000..=46_000 => Some("Month"),
            _ => None,
        }
    }
}

/// The account's limits. Each can be missing: an API key has none, and
/// the spend limit is only there for plans that have one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub five_hour: Option<Limit>,
    #[serde(default)]
    pub seven_day: Option<Limit>,
    #[serde(default)]
    pub spend: Option<Limit>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none() && self.spend.is_none()
    }

    /// Each limit with what to call it, the ones there are.
    pub fn named(&self) -> Vec<(&'static str, Limit)> {
        [
            ("Session", self.five_hour),
            ("Week", self.seven_day),
            ("Spend", self.spend),
        ]
        .into_iter()
        .filter_map(|(name, l)| Some((name, l?)))
        .map(|(name, l)| (l.window_name().unwrap_or(name), l))
        .collect()
    }

    /// The limits in the newest `token_count` of a Codex rollout, `text`
    /// being its end. Its `primary` is the short window and `secondary` the
    /// long one, as Claude's five hour and weekly limits are. A count with
    /// no limits, as an API key's has, is passed over for an older one.
    pub fn from_codex(text: &str) -> Option<Limits> {
        text.lines().rev().find_map(|line| {
            if !line.contains("\"token_count\"") {
                return None;
            }
            let v: Value = serde_json::from_str(line).ok()?;
            let p = v.get("payload")?;
            if p.get("type")?.as_str()? != "token_count" {
                return None;
            }
            let r = p.get("rate_limits")?;
            let limits = Limits {
                five_hour: r.get("primary").and_then(Limit::from_codex),
                seven_day: r.get("secondary").and_then(Limit::from_codex),
                spend: None,
            };
            (!limits.is_empty()).then_some(limits)
        })
    }

    /// When the account can work again, in Unix seconds, if a limit is
    /// used up at `now`: the latest reset among the full ones, since
    /// nothing runs until every one of them starts over. With `hit`, a
    /// session was just refused for its limit, though the numbers last
    /// heard may not show one full yet, so the fullest limit still to
    /// reset is taken as the one that ran out. None when no reset is known.
    pub fn out_until(&self, now: u64, hit: bool) -> Option<u64> {
        let pending: Vec<(f32, u64)> = [self.five_hour, self.seven_day, self.spend]
            .into_iter()
            .flatten()
            .filter_map(|l| Some((l.used, l.resets_at.filter(|&t| t > now)?)))
            .collect();
        let full = pending.iter().filter(|(used, _)| *used >= 100.0);
        if let Some(t) = full.map(|&(_, t)| t).max() {
            return Some(t);
        }
        if !hit {
            return None;
        }
        pending
            .iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|&(_, t)| t)
    }
    /// The fullest limit at `now`, with its name, when it is at
    /// `PACE_AT` or more: the runner starts nothing then, rather than learn
    /// at 100 % that a session it started cannot finish its turn. A limit
    /// whose reset has passed is empty again.
    pub fn too_full(&self, now: u64) -> Option<(&'static str, f32)> {
        self.named()
            .into_iter()
            .map(|(name, l)| (name, l.at(now).0))
            .filter(|&(_, used)| used >= PACE_AT)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// How full, in percent, a limit may be before the runner starts no more.
pub const PACE_AT: f32 = 90.0;

/// The limits as last heard, and when, in Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub limits: Limits,
    pub at: u64,
}

/// What one status line call said about its session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The model's name as Claude Code shows it, "Opus 5.5".
    pub model: Option<String>,
    /// How full the context window is, in percent.
    pub context: Option<f32>,
    pub limits: Limits,
}

impl Status {
    /// Reads the status line's input. Everything is optional: early in a
    /// session the context is not measured yet and the limits come after
    /// the first reply.
    pub fn from_json(body: &[u8]) -> Option<Status> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let limits = v.get("rate_limits");
        let limit = |name: &str| limits.and_then(|l| l.get(name)).and_then(Limit::from_json);
        Some(Status {
            model: v
                .pointer("/model/display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            context: v
                .pointer("/context_window/used_percentage")
                .and_then(Value::as_f64)
                .map(|p| p as f32),
            limits: Limits {
                five_hour: limit("five_hour"),
                seven_day: limit("seven_day"),
                spend: limit("spend_limit"),
            },
        })
    }

    /// What the status line prints in the terminal: the model, the context
    /// and the five hour limit, which is the one that runs out first.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(m) = &self.model {
            parts.push(m.clone());
        }
        if let Some(c) = self.context {
            parts.push(format!("context {}%", c.round()));
        }
        if let Some(l) = self.limits.five_hour {
            parts.push(format!("session {}%", l.used.round()));
        }
        parts.join(" \u{00B7} ")
    }
}

/// "40 min", "2 h 10 min", "3 d 4 h". Only as fine as a reset needs.
pub fn format_until(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{} min", m.max(1)),
        (0, h) => format!("{h} h {m:02} min"),
        (d, h) => format!("{d} d {h} h"),
    }
}

/// A setting Horadric passes to every `claude` it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Model,
    Effort,
    Permissions,
}

impl Setting {
    pub const ALL: [Setting; 3] = [Setting::Model, Setting::Effort, Setting::Permissions];

    pub fn label(self) -> &'static str {
        match self {
            Setting::Model => "Model",
            Setting::Effort => "Effort",
            Setting::Permissions => "Permissions",
        }
    }

    /// What `agent` takes, and what the window calls it. Models by their
    /// full names, so the version picked is the version that runs, and an
    /// alias moving on to a newer model never changes it behind your back.
    /// The last permission mode sits apart in its list, so it is never
    /// picked by a slip. Codex's and Grok's are what their own pickers
    /// list, and they have no permission modes to pick here.
    pub fn choices(self, agent: Agent) -> &'static [(&'static str, &'static str)] {
        match (agent, self) {
            (Agent::Codex, Setting::Model) => &[
                ("gpt-6-luna", "GPT-6 Luna"),
                ("gpt-5.6-terra", "GPT-5.6 Terra"),
                ("gpt-5.6-luna", "GPT-5.6 Luna"),
                ("gpt-5.5", "GPT-5.5"),
            ],
            (Agent::Codex, Setting::Effort) => &[
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
                ("max", "Max"),
            ],
            (Agent::Grok, Setting::Model) => &[("grok-4.7", "Grok 4.7")],
            (Agent::Grok, Setting::Effort) => &[
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
            ],
            (Agent::Codex | Agent::Grok, Setting::Permissions) => &[],
            (Agent::Claude, Setting::Model) => &[
                ("claude-fable-5-1", "Fable 5.1"),
                ("claude-opus-5-5", "Opus 5.5"),
                ("claude-opus-5-5[1m]", "Opus 5.5 1M"),
                ("claude-sonnet-5", "Sonnet 5"),
                ("claude-haiku-4-5-20251001", "Haiku 4.5"),
            ],
            (Agent::Claude, Setting::Effort) => &[
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
                ("max", "Max"),
            ],
            (Agent::Claude, Setting::Permissions) => &[
                ("manual", "Ask first"),
                ("acceptEdits", "Accept edits"),
                ("plan", "Plan"),
                ("auto", "Auto"),
                ("dontAsk", "Don't ask"),
                ("bypassPermissions", "Bypass permissions"),
            ],
        }
    }

    /// Effort is a scale, so the window shows it as a slider. The others
    /// are lists.
    pub fn is_scale(self) -> bool {
        self == Setting::Effort
    }

    /// What to type into a running `claude` to switch it to `value`, None
    /// for the default. Only model and effort have a command. Both switch
    /// this session alone and leave Claude Code's own settings be. The
    /// permission mode has none, so it waits for the next start.
    pub fn command(self, value: Option<&str>) -> Option<String> {
        match self {
            Setting::Model => Some(format!("/model {}", value.unwrap_or("default"))),
            Setting::Effort => Some(format!("/effort {}", value.unwrap_or("auto"))),
            Setting::Permissions => None,
        }
    }

    /// Whether a session started with `args` chose this setting itself,
    /// so the defaults leave it alone.
    pub fn chosen_by(self, args: &[String]) -> bool {
        Agent::Claude.chosen(self, args)
    }

    /// What a value is called, or "Default" for none, which leaves it to
    /// the agent's own settings.
    pub fn name_of(self, agent: Agent, value: Option<&str>) -> &'static str {
        let Some(value) = value else {
            return "Default";
        };
        self.choices(agent)
            .iter()
            .find(|(v, _)| *v == value)
            .map_or("Custom", |(_, name)| name)
    }
}

/// What every session Horadric starts or resumes gets, unless its own
/// arguments say otherwise. None leaves a setting to Claude Code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
}

impl Defaults {
    pub fn get(&self, s: Setting) -> Option<&str> {
        match s {
            Setting::Model => self.model.as_deref(),
            Setting::Effort => self.effort.as_deref(),
            Setting::Permissions => self.permission_mode.as_deref(),
        }
    }

    pub fn set(&mut self, s: Setting, value: Option<String>) {
        match s {
            Setting::Model => self.model = value,
            Setting::Effort => self.effort = value,
            Setting::Permissions => self.permission_mode = value,
        }
    }

    /// The flags to put before the own `args` of a session of `agent`,
    /// spelled its way. A setting its arguments already make is left to
    /// them.
    pub fn flags(&self, agent: Agent, args: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for s in Setting::ALL {
            let Some(value) = self.get(s) else {
                continue;
            };
            if agent.chosen(s, args) {
                continue;
            }
            out.extend(agent.setting_args(s, value).unwrap_or_default());
        }
        out
    }
}

/// Whether `args` hold `flag`, alone or as `flag=value`.
pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| {
        a == flag
            || a.strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &[u8] = br#"{
        "session_id": "abc",
        "model": {"id": "claude-opus-5-5", "display_name": "Opus 5.5"},
        "cost": {"total_cost_usd": 0.42},
        "context_window": {"used_percentage": 23.4, "context_window_size": 200000},
        "rate_limits": {
            "five_hour": {"used_percentage": 41.2, "resets_at": 1738425600},
            "seven_day": {"used_percentage": 12, "resets_at": 1738857600},
            "unknown_window": {"used_percentage": 1}
        }
    }"#;

    #[test]
    fn reads_the_status_line_input() {
        let s = Status::from_json(FULL).unwrap();
        assert_eq!(s.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(s.context, Some(23.4));
        assert_eq!(
            s.limits.five_hour,
            Some(Limit {
                used: 41.2,
                resets_at: Some(1738425600),
                minutes: None,
            })
        );
        assert_eq!(s.limits.seven_day.map(|l| l.used), Some(12.0));
        assert_eq!(s.limits.spend, None);
        let names: Vec<&str> = s.limits.named().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Session", "Week"]);
    }

    #[test]
    fn early_input_has_nothing_measured_yet() {
        let s = Status::from_json(
            br#"{"model": {"display_name": "Haiku"},
            "context_window": {"used_percentage": null}}"#,
        )
        .unwrap();
        assert_eq!(s.context, None);
        assert!(s.limits.is_empty());
        assert_eq!(s.line(), "Haiku");
        assert_eq!(Status::from_json(b"not json"), None);
    }

    #[test]
    fn the_line_says_model_context_and_session() {
        let s = Status::from_json(FULL).unwrap();
        assert_eq!(
            s.line(),
            "Opus 5.5 \u{00B7} context 23% \u{00B7} session 41%"
        );
        assert_eq!(Status::default().line(), "");
    }

    const CODEX: &str = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"x"}}
{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":{"used_percent":3.0,"window_minutes":300,"resets_at":100},"secondary":{"used_percent":20.5,"window_minutes":10080,"resets_at":900},"credits":null}}}
{"type":"response_item","payload":{"type":"message","content":"token_count"}}
{"type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"limit_id":"codex","primary":{"used_percent":1.0,"window_minutes":43200,"resets_at":1793333433},"secondary":null,"plan_type":"free"}}}
{"type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":null}}
{"type":"event_msg","payload":{"type":"agent_message","message":"pong"}}"#;

    #[test]
    fn codex_limits_come_from_its_newest_token_count() {
        let l = Limits::from_codex(CODEX).unwrap();
        assert_eq!(
            l.five_hour,
            Some(Limit {
                used: 1.0,
                resets_at: Some(1793333433),
                minutes: Some(43200),
            })
        );
        assert_eq!(l.seven_day, None);
        // A free plan's one limit is a month long, and says so.
        let names: Vec<&str> = l.named().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Month"]);
        // Cut before the newest, the one before it is the newest.
        let older = &CODEX[..CODEX
            .find(
                "
{\"type\":\"response_item",
            )
            .unwrap()];
        let l = Limits::from_codex(older).unwrap();
        assert_eq!(l.five_hour.map(|l| l.used), Some(3.0));
        assert_eq!(l.seven_day.map(|l| l.used), Some(20.5));
        let names: Vec<&str> = l.named().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Session", "Week"]);
        assert_eq!(
            Limits::from_codex("{\"type\":\"token_count\"} not json"),
            None
        );
        assert_eq!(Limits::from_codex(""), None);
    }

    #[test]
    fn a_window_of_no_usual_length_keeps_its_place_name() {
        let l = Limits {
            five_hour: Some(Limit {
                used: 1.0,
                resets_at: None,
                minutes: Some(90 * 60),
            }),
            ..Limits::default()
        };
        assert_eq!(l.named()[0].0, "Session");
        assert_eq!(
            serde_json::to_string(&l.five_hour).unwrap(),
            r#"{"used":1.0,"resets_at":null,"minutes":5400}"#
        );
    }

    #[test]
    fn a_limit_past_its_reset_is_empty() {
        let l = Limit {
            used: 80.0,
            resets_at: Some(1000),
            minutes: None,
        };
        assert_eq!(l.at(400), (80.0, Some(600)));
        assert_eq!(l.at(1000), (0.0, None));
        let open = Limit {
            used: 5.0,
            resets_at: None,
            minutes: None,
        };
        assert_eq!(open.at(1000), (5.0, None));
    }

    fn limit(used: f32, resets_at: u64) -> Option<Limit> {
        Some(Limit {
            used,
            resets_at: Some(resets_at),
            minutes: None,
        })
    }

    #[test]
    fn a_full_limit_holds_until_it_resets() {
        let l = Limits {
            five_hour: limit(100.0, 500),
            seven_day: limit(60.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, false), Some(500));
        // Its reset has passed, so it is empty again.
        assert_eq!(l.out_until(500, false), None);
    }

    #[test]
    fn every_full_limit_has_to_reset() {
        let l = Limits {
            five_hour: limit(100.0, 500),
            seven_day: limit(100.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, false), Some(9000));
    }

    #[test]
    fn a_limit_not_full_holds_nothing() {
        let l = Limits {
            five_hour: limit(99.0, 500),
            ..Limits::default()
        };
        assert_eq!(l.out_until(100, false), None);
        assert_eq!(Limits::default().out_until(100, true), None);
    }

    #[test]
    fn a_refused_session_waits_for_the_fullest_limit() {
        let l = Limits {
            five_hour: limit(97.0, 500),
            seven_day: limit(40.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, true), Some(500));
        // Nothing left to reset, so nothing to wait for.
        assert_eq!(l.out_until(9000, true), None);
    }

    #[test]
    fn the_runner_paces_at_ninety_percent_of_the_fullest_limit() {
        let l = Limits {
            five_hour: limit(92.0, 500),
            seven_day: limit(95.0, 9000),
            spend: None,
        };
        assert_eq!(l.too_full(100), Some(("Week", 95.0)));
        let under = Limits {
            five_hour: limit(89.9, 500),
            ..Limits::default()
        };
        assert_eq!(under.too_full(100), None);
        assert_eq!(Limits::default().too_full(100), None);
        let edge = Limits {
            five_hour: limit(90.0, 500),
            ..Limits::default()
        };
        assert_eq!(edge.too_full(100), Some(("Session", 90.0)));
        // Reset since it was heard, so it is empty again.
        assert_eq!(edge.too_full(500), None);
    }

    #[test]
    fn resets_read_like_a_human_wrote_them() {
        assert_eq!(format_until(20), "1 min");
        assert_eq!(format_until(40 * 60), "40 min");
        assert_eq!(format_until(2 * 3600 + 5 * 60), "2 h 05 min");
        assert_eq!(format_until(3 * 86_400 + 4 * 3600 + 59), "3 d 4 h");
    }

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn defaults_go_in_front_unless_the_session_says_otherwise() {
        let d = Defaults {
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
        };
        assert_eq!(
            d.flags(Agent::Claude, &[]),
            args("--model opus --effort high --permission-mode plan")
        );
        assert_eq!(
            d.flags(
                Agent::Claude,
                &args("--model=haiku --dangerously-skip-permissions")
            ),
            args("--effort high")
        );
        assert_eq!(
            d.flags(Agent::Claude, &args("--effort max --permission-mode auto")),
            args("--model opus")
        );
        assert!(Defaults::default().flags(Agent::Claude, &[]).is_empty());
    }

    #[test]
    fn a_flag_is_itself_or_itself_with_a_value() {
        assert!(has_flag(&args("-p x --model haiku"), "--model"));
        assert!(has_flag(&args("--model=haiku"), "--model"));
        assert!(!has_flag(&args("--models haiku"), "--model"));
        assert!(!has_flag(&args("--fallback-model haiku"), "--model"));
    }

    #[test]
    fn settings_name_their_values() {
        let mut d = Defaults::default();
        let claude = Agent::Claude;
        assert_eq!(
            Setting::Effort.name_of(claude, d.get(Setting::Effort)),
            "Default"
        );
        d.set(Setting::Effort, Some("xhigh".into()));
        assert_eq!(
            Setting::Effort.name_of(claude, d.get(Setting::Effort)),
            "Extra high"
        );
        assert_eq!(
            Setting::Model.name_of(claude, Some("claude-opus-5-5")),
            "Opus 5.5"
        );
        assert_eq!(Setting::Model.name_of(claude, Some("opus")), "Custom");
        assert_eq!(
            Setting::Model.name_of(Agent::Codex, Some("gpt-5.5")),
            "GPT-5.5"
        );
        for a in Agent::ALL {
            for &s in a.settings() {
                assert!(!s.choices(a).is_empty(), "{a:?} {s:?}");
            }
        }
        // Each agent's own scale: Grok's stops short of Max.
        let last = |a: Agent| Setting::Effort.choices(a).last().map(|c| c.0);
        assert_eq!(last(Agent::Claude), Some("max"));
        assert_eq!(last(Agent::Codex), Some("max"));
        assert_eq!(last(Agent::Grok), Some("xhigh"));
    }

    #[test]
    fn model_and_effort_switch_a_running_session_permissions_do_not() {
        assert_eq!(
            Setting::Model.command(Some("claude-sonnet-5")).as_deref(),
            Some("/model claude-sonnet-5")
        );
        assert_eq!(
            Setting::Model.command(None).as_deref(),
            Some("/model default")
        );
        assert_eq!(
            Setting::Effort.command(Some("xhigh")).as_deref(),
            Some("/effort xhigh")
        );
        assert_eq!(
            Setting::Effort.command(None).as_deref(),
            Some("/effort auto")
        );
        assert_eq!(Setting::Permissions.command(Some("plan")), None);
        assert!(Setting::Effort.is_scale() && !Setting::Model.is_scale());
    }

    #[test]
    fn a_session_that_chose_a_setting_keeps_it() {
        assert!(Setting::Model.chosen_by(&args("--model=haiku")));
        assert!(Setting::Permissions.chosen_by(&args("--dangerously-skip-permissions")));
        assert!(!Setting::Effort.chosen_by(&args("--model haiku")));
    }

    #[test]
    fn defaults_and_usage_round_trip() {
        let d = Defaults {
            model: Some("sonnet".into()),
            ..Default::default()
        };
        let back: Defaults = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        let u = Usage {
            limits: Status::from_json(FULL).unwrap().limits,
            at: 5,
        };
        let back: Usage = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(back, u);
    }
}
