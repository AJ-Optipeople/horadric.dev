//! OSC 7501, the Program Status Protocol: a program tells its terminal
//! whether it is idle, working, blocked on the human, done or failed.
//!
//! The one terminal output Horadric reads for state. It is a status the
//! program sends on purpose, in a fixed format, not a guess from what it
//! shows. Hooks win where both speak (see [`crate::Session::hear_program`]).
//!
//! The spec is revision 0.4 of 2026-10-10, at
//! <https://superlogical.com/rex/docs/build/program-status>. In short:
//! `OSC 7501 ; key=value:key=value ST`, where ST is `ESC \` or BEL, and
//! `OSC 7501 ; ? ST` asks whether the terminal speaks it.

use crate::session::{Phase, WaitReason};

/// What a terminal that speaks the protocol answers to `OSC 7501 ; ?`.
/// Claude Code sends its status only after hearing it.
pub const QUERY_REPLY: &[u8] = b"\x1b]7501;?\x07";

/// `bytes` as win32-input-mode key presses (`CSI Vk;Sc;Uc;Kd;Cs;Rc _`),
/// one per byte. A pseudo console drops any OSC written to its input, so
/// a reply sent as it is never reaches the program. Typed as keys, each
/// byte comes out the other side unchanged.
pub fn as_key_presses(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|b| format!("\x1b[0;0;{b};1;0;1_").into_bytes())
        .collect()
}

/// The spec caps a whole sequence at 4096 bytes, OSC to ST.
const MAX_SEQUENCE: usize = 4096;
/// Encoded lengths the spec allows.
const MAX_MSG: usize = 2732;
const MAX_TITLE: usize = 256;
const MAX_APP: usize = 32;

const REPORT: &[u8] = b"7501;";
/// OSC 133 A, a shell prompt beginning, ends what the last command said.
const PROMPT: &[u8] = b"133;A";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

/// What a blocked program waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

/// One status report. Each replaces its record whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Slash separated; empty for the root record.
    pub id: String,
    pub state: State,
    pub kind: Option<Kind>,
    /// 0 to 100, for working or blocked. None is indeterminate.
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// A sequence the scanner found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sequence {
    Report(Report),
    /// The program asks whether the terminal speaks the protocol.
    Query,
    /// A shell prompt began (OSC 133 A).
    Prompt,
}

impl Report {
    /// The phase a tile shows for it.
    pub fn phase(&self) -> Phase {
        match self.state {
            State::Idle | State::Clear => Phase::Idle,
            State::Working => Phase::Working,
            State::Done => Phase::Done,
            State::Blocked => Phase::Waiting(match self.kind {
                Some(Kind::Permission) => WaitReason::Permission,
                Some(Kind::Auth) => WaitReason::Dialog,
                Some(Kind::Question) | None => WaitReason::Input,
            }),
            State::Error => Phase::Waiting(WaitReason::Error("error".into())),
        }
    }

    /// What a tile says under the name: the message, or the label, with
    /// the progress when the program gives one.
    pub fn line(&self) -> Option<String> {
        let text = self.msg.clone().or_else(|| self.title.clone());
        let progress = self
            .progress
            .filter(|_| matches!(self.state, State::Working | State::Blocked));
        match (text, progress) {
            (Some(t), Some(p)) => Some(format!("{t} ({p}%)")),
            (Some(t), None) => Some(t),
            (None, Some(p)) => Some(format!("{p}%")),
            (None, None) => None,
        }
    }

    fn urgency(&self) -> u8 {
        match self.state {
            State::Blocked => 5,
            State::Error => 4,
            State::Working => 3,
            State::Done => 2,
            State::Idle | State::Clear => 1,
        }
    }
}

/// Parses what came between `ESC ]` and ST. None for anything that is not
/// OSC 7501 or OSC 133 A, and for a report the spec says to discard.
pub fn parse(body: &[u8]) -> Option<Sequence> {
    if let Some(rest) = body.strip_prefix(PROMPT) {
        return (rest.is_empty() || rest[0] == b';').then_some(Sequence::Prompt);
    }
    let rest = body.strip_prefix(REPORT)?;
    if rest.first() == Some(&b'?') {
        return Some(Sequence::Query);
    }
    parse_report(std::str::from_utf8(rest).ok()?).map(Sequence::Report)
}

