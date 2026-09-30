//! The accounts Horadric can switch between, for each agent.
//!
//! Claude Code keeps one login. On Windows its token is in
//! `.credentials.json` under `claudeAiOauth`, beside the tokens of MCP
//! servers, and who it belongs to is in `.claude.json` under
//! `oauthAccount`. Everything else, the hooks, settings and conversations,
//! does not care which account is logged in. So an account is those two
//! pieces, kept aside, and switching puts another pair in their place and
//! leaves the rest of both files as it was.
//!
//! Codex keeps its login in `$CODEX_HOME/auth.json` and nothing else in
//! that file, so the whole file is the login. The account is its
//! `account_id`, and the email is in the `id_token`. Grok keeps one entry
//! in `$GROK_HOME/auth.json` per sign in service, and that entry is the
//! login.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent::Agent;
use crate::release::base64_decode;
use crate::usage::Usage;

/// The key in `.credentials.json` that holds the Claude login.
const OAUTH: &str = "claudeAiOauth";
/// The organization the login is for, beside it.
const ORG: &str = "organizationUuid";
/// The key in `.claude.json` that says whose login it is.
const PROFILE: &str = "oauthAccount";

/// One account's login, kept while another is in use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Whose login it is. Missing reads as Claude's, so the accounts kept
    /// before there were others load.
    #[serde(default)]
    pub agent: Agent,
    /// The account and organization, which together pick one login: the
    /// same person can be in two organizations with a plan each. Codex's
    /// and Grok's begin with the agent's name, so no two agents share one.
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub org_name: Option<String>,
    /// `claudeAiOauth`, as Claude Code wrote it.
    #[serde(default)]
    pub oauth: Value,
    /// `organizationUuid` beside it, when there was one.
    #[serde(default)]
    pub org: Option<Value>,
    /// `oauthAccount`, as Claude Code wrote it.
    #[serde(default)]
    pub profile: Value,
    /// Codex's whole `auth.json`, or Grok's one entry as `{key: entry}`,
    /// as the agent wrote it.
    #[serde(default)]
    pub login: Value,
    /// Its limits as last heard, shown again when it comes back.
    #[serde(default)]
    pub usage: Option<Usage>,
}

impl Account {
    /// The login in `credentials` and `config`, the contents of
    /// `.credentials.json` and `.claude.json`. None without a Claude login,
    /// as with an API key, and None when the two disagree on the
    /// organization: a login caught half written, its token new and its
    /// profile still the old account's, which kept would give the old
    /// account the new one's token.
    pub fn capture(credentials: &Value, config: &Value) -> Option<Account> {
        let oauth = credentials.get(OAUTH).filter(|v| v.is_object())?;
        oauth.get("refreshToken")?.as_str()?;
        let profile = config.get(PROFILE).filter(|v| v.is_object())?;
        if let (Some(a), Some(b)) = (
            credentials.get(ORG).and_then(Value::as_str),
            profile.get(ORG).and_then(Value::as_str),
        ) {
            if a != b {
                return None;
            }
        }
        Some(Account {
            agent: Agent::Claude,
            id: identify(config)?,
            email: profile.get("emailAddress")?.as_str()?.to_string(),
            org_name: profile
                .get("organizationName")
                .and_then(Value::as_str)
                .map(str::to_string),
            oauth: oauth.clone(),
            org: credentials.get(ORG).cloned(),
            profile: profile.clone(),
            login: Value::Null,
            usage: None,
        })
    }

    /// The login in Codex's `auth.json`, `auth`. None without a ChatGPT
    /// login, as with an API key.
    pub fn capture_codex(auth: &Value) -> Option<Account> {
        let tokens = auth.get("tokens").filter(|t| t.is_object())?;
        tokens.get("refresh_token")?.as_str()?;
        let account = tokens.get("account_id")?.as_str()?;
        let claims = jwt_claims(tokens.get("id_token")?.as_str()?)?;
        let user = claims
            .get(OPENAI_AUTH)
            .and_then(|a| a.get("chatgpt_user_id"))
            .or_else(|| claims.get("sub"))
            .and_then(Value::as_str)
            .unwrap_or("");
        Some(Account {
            agent: Agent::Codex,
            id: format!("codex:{account}:{user}"),
            email: claims.get("email")?.as_str()?.to_string(),
            org_name: None,
            oauth: Value::Null,
            org: None,
            profile: Value::Null,
            login: auth.clone(),
            usage: None,
        })
    }

