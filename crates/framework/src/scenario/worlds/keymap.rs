//! Keybindings — a chord you press, and the action it runs.
//!
//! Pure at every layer: parsing a chord, resolving an action name, and composing the
//! table from the shipped defaults plus plugin and config bindings.

use corelib::types::Chord;
use corelib::wire::Toml;

use super::super::world::{self, World};
use crate::gui::action::{keybinding_pairs, Action};
use crate::keymap::Keymap;

pub struct KeymapWorld {
    map: Keymap<Action>,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    // Start from what ships unless a scenario wants a blank table.
    let map = if world::flag(setup, "defaults").unwrap_or(true) {
        crate::gui::action::default_keymap()
    } else {
        Keymap::empty()
    };
    Ok(Box::new(KeymapWorld { map }))
}

impl World for KeymapWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // `bind` layers over whatever is there, exactly as a later source does.
        if let Some(lines) = world::list(step, "bind") {
            let doc = Toml::parse(&lines.join("\n")).map_err(|e| format!("bad keybinding TOML: {e}"))?;
            for (key, action) in keybinding_pairs(&doc) {
                match Action::from_name(&action) {
                    Some(a) => {
                        self.map.bind_str(&key, a);
                    }
                    // Dropped, not fatal — a config naming a removed action must not
                    // stop the terminal from starting.
                    None => continue,
                }
            }
            return Ok(());
        }

        if let Some(chord) = world::text(step, "expect_action") {
            let want = world::text(step, "action").ok_or("expect_action needs an `action`")?;
            let c = Chord::parse(&chord).ok_or_else(|| format!("{chord:?} is not a chord this build understands"))?;
            let got = self.map.lookup(&c).map(|a| format!("{a:?}")).unwrap_or_else(|| "(unbound)".into());
            return world::expect_eq(&got, &want, &format!("the action bound to {chord}"));
        }
        if let Some(chord) = world::text(step, "expect_unbound") {
            let c = Chord::parse(&chord).ok_or_else(|| format!("{chord:?} is not a chord this build understands"))?;
            if let Some(a) = self.map.lookup(&c) {
                return Err(format!("{chord} should be unbound, but it runs {a:?}"));
            }
            return Ok(());
        }
        if let Some(chord) = world::text(step, "expect_parses") {
            return match Chord::parse(&chord) {
                Some(_) => Ok(()),
                None => Err(format!("{chord:?} should be a valid chord, but it does not parse")),
            };
        }
        if let Some(chord) = world::text(step, "expect_rejected") {
            return match Chord::parse(&chord) {
                None => Ok(()),
                Some(c) => Err(format!("{chord:?} should be rejected, but it parsed as {c:?}")),
            };
        }
        if let Some(a) = world::text(step, "expect_same_chord") {
            let b = world::text(step, "as").ok_or("expect_same_chord needs an `as`")?;
            let (ca, cb) = (Chord::parse(&a), Chord::parse(&b));
            if ca.is_none() || ca != cb {
                return Err(format!("{a:?} and {b:?} should be the same chord — got {ca:?} and {cb:?}"));
            }
            return Ok(());
        }
        if let Some(name) = world::text(step, "expect_action_name") {
            let want = world::text(step, "action").ok_or("expect_action_name needs an `action`")?;
            let got = Action::from_name(&name).map(|a| format!("{a:?}")).unwrap_or_else(|| "(unknown)".into());
            return world::expect_eq(&got, &want, &format!("the action named {name:?}"));
        }

        Err(world::unknown_verb(step))
    }
}