fn parse_report(pairs: &str) -> Option<Report> {
    let mut state = None;
    let mut report = Report {
        id: String::new(),
        state: State::Idle,
        kind: None,
        progress: None,
        app: None,
        title: None,
        msg: None,
    };
    for pair in pairs.split(':') {
        let (key, value) = pair.split_once('=')?;
        let key_ok = !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase());
        let value_ok = value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b));
        if !key_ok || !value_ok {
            return None;
        }
        // A repeated key: the last value wins, so each one overwrites.
        match key {
            "state" => {
                state = Some(match value {
                    "idle" => State::Idle,
                    "working" => State::Working,
                    "done" => State::Done,
                    "blocked" => State::Blocked,
                    "error" => State::Error,
                    "clear" => State::Clear,
                    _ => return None,
                })
            }
            "id" => report.id = value.to_string(),
            "kind" => {
                report.kind = match value {
                    "permission" => Some(Kind::Permission),
                    "question" => Some(Kind::Question),
                    "auth" => Some(Kind::Auth),
                    _ => None,
                }
            }
            "progress" => report.progress = value.parse().ok().filter(|p| *p <= 100),
            "app" => report.app = (value.len() <= MAX_APP).then(|| value.to_string()),
            "title" => report.title = Some(text(value, MAX_TITLE)?),
            "msg" => report.msg = Some(text(value, MAX_MSG)?),
            // Unknown keys are how the protocol grows.
            _ => {}
        }
    }
    report.state = state?;
    if report.state != State::Blocked {
        report.kind = None;
    }
    Some(report)
}

/// A free text value: base64 of one line of UTF-8.
fn text(value: &str, max: usize) -> Option<String> {
    if value.len() > max {
        return None;
    }
    let text = String::from_utf8(base64(value)?).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}