    /// The login in Grok's `auth.json`, `auth`: its first entry that has
    /// an address and a token.
    pub fn capture_grok(auth: &Value) -> Option<Account> {
        let (key, entry) = auth.as_object()?.iter().find(|(_, e)| {
            e.get("email").and_then(Value::as_str).is_some()
                && ["key", "refresh_token"]
                    .iter()
                    .any(|k| e.get(k).and_then(Value::as_str).is_some())
        })?;
        let email = entry.get("email")?.as_str()?;
        let field = |k: &str| entry.get(k).and_then(Value::as_str).unwrap_or("");
        let user = match field("user_id") {
            "" => email,
            u => u,
        };
        let mut login = Map::new();
        login.insert(key.clone(), entry.clone());
        Some(Account {
            agent: Agent::Grok,
            id: format!("grok:{user}:{}", field("team_id")),
            email: email.to_string(),
            org_name: None,
            oauth: Value::Null,
            org: None,
            profile: Value::Null,
            login: Value::Object(login),
            usage: None,
        })
    }

    /// Puts this Codex or Grok login into the contents of the agent's
    /// `auth.json`. Codex's is the whole file. Grok's is its entry, and
    /// any other entry stays.
    pub fn put_login(&self, auth: &mut Value) {
        match self.agent {
            Agent::Claude => {}
            Agent::Codex => *auth = self.login.clone(),
            Agent::Grok => {
                if !auth.is_object() {
                    *auth = Value::Object(Default::default());
                }
                if let (Some(map), Some(login)) = (auth.as_object_mut(), self.login.as_object()) {
                    for (k, v) in login {
                        map.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }

    /// Puts this login into `.credentials.json`'s contents, leaving the
    /// MCP servers' tokens and anything else there alone.
    pub fn put_credentials(&self, credentials: &mut Value) {
        if !credentials.is_object() {
            *credentials = Value::Object(Default::default());
        }
        let Some(map) = credentials.as_object_mut() else {
            return;
        };
        map.insert(OAUTH.into(), self.oauth.clone());
        match &self.org {
            Some(org) => map.insert(ORG.into(), org.clone()),
            None => map.remove(ORG),
        };
    }

    /// Puts whose login it is into `.claude.json`'s contents.
    pub fn put_profile(&self, config: &mut Value) {
        if let Some(map) = config.as_object_mut() {
            map.insert(PROFILE.into(), self.profile.clone());
        }
    }

    /// What the plan is called, "Max 5x", from the login's tier.
    pub fn plan(&self) -> Option<String> {
        match self.agent {
            Agent::Claude => {
                let tier = self.oauth.get("rateLimitTier").and_then(Value::as_str);
                let kind = self.oauth.get("subscriptionType").and_then(Value::as_str);
                plan_name(kind, tier)
            }
            Agent::Codex => {
                let token = self.login.get("tokens")?.get("id_token")?.as_str()?;
                let claims = jwt_claims(token)?;
                let kind = claims.get(OPENAI_AUTH)?.get("chatgpt_plan_type")?;
                capitalised(kind.as_str()?)
            }
            Agent::Grok => None,
        }
    }

    /// What a menu calls it: the address, then the plan and the
    /// organization when there is one worth naming.
    pub fn label(&self) -> String {
        let mut detail = Vec::new();
        if let Some(p) = self.plan() {
            detail.push(p);
        }
        if let Some(o) = self
            .org_name
            .as_deref()
            .filter(|o| !is_personal(o, &self.email))
        {
            detail.push(o.to_string());
        }
        match detail.is_empty() {
            true => self.email.clone(),
            false => format!("{}\t{}", self.email, detail.join(", ")),
        }
    }
}

/// Who is logged in, by `.claude.json`'s contents: the account and the
/// organization.
pub fn identify(config: &Value) -> Option<String> {
    let profile = config.get(PROFILE)?;
    let account = profile.get("accountUuid")?.as_str()?;
    let org = profile
        .get("organizationUuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!("{account}:{org}"))
}

/// An organization Claude names after its one member says nothing.
fn is_personal(org: &str, email: &str) -> bool {
    org.contains(email) || org.ends_with("'s Organization") || org.ends_with("’s Organization")
}

/// "Max 20x" from `default_claude_max_20x`, "Pro" from `pro`.
fn plan_name(kind: Option<&str>, tier: Option<&str>) -> Option<String> {
    if let Some(n) = tier.and_then(|t| t.rsplit_once("max_")).map(|(_, n)| n) {
        return Some(format!("Max {n}"));
    }
    capitalised(kind?)
}

fn capitalised(word: &str) -> Option<String> {
    let mut c = word.chars();
    let first = c.next()?.to_uppercase();
    Some(first.chain(c).collect())
}

/// The claim in an OpenAI token that holds the ChatGPT account.
const OPENAI_AUTH: &str = "https://api.openai.com/auth";

/// What a JSON web token says about its holder: its middle part, which is
/// base64url JSON. Nothing here checks its signature, and nothing needs
/// to: it only names an account the user logged in to themselves.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let mut text: String = payload
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    while !text.len().is_multiple_of(4) {
        text.push('=');
    }
    serde_json::from_slice(&base64_decode(&text)?).ok()
}

/// Every account logged in to so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default)]
    pub list: Vec<Account>,
}

impl Accounts {
    pub fn get(&self, id: &str) -> Option<&Account> {
        self.list.iter().find(|a| a.id == id)
    }

    /// `agent`'s accounts, in the order they were first kept.
    pub fn of(&self, agent: Agent) -> impl Iterator<Item = &Account> {
        self.list.iter().filter(move |a| a.agent == agent)
    }

    /// Keeps `account`'s login as it is now, over what was kept for it,
    /// and its limits unless it brings some. A token Claude Code refreshed
    /// replaces the old one this way, which may no longer work. True when
    /// anything changed.
    pub fn remember(&mut self, mut account: Account) -> bool {
        match self.list.iter_mut().find(|a| a.id == account.id) {
            Some(kept) => {
                if account.usage.is_none() {
                    account.usage = kept.usage.clone();
                }
                let changed = *kept != account;
                *kept = account;
                changed
            }
            None => {
                self.list.push(account);
                true
            }
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.list.retain(|a| a.id != id);
    }

    /// Notes the limits last heard for account `id`.
    pub fn set_usage(&mut self, id: &str, usage: Option<Usage>) {
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            if usage.is_some() {
                a.usage = usage;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Limit, Limits};
    use serde_json::json;

    fn credentials(token: &str) -> Value {
        json!({
            "mcpOAuth": { "figma": { "accessToken": "keep" } },
            "claudeAiOauth": {
                "accessToken": format!("a-{token}"),
                "refreshToken": format!("r-{token}"),
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_5x"
            },
            "organizationUuid": format!("org-{token}")
        })
    }

    /// The profile for `credentials(token)`.
    fn config(token: &str, email: &str) -> Value {
        json!({
            "numStartups": 3,
            "oauthAccount": {
                "accountUuid": format!("u-{token}"),
                "organizationUuid": format!("org-{token}"),
                "emailAddress": email,
                "organizationName": format!("{email}'s Organization")
            }
        })
    }

    #[test]
    fn a_login_is_its_token_and_its_profile() {
        let a = Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap();
        assert_eq!(a.id, "u-one:org-one");
        assert_eq!(a.email, "a@x.dk");
        assert_eq!(a.oauth["refreshToken"], "r-one");
        assert_eq!(a.org, Some(json!("org-one")));
        assert_eq!(a.plan().as_deref(), Some("Max 5x"));
        assert_eq!(a.label(), "a@x.dk\tMax 5x");
    }

    #[test]
    fn no_login_without_a_claude_token_or_a_profile() {
        let key_only = json!({ "mcpOAuth": {} });
        assert!(Account::capture(&key_only, &config("one", "a@x.dk")).is_none());
        assert!(Account::capture(&credentials("one"), &json!({})).is_none());
    }

    #[test]
    fn switching_keeps_the_rest_of_both_files() {
        let b = Account::capture(&credentials("two"), &config("two", "b@x.dk")).unwrap();
        let mut creds = credentials("one");
        let mut conf = config("one", "a@x.dk");
        b.put_credentials(&mut creds);
        b.put_profile(&mut conf);
        assert_eq!(creds["mcpOAuth"]["figma"]["accessToken"], "keep");
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "r-two");
        assert_eq!(creds["organizationUuid"], "org-two");
        assert_eq!(conf["numStartups"], 3);
        assert_eq!(identify(&conf).as_deref(), Some("u-two:org-two"));
    }

    #[test]
    fn a_login_without_an_organization_takes_the_old_one_out() {
        let mut b = Account::capture(&credentials("two"), &config("two", "b@x.dk")).unwrap();
        b.org = None;
        let mut creds = credentials("one");
        b.put_credentials(&mut creds);
        assert!(creds.get("organizationUuid").is_none());
    }

    #[test]
    fn a_refreshed_token_replaces_the_kept_one_and_keeps_the_limits() {
        let mut all = Accounts::default();
        let mut a = Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap();
        a.usage = Some(Usage {
            limits: Limits {
                five_hour: Some(Limit {
                    used: 40.0,
                    resets_at: None,
                    minutes: None,
                }),
                ..Default::default()
            },
            at: 1,
        });
        assert!(all.remember(a));
        let mut refreshed = credentials("three");
        refreshed["organizationUuid"] = json!("org-one");
        let fresh = Account::capture(&refreshed, &config("one", "a@x.dk")).unwrap();
        assert!(all.remember(fresh.clone()));
        assert!(!all.remember(fresh));
        assert_eq!(all.list.len(), 1);
        let kept = all.get("u-one:org-one").unwrap();
        assert_eq!(kept.oauth["refreshToken"], "r-three");
        assert!(kept.usage.is_some());
    }

    #[test]
    fn a_login_caught_half_written_is_not_kept() {
        assert!(Account::capture(&credentials("two"), &config("one", "a@x.dk")).is_none());
    }

    #[test]
    fn plans_are_named_by_tier_then_by_kind() {
        assert_eq!(
            plan_name(Some("team"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(plan_name(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(plan_name(None, None), None);
    }

    /// A token whose middle part is `claims`, as a JWT carries them.
    fn jwt(claims: Value) -> String {
        let body = crate::release::base64_encode(claims.to_string().as_bytes())
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        format!("e30.{body}.sig")
    }

    fn codex_auth(account: &str, email: &str) -> Value {
        json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": jwt(json!({
                    "email": email,
                    "sub": "auth0|x",
                    OPENAI_AUTH: {
                        "chatgpt_plan_type": "plus",
                        "chatgpt_user_id": format!("user-{email}")
                    }
                })),
                "access_token": "a",
                "refresh_token": format!("r-{account}"),
                "account_id": account
            },
            "last_refresh": "2026-09-30T10:00:00Z"
        })
    }

    #[test]
    fn a_codex_login_is_its_whole_file() {
        let auth = codex_auth("acc-1", "a@x.dk");
        let a = Account::capture_codex(&auth).unwrap();
        assert_eq!(a.agent, Agent::Codex);
        assert_eq!(a.id, "codex:acc-1:user-a@x.dk");
        assert_eq!(a.email, "a@x.dk");
        assert_eq!(a.plan().as_deref(), Some("Plus"));
        let mut file = codex_auth("acc-2", "b@x.dk");
        a.put_login(&mut file);
        assert_eq!(file, auth);
    }

    #[test]
    fn no_codex_login_with_an_api_key() {
        let auth = json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "sk", "tokens": null });
        assert!(Account::capture_codex(&auth).is_none());
    }

    #[test]
    fn a_token_is_read_whatever_its_padding() {
        for email in ["a@x.dk", "ab@x.dk", "abc@x.dk"] {
            let claims = jwt_claims(&jwt(json!({ "email": email }))).unwrap();
            assert_eq!(claims["email"], email);
        }
        assert!(jwt_claims("no-dots").is_none());
    }

    fn grok_auth(user: &str, email: &str) -> Value {
        json!({
            "https://auth.x.ai::client": {
                "key": format!("k-{user}"),
                "user_id": user,
                "team_id": "t",
                "email": email,
                "refresh_token": format!("r-{user}"),
                "expires_at": "2026-10-01T00:00:00Z"
            }
        })
    }

    #[test]
    fn a_grok_login_is_its_entry_and_the_rest_stays() {
        let a = Account::capture_grok(&grok_auth("u1", "a@x.dk")).unwrap();
        assert_eq!(a.agent, Agent::Grok);
        assert_eq!(a.id, "grok:u1:t");
        assert_eq!(a.email, "a@x.dk");
        let mut file = grok_auth("u2", "b@x.dk");
        file["other::service"] = json!({ "note": "keep" });
        a.put_login(&mut file);
        assert_eq!(file["https://auth.x.ai::client"]["key"], "k-u1");
        assert_eq!(file["other::service"]["note"], "keep");
        assert!(Account::capture_grok(&json!({})).is_none());
    }

    #[test]
    fn accounts_kept_before_other_agents_load_as_claude() {
        let mut old = serde_json::to_value(
            Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap(),
        )
        .unwrap();
        let map = old.as_object_mut().unwrap();
        map.remove("agent");
        map.remove("login");
        let a: Account = serde_json::from_value(old).unwrap();
        assert_eq!(a.agent, Agent::Claude);
        let mut all = Accounts::default();
        all.remember(a);
        all.remember(Account::capture_grok(&grok_auth("u1", "a@x.dk")).unwrap());
        assert_eq!(all.of(Agent::Grok).count(), 1);
        assert_eq!(all.of(Agent::Claude).count(), 1);
    }

    #[test]
    fn a_named_organization_shows_in_the_label() {
        let mut conf = config("one", "a@x.dk");
        conf["oauthAccount"]["organizationName"] = json!("Optipeople");
        let a = Account::capture(&credentials("one"), &conf).unwrap();
        assert_eq!(a.label(), "a@x.dk\tMax 5x, Optipeople");
    }
}
