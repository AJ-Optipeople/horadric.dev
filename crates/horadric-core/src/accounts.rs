//! The Claude accounts Horadric can switch between.
//!
//! Claude Code keeps one login. On Windows its token is in
//! `.credentials.json` under `claudeAiOauth`, beside the tokens of MCP
//! servers, and who it belongs to is in `.claude.json` under
//! `oauthAccount`. Everything else, the hooks, settings and conversations,
//! does not care which account is logged in. So an account is those two
//! pieces, kept aside, and switching puts another pair in their place and
//! leaves the rest of both files as it was.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// The account and organization, which together pick one login: the
    /// same person can be in two organizations with a plan each.
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub org_name: Option<String>,
    /// `claudeAiOauth`, as Claude Code wrote it.
    pub oauth: Value,
    /// `organizationUuid` beside it, when there was one.
    #[serde(default)]
    pub org: Option<Value>,
    /// `oauthAccount`, as Claude Code wrote it.
    pub profile: Value,
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
            id: identify(config)?,
            email: profile.get("emailAddress")?.as_str()?.to_string(),
            org_name: profile
                .get("organizationName")
                .and_then(Value::as_str)
                .map(str::to_string),
            oauth: oauth.clone(),
            org: credentials.get(ORG).cloned(),
            profile: profile.clone(),
            usage: None,
        })
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
        let tier = self.oauth.get("rateLimitTier").and_then(Value::as_str);
        let kind = self.oauth.get("subscriptionType").and_then(Value::as_str);
        plan_name(kind, tier)
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
    let kind = kind?;
    let mut c = kind.chars();
    let first = c.next()?.to_uppercase();
    Some(first.chain(c).collect())
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

    #[test]
    fn a_named_organization_shows_in_the_label() {
        let mut conf = config("one", "a@x.dk");
        conf["oauthAccount"]["organizationName"] = json!("Optipeople");
        let a = Account::capture(&credentials("one"), &conf).unwrap();
        assert_eq!(a.label(), "a@x.dk\tMax 5x, Optipeople");
    }
}