/// Standard base64, padding optional.
fn base64(value: &str) -> Option<Vec<u8>> {
    let digits = value.trim_end_matches('=');
    if value.len() - digits.len() > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for b in digits.bytes() {
        let d = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(d);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // One digit left over carries too few bits for a byte.
    (digits.len() % 4 != 1).then_some(out)
}

/// Finds OSC 7501 and OSC 133 A in output as it arrives, in chunks that may
/// cut a sequence anywhere. Everything else passes by untouched: the grid's
/// own parser sees the same bytes.
#[derive(Debug, Default)]
pub struct Scanner {
    at: At,
    body: Vec<u8>,
    /// The sequence is not one of ours, or grew past the cap.
    skip: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum At {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
}

impl Scanner {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Sequence>) {
        for &b in bytes {
            self.at = match (self.at, b) {
                (At::Ground, 0x1b) => At::Escape,
                (At::Ground, _) => At::Ground,
                (At::Escape, b']') => self.begin(),
                (At::Escape, 0x1b) => At::Escape,
                (At::Escape, _) => At::Ground,
                (At::Osc, 0x07) | (At::OscEscape, b'\\') => {
                    if !self.skip {
                        out.extend(parse(&self.body));
                    }
                    At::Ground
                }
                (At::Osc, 0x1b) => At::OscEscape,
                // CAN and SUB abort a sequence.
                (At::Osc, 0x18 | 0x1a) => At::Ground,
                (At::Osc, _) => {
                    self.take(b);
                    At::Osc
                }
                // ESC not followed by `\` ends the OSC unfinished and starts
                // whatever comes next.
                (At::OscEscape, b']') => self.begin(),
                (At::OscEscape, 0x1b) => At::Escape,
                (At::OscEscape, _) => At::Ground,
            };
        }
    }

    fn begin(&mut self) -> At {
        self.body.clear();
        self.skip = false;
        At::Osc
    }

    fn take(&mut self, b: u8) {
        if self.skip {
            return;
        }
        self.body.push(b);
        let ours = |p: &[u8]| {
            let n = self.body.len().min(p.len());
            self.body[..n] == p[..n]
        };
        if self.body.len() > MAX_SEQUENCE || !(ours(REPORT) || ours(PROMPT)) {
            self.skip = true;
            self.body.clear();
        }
    }
}

/// The records a terminal holds, one per id, as the spec keeps them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Records(Vec<Report>);

impl Records {
    /// Takes a sequence. True when the records changed.
    pub fn hear(&mut self, seq: &Sequence) -> bool {
        let before = self.0.clone();
        match seq {
            Sequence::Report(r) if r.state == State::Clear => {
                let id = &r.id;
                self.0
                    .retain(|old| !(id.is_empty() || beneath(&old.id, id)));
            }
            Sequence::Report(r) => {
                // The newest last, so it wins a tie in urgency.
                self.0.retain(|old| old.id != r.id);
                self.0.push(r.clone());
            }
            // A prompt drops what was in flight. Done and error stay
            // until seen.
            Sequence::Prompt => self
                .0
                .retain(|r| matches!(r.state, State::Done | State::Error)),
            Sequence::Query => {}
        }
        self.0 != before
    }

    /// The record that says the most to the human: one blocked on them
    /// over a failure, over work, over a result, over rest.
    pub fn summary(&self) -> Option<&Report> {
        self.0.iter().max_by_key(|r| r.urgency())
    }
}

fn beneath(id: &str, parent: &str) -> bool {
    id == parent
        || id
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Sequence> {
        let mut s = Scanner::default();
        let mut out = Vec::new();
        for c in chunks {
            s.feed(c, &mut out);
        }
        out
    }

    fn report(body: &str) -> Report {
        match parse(body.as_bytes()) {
            Some(Sequence::Report(r)) => r,
            other => panic!("{body}: {other:?}"),
        }
    }

    #[test]
    fn a_reply_typed_as_keys_carries_each_byte() {
        assert_eq!(
            as_key_presses(b"\x1b]"),
            b"\x1b[0;0;27;1;0;1_\x1b[0;0;93;1;0;1_"
        );
        assert!(as_key_presses(b"").is_empty());
    }

    #[test]
    fn the_specs_own_example_parses() {
        let r = report(
            "7501;state=blocked:kind=permission:app=terraform:\
             msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/",
        );
        assert_eq!(r.state, State::Blocked);
        assert_eq!(r.kind, Some(Kind::Permission));
        assert_eq!(r.app.as_deref(), Some("terraform"));
        assert_eq!(
            r.msg.as_deref(),
            Some("Apply 3 to add, 1 to change, 0 to destroy?")
        );
        assert_eq!(r.phase(), Phase::Waiting(WaitReason::Permission));
    }

    #[test]
    fn both_terminators_end_a_sequence() {
        let seqs = scan(&[b"a\x1b]7501;state=working\x07b\x1b]7501;state=done\x1b\\c"]);
        let states: Vec<_> = seqs
            .iter()
            .map(|s| match s {
                Sequence::Report(r) => r.state,
                _ => panic!(),
            })
            .collect();
        assert_eq!(states, [State::Working, State::Done]);
    }

    #[test]
    fn a_sequence_cut_anywhere_is_still_found() {
        let whole = b"x\x1b]7501;state=working:progress=40\x1b\\y";
        for cut in 0..whole.len() {
            let seqs = scan(&[&whole[..cut], &whole[cut..]]);
            assert_eq!(seqs.len(), 1, "cut at {cut}");
        }
    }

    #[test]
    fn the_query_and_a_prompt_are_found() {
        assert_eq!(
            scan(&[b"\x1b]7501;?\x1b\\\x1b]133;A;cl=m\x07\x1b]133;B\x07"]),
            [Sequence::Query, Sequence::Prompt]
        );
    }

    #[test]
    fn other_sequences_pass_by() {
        assert!(scan(&[b"\x1b]0;title\x07\x1b]8;;http://x\x1b\\\x1b[1mhi"]).is_empty());
        assert!(scan(&[b"\x1b]75010;state=done\x07"]).is_empty());
    }

    #[test]
    fn an_escape_inside_an_osc_starts_over() {
        assert_eq!(
            scan(&[b"\x1b]7501;state=working\x1b]7501;?\x07"]),
            [Sequence::Query]
        );
        assert!(scan(&[b"\x1b]7501;state=working\x18\x07"]).is_empty());
    }

    #[test]
    fn a_sequence_past_the_cap_is_dropped() {
        let mut long = b"\x1b]7501;state=done:x=".to_vec();
        long.extend(std::iter::repeat_n(b'a', MAX_SEQUENCE));
        long.push(0x07);
        long.extend(b"\x1b]7501;state=idle\x07");
        let seqs = scan(&[&long]);
        assert_eq!(seqs.len(), 1);
    }

    #[test]
    fn a_report_needs_a_known_state() {
        assert!(parse(b"7501;progress=5").is_none());
        assert!(parse(b"7501;state=sleeping").is_none());
        assert!(parse(b"7501;").is_none());
    }

    #[test]
    fn a_malformed_pair_discards_the_report() {
        assert!(parse(b"7501;state=done:Key=x").is_none());
        assert!(parse(b"7501;state=done:noequals").is_none());
        assert!(parse(b"7501;state=done:app=a b").is_none());
    }

    #[test]
    fn unknown_keys_are_ignored_and_the_last_value_wins() {
        let r = report("7501;state=working:future=1:state=done");
        assert_eq!(r.state, State::Done);
    }

    #[test]
    fn bad_text_discards_the_report() {
        // "a\nb"
        assert!(parse(b"7501;state=done:msg=YQpi").is_none());
        assert!(parse(b"7501;state=done:msg=Y").is_none());
        assert!(parse(b"7501;state=done:msg=/w==").is_none());
    }

    #[test]
    fn base64_padding_is_optional() {
        assert_eq!(
            report("7501;state=done:msg=aGk=").msg.as_deref(),
            Some("hi")
        );
        assert_eq!(report("7501;state=done:msg=aGk").msg.as_deref(), Some("hi"));
        assert_eq!(report("7501;state=done:title=").title.as_deref(), Some(""));
    }

    #[test]
    fn progress_out_of_range_is_indeterminate() {
        assert_eq!(report("7501;state=working:progress=101").progress, None);
        assert_eq!(report("7501;state=working:progress=7").progress, Some(7));
    }

    #[test]
    fn kind_belongs_to_blocked_only() {
        assert_eq!(report("7501;state=working:kind=auth").kind, None);
        let r = report("7501;state=blocked:kind=auth");
        assert_eq!(r.phase(), Phase::Waiting(WaitReason::Dialog));
        let r = report("7501;state=blocked");
        assert_eq!(r.phase(), Phase::Waiting(WaitReason::Input));
    }

    #[test]
    fn the_line_carries_progress_while_it_means_something() {
        assert_eq!(
            report("7501;state=working:progress=40:msg=YnVpbGQ").line(),
            Some("build (40%)".into())
        );
        assert_eq!(
            report("7501;state=done:progress=40").line(),
            None,
            "progress means nothing once done"
        );
    }

    fn hear(records: &mut Records, body: &str) -> bool {
        records.hear(&parse(body.as_bytes()).unwrap())
    }

    #[test]
    fn the_most_urgent_record_speaks_for_the_terminal() {
        let mut r = Records::default();
        assert!(hear(&mut r, "7501;state=working"));
        assert!(hear(&mut r, "7501;state=blocked:id=deploy/db"));
        assert_eq!(r.summary().unwrap().state, State::Blocked);
        assert!(!hear(&mut r, "7501;state=blocked:id=deploy/db"));
        assert!(hear(&mut r, "7501;state=clear:id=deploy"));
        assert_eq!(r.summary().unwrap().state, State::Working);
    }

    #[test]
    fn clear_takes_the_record_and_everything_beneath_it() {
        let mut r = Records::default();
        hear(&mut r, "7501;state=working:id=a");
        hear(&mut r, "7501;state=working:id=a/b");
        hear(&mut r, "7501;state=done:id=ab");
        hear(&mut r, "7501;state=clear:id=a");
        assert_eq!(r.summary().unwrap().id, "ab");
        hear(&mut r, "7501;state=clear");
        assert!(r.summary().is_none());
    }

    #[test]
    fn a_prompt_keeps_only_results() {
        let mut r = Records::default();
        hear(&mut r, "7501;state=working:id=a");
        hear(&mut r, "7501;state=done:id=b");
        assert!(r.hear(&Sequence::Prompt));
        assert_eq!(r.summary().unwrap().id, "b");
    }

    #[test]
    fn a_new_report_replaces_its_record_whole() {
        let mut r = Records::default();
        hear(&mut r, "7501;state=working:msg=YQ");
        hear(&mut r, "7501;state=idle");
        assert_eq!(r.summary().unwrap().msg, None);
    }
}
