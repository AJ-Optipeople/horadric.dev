//! A project's fleet: devices by the hundred, too many to list in
//! `hosts`, kept in an inventory file the project already has.
//!
//! The inventory is Ansible's INI form, which a plain list of names also
//! is: `[group]` headings, then one device a line, its name first and
//! `ansible_host`, `ansible_user` and `ansible_port` after it when the name
//! alone does not reach it. `:vars` and `:children` sections say nothing
//! about which devices there are, so they are skipped.

use serde_json::Value;

use crate::ssh;

/// One device of the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// The first group it is listed under, None above any heading.
    pub group: Option<String>,
    /// What `ssh` is given to reach it.
    pub destination: String,
}

/// Where the config says the inventory is, as written: relative to the
/// project, or absolute.
pub fn inventory(config: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(config).ok()?;
    let path = v.get("inventory")?.as_str()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// The devices an inventory lists, in its order, each once under the first
/// group that names it. A device `ssh` could not take as one destination
/// is left out, since the file comes with the repository.
pub fn parse(text: &str) -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    let mut group: Option<String> = None;
    let mut listing = true;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(heading) = line.strip_prefix('[') {
            let heading = heading.split(']').next().unwrap_or_default().trim();
            listing = !heading.contains(':');
            group = Some(heading.to_string()).filter(|g| listing && !g.is_empty());
            continue;
        }
        if !listing {
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            continue;
        };
        let (mut host, mut user, mut port) = (None, None, None);
        for word in words {
            if word.starts_with('#') {
                break;
            }
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            let value = value.trim_matches(['"', '\'']);
            match key {
                "ansible_host" | "ansible_ssh_host" => host = Some(value),
                "ansible_user" | "ansible_ssh_user" => user = Some(value),
                "ansible_port" | "ansible_ssh_port" => port = Some(value),
                _ => {}
            }
        }
        let destination = destination(host.unwrap_or(name), user, port);
        let known = out.iter().any(|d| d.name == name);
        if !known && ssh::usable(name) && ssh::usable(&destination) {
            out.push(Device {
                name: name.to_string(),
                group: group.clone(),
                destination,
            });
        }
    }
    out
}

/// What `ssh` is given for `host` as `user` on `port`. A port needs the
/// `ssh://` form, the one way to put it in a destination.
fn destination(host: &str, user: Option<&str>, port: Option<&str>) -> String {
    let user = user.map(|u| format!("{u}@")).unwrap_or_default();
    match port.filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        Some(p) if host.contains(':') => format!("ssh://{user}[{host}]:{p}"),
        Some(p) => format!("ssh://{user}{host}:{p}"),
        None => format!("{user}{host}"),
    }
}

/// The groups of `devices`, in the order they first appear.
pub fn groups(devices: &[Device]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for g in devices.iter().filter_map(|d| d.group.as_deref()) {
        if !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

/// Up to `limit` of `devices` that `query` finds: every word of it, case
/// aside, somewhere in the name, the group or the destination. Names that
/// start with the query come first, the rest in the inventory's order.
pub fn find<'a>(devices: &'a [Device], query: &str, limit: usize) -> Vec<&'a Device> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let found = devices.iter().filter(|d| {
        let hay = format!(
            "{} {} {}",
            d.name,
            d.group.as_deref().unwrap_or_default(),
            d.destination
        )
        .to_lowercase();
        words.iter().all(|w| hay.contains(w))
    });
    let (mut first, rest): (Vec<&Device>, Vec<&Device>) =
        found.partition(|d| d.name.to_lowercase().starts_with(&query));
    first.extend(rest);
    first.truncate(limit);
    first
}

/// The hosts in `hosts` as devices of no group, then `devices`, each name
/// once: everything the picker offers.
pub fn with_hosts(hosts: &[String], devices: Vec<Device>) -> Vec<Device> {
    let mut out: Vec<Device> = hosts
        .iter()
        .map(|h| Device {
            name: h.clone(),
            group: None,
            destination: h.clone(),
        })
        .collect();
    for d in devices {
        if !out.iter().any(|o| o.name == d.name) {
            out.push(d);
        }
    }
    out
}

/// How many groups the prompt names before it says how many more.
const NAMED_GROUPS: usize = 12;

