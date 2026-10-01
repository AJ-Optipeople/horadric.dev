//! Checks the Rich Presence client against the Discord running here: sets
//! an activity, changes it twice inside the gap, then clears it.
//!
//! `cargo run -p horadric-ui --example discord`, with `HORADRIC_DEBUG=1` to
//! hear what Discord answers. Look at your profile while it runs.

#[cfg(windows)]
fn main() {
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use horadric_core::discord::Activity;
    use horadric_ui::discord::Discord;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let activity = |details: &str| Activity {
        details: details.into(),
        state: Some("Checking the pipe".into()),
        large_image: Some("horadric".into()),
        large_text: Some("Horadric".into()),
        small_image: Some("working".into()),
        small_text: Some("Working".into()),
        start: Some(now - 60),
    };

    let discord = Discord::start();
    println!("set: 1 agent working");
    discord.set(Some(activity("1 agent working")));
    sleep(Duration::from_secs(1));
    println!("set twice inside the gap, only the last should show");
    discord.set(Some(activity("2 agents working")));
    discord.set(Some(activity("3 agents working, 1 waits for you")));
    sleep(Duration::from_secs(15));
    println!("stop: clears");
    discord.stop();
    println!("done");
}

#[cfg(not(windows))]
fn main() {}
