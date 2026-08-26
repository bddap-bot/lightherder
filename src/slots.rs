//! The eight preset slots: the panel written to disk and read back.
//!
//! A slot is a config file and nothing else — the same TOML the command line
//! takes, through the same [`config::read`] — so a performance saved to slot
//! 3 can be opened in an editor, kept in a repository, or handed straight
//! back to the instrument as the graph it starts on. There is no second
//! format for "a saved state", because a saved state is a graph.

use std::path::{Path, PathBuf};

use crate::config;
use crate::params::Params;

/// Slots on the front panel, one per function key.
pub const SLOTS: usize = 8;

/// Where the slots live when nobody says otherwise: `$XDG_CONFIG_HOME` or
/// the `~/.config` the spec falls back to, with a relative `XDG_CONFIG_HOME`
/// ignored the way the spec says to.
pub fn default_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("lightherder")
}

/// Numbered from one, the way the keys are labelled.
pub fn path(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("slot-{}.toml", slot + 1))
}

/// Write the live graph to a slot, making the directory if this is the first
/// one. Returns where it went, so the terminal can say.
pub fn store(dir: &Path, slot: usize, params: &Params) -> Result<PathBuf, String> {
    let path = path(dir, slot);
    let shown = path.display();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = toml::to_string(params).map_err(|e| format!("{shown}: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("{shown}: {e}"))?;
    Ok(path)
}

/// Read a slot back, validated — an edited slot file is as untrusted as any
/// other config, and a slot the instrument never wrote is a legal thing to
/// put there.
pub fn recall(dir: &Path, slot: usize) -> Result<Params, String> {
    config::read(&path(dir, slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own: the suite runs in one process, so the
    /// pid alone would have every test in this file sharing a directory.
    fn scratch(what: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-slots-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_slot_holds_the_whole_panel() {
        let dir = scratch("round-trip");
        // Between them these carry every part of the format, and `kinetic`
        // is the one that carries the automation — the part a preset saves
        // that a preset could not save before this stage.
        for (slot, params) in config::PRESETS.iter().enumerate() {
            let params = params.1();
            store(&dir, slot % SLOTS, &params).unwrap();
            assert_eq!(recall(&dir, slot % SLOTS).unwrap(), params);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stored_slot_is_a_config_file_like_any_other() {
        // The whole reason a slot is TOML and not a private format: what the
        // instrument writes, the command line can open.
        let dir = scratch("command-line");
        let mut params = config::kinetic();
        params.monitors[0].colour.hue = 0.7;
        let written = store(&dir, 4, &params).unwrap();
        assert!(written.ends_with("slot-5.toml"), "{}", written.display());
        assert_eq!(config::load(written.to_str().unwrap()).unwrap(), params);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_slot_says_which_file_it_wanted() {
        let dir = scratch("empty");
        let why = recall(&dir, 0).unwrap_err();
        assert!(why.contains("slot-1.toml"), "unhelpful: {why}");
    }

    #[test]
    fn a_slot_holding_a_graph_the_instrument_would_refuse_is_refused() {
        // A slot file is as editable as any other, so recall validates. The
        // door validate is the only one for stays the only one.
        let dir = scratch("poisoned");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            path(&dir, 2),
            "cameras = [{ look = [1.0], gain = [nan, 1.0, 1.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n",
        )
        .unwrap();
        assert!(recall(&dir, 2).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
