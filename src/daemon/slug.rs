//! Session ids: a random adjective-noun pair, minted at creation
//! (docs/session-title.md).
//!
//! The id is the session directory's name, the `--name` a client resolves, and
//! the address a peer will message; it never changes. A word pair is what makes
//! it typeable from memory — the timestamp it replaced had to be copied out of
//! an `ls`. The lists are short on purpose: every word is common, unambiguous
//! to spell, and safe to say aloud.

#[rustfmt::skip]
const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brisk", "calm", "clear", "cool", "crisp", "deft", "dry", "eager",
    "fair", "fond", "glad", "green", "hardy", "keen", "kind", "late", "lean", "light",
    "lucid", "mild", "neat", "new", "odd", "pale", "plain", "proud", "quick", "quiet",
    "rare", "ripe", "rosy", "round", "sharp", "shy", "sleek", "slow", "small", "snug",
    "soft", "spare", "steep", "still", "swift", "tall", "tidy", "true", "vivid", "warm",
    "wide", "wild", "wise", "young",
];

#[rustfmt::skip]
const NOUNS: &[&str] = &[
    "anvil", "arch", "badger", "bay", "beacon", "birch", "brook", "cedar", "cliff", "comet",
    "crane", "delta", "dune", "ember", "falcon", "fern", "finch", "fjord", "glade", "harbor",
    "hawk", "heron", "inlet", "isle", "kestrel", "lark", "lantern", "linnet", "lynx", "marsh",
    "meadow", "mesa", "moss", "otter", "pebble", "pine", "plover", "prism", "quail", "raven",
    "reef", "ridge", "river", "robin", "sable", "shoal", "sparrow", "spruce", "summit", "tarn",
    "thrush", "walnut", "willow", "wren",
];

/// Draws before the minter gives up on finding a free pair and suffixes the last
/// one instead. Two short lists give a few thousand pairs; a project with so many
/// session directories that this many draws all collide is the case the suffix
/// exists for, not the ordinary one.
const DRAWS: usize = 16;

fn draw() -> String {
    format!(
        "{}-{}",
        ADJECTIVES[rand::random_range(0..ADJECTIVES.len())],
        NOUNS[rand::random_range(0..NOUNS.len())]
    )
}

/// Mint an id no current session holds: a random `adjective-noun`, redrawn while
/// `taken` says the pair is in use. A random name carries nothing worth keeping
/// through a collision, so another draw beats a numbered suffix; only after
/// [`DRAWS`] taken pairs does the last one get the first free `-2`, `-3`, … so the
/// call still terminates when the space is nearly full.
pub fn mint(taken: impl Fn(&str) -> bool) -> String {
    let mut pair = draw();
    for _ in 1..DRAWS {
        if !taken(&pair) {
            return pair;
        }
        pair = draw();
    }
    if !taken(&pair) {
        return pair;
    }
    (2..)
        .map(|n| format!("{pair}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded suffix range finds a free id")
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_minted_id_is_a_plain_word_pair() {
        let id = mint(|_| false);
        let (adjective, noun) = id.split_once('-').expect("adjective-noun");
        assert!(ADJECTIVES.contains(&adjective));
        assert!(NOUNS.contains(&noun));
        assert!(super::super::tcp::plain_id(&id));
    }

    #[test]
    fn a_collision_redraws_rather_than_suffixing() {
        // Exactly one pair is taken; a redraw finds another bare pair.
        let first = mint(|_| false);
        let id = mint(|candidate| candidate == first);
        assert_ne!(id, first);
        assert_eq!(id.matches('-').count(), 1, "{id}");
    }

    #[test]
    fn a_full_space_falls_back_to_a_suffix() {
        // Every bare pair and every `-2` is taken; the draws run out and the last
        // pair gets `-3`, so the minter still terminates.
        let id = mint(|candidate| candidate.matches('-').count() == 1 || candidate.ends_with("-2"));
        assert!(id.ends_with("-3"), "{id}");
    }
}
