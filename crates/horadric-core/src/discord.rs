//! Rich Presence: what Horadric shows on the human's Discord profile, and
//! the frames that carry it down Discord's local pipe.
//!
//! Everything here is pure. The pipe itself, the thread that owns it and
//! the pacing live in the UI crate (`horadric_ui::discord`), which hands
//! these frames to Discord and reads its answers back through [`decode`].

/// One activity, the "Playing Horadric" card on a Discord profile.
///
/// This is the type both halves of the feature meet on: the presence quest
/// builds one from the sessions and the setting, and the pipe client sends
/// it. Build on this one rather than making a second.
///
/// Images are Rich Presence keys, an art asset's name in the Discord
/// application or an `https` URL, whichever the art ends up as.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// The first line under the name, "3 agents working, 1 waits for you".
    pub details: String,
    /// The second line, the project, only when names are allowed.
    pub state: Option<String>,
    /// Horadric's own mark.
    pub large_image: Option<String>,
    /// The tooltip on the large image.
    pub large_text: Option<String>,
    /// The state of the most urgent session.
    pub small_image: Option<String>,
    /// The tooltip on the small image.
    pub small_text: Option<String>,
    /// Unix seconds when the current run of work began, so Discord counts
    /// up from it. Kept while any session works, not reset every turn.
    pub start: Option<u64>,
}