/// What an agent is told about the fleet in the inventory at `path`: where
/// it is and how big, never the devices themselves, which would cost every
/// session hundreds of names it mostly does not need. `ConnectTimeout`
/// because devices in the field are often offline, and one that does not
/// answer must not hold up a loop over the rest.
pub fn system_prompt(path: &str, devices: &[Device]) -> String {
    let groups = groups(devices);
    let named = groups
        .iter()
        .take(NAMED_GROUPS)
        .map(|g| format!("`{g}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let grouped = match groups.len() {
        0 => String::new(),
        n if n > NAMED_GROUPS => format!(" in {n} groups: {named} and {} more", n - NAMED_GROUPS),
        1 => format!(" in the group {named}"),
        n => format!(" in {n} groups: {named}"),
    };
    format!(
        "This project manages a fleet of {}{grouped}, listed in the inventory \
         {path}. It is an Ansible style INI file: `[group]` headings, then one device \
         a line, its name first, with `ansible_host`, `ansible_user` and \
         `ansible_port` after it when the name alone does not reach it. Look a device \
         up there before reaching it, since only a line without `ansible_host` is \
         reached by its name. Across many devices, loop over them \
         from your shell with `-o ConnectTimeout=10` added, so one that is offline \
         does not hold up the rest, and say which ones failed.",
        match devices.len() {
            1 => "1 device".to_string(),
            n => format!("{n} devices"),
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, group: Option<&str>, destination: &str) -> Device {
        Device {
            name: name.into(),
            group: group.map(String::from),
            destination: destination.into(),
        }
    }

    #[test]
    fn the_config_names_the_inventory() {
        assert_eq!(
            inventory(r#"{"hosts":[],"inventory":" ops/devices.ini "}"#),
            Some("ops/devices.ini".into())
        );
        assert_eq!(inventory(r#"{"inventory":""}"#), None);
        assert_eq!(inventory(r#"{"inventory":3}"#), None);
        assert_eq!(inventory(r#"{"hosts":["a"]}"#), None);
        assert_eq!(inventory("not json"), None);
    }

    #[test]
    fn a_plain_list_is_an_inventory() {
        let devices = parse("sensor-001\nsensor-002\n\n# spare\nsensor-001\n");
        assert_eq!(
            devices,
            vec![
                device("sensor-001", None, "sensor-001"),
                device("sensor-002", None, "sensor-002"),
            ]
        );
    }

    #[test]
    fn groups_and_where_a_device_is_are_read() {
        let text = "\
\u{feff}[gateways]
gw-01 ansible_host=10.0.0.5 ansible_user=pi
gw-02 ansible_host=10.0.0.6 ansible_port=2222 # moved
gw-03 ansible_host=fe80::1 ansible_user='root' ansible_port=22

[gateways:vars]
ansible_user=admin
not-a-device

[sensors]
s-1 ansible_ssh_host=\"10.1.0.1\"
gw-01

[all:children]
gateways
";
        assert_eq!(
            parse(text),
            vec![
                device("gw-01", Some("gateways"), "pi@10.0.0.5"),
                device("gw-02", Some("gateways"), "ssh://10.0.0.6:2222"),
                device("gw-03", Some("gateways"), "ssh://root@[fe80::1]:22"),
                device("s-1", Some("sensors"), "10.1.0.1"),
            ]
        );
    }

    #[test]
    fn a_device_ssh_would_misread_is_left_out() {
        let text = "-oProxyCommand=calc\nok ansible_host=-x\nfine ansible_port=abc\n";
        assert_eq!(parse(text), vec![device("fine", None, "fine")]);
    }

    #[test]
    fn groups_come_in_order_each_once() {
        let devices = vec![
            device("a", Some("x"), "a"),
            device("b", None, "b"),
            device("c", Some("y"), "c"),
            device("d", Some("x"), "d"),
        ];
        assert_eq!(groups(&devices), vec!["x", "y"]);
    }

    #[test]
    fn finding_takes_every_word_and_puts_name_starts_first() {
        let devices = vec![
            device("north-gw-1", Some("gateways"), "10.0.0.1"),
            device("gw-2", Some("gateways"), "10.0.0.2"),
            device("sensor-9", Some("north"), "10.0.9.9"),
            device("gw-3", Some("spares"), "10.0.0.3"),
        ];
        let names = |q: &str, limit| {
            find(&devices, q, limit)
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("GW", 10), vec!["gw-2", "gw-3", "north-gw-1"]);
        assert_eq!(names("north", 10), vec!["north-gw-1", "sensor-9"]);
        assert_eq!(names("gw spares", 10), vec!["gw-3"]);
        assert_eq!(names("10.0.9", 10), vec!["sensor-9"]);
        assert_eq!(names("", 2), vec!["north-gw-1", "gw-2"]);
        assert!(names("nothing", 10).is_empty());
    }

    #[test]
    fn hosts_come_first_and_a_name_is_offered_once() {
        let hosts = vec!["myvps".to_string(), "gw-1".to_string()];
        let devices = vec![
            device("gw-1", Some("g"), "10.0.0.1"),
            device("gw-2", Some("g"), "10.0.0.2"),
        ];
        assert_eq!(
            with_hosts(&hosts, devices),
            vec![
                device("myvps", None, "myvps"),
                device("gw-1", None, "gw-1"),
                device("gw-2", Some("g"), "10.0.0.2"),
            ]
        );
    }

    #[test]
    fn the_prompt_says_where_and_how_many_not_which() {
        let devices = vec![
            device("gw-1", Some("gateways"), "10.0.0.1"),
            device("s-1", Some("sensors"), "10.0.1.1"),
            device("s-2", Some("sensors"), "10.0.1.2"),
        ];
        let p = system_prompt("C:/work/fleet/devices.ini", &devices);
        assert!(p.contains("a fleet of 3 devices in 2 groups: `gateways`, `sensors`,"));
        assert!(p.contains("inventory C:/work/fleet/devices.ini."));
        assert!(p.contains("-o ConnectTimeout=10"));
        assert!(!p.contains("gw-1"));
        assert!(!p.contains('\n'));
    }

    #[test]
    fn the_prompt_counts_the_groups_it_does_not_name() {
        let devices: Vec<Device> = (0..15)
            .map(|i| device(&format!("d{i}"), Some(&format!("g{i}")), "x"))
            .collect();
        let p = system_prompt("f", &devices);
        assert!(p.contains("in 15 groups: `g0`,"));
        assert!(p.contains("`g11` and 3 more,"));
        assert!(!p.contains("`g12`"));
        assert!(system_prompt("f", &[device("a", None, "a")]).contains("fleet of 1 device, listed"));
        assert!(system_prompt("f", &[device("a", Some("g"), "a")])
            .contains("1 device in the group `g`,"));
    }
}
