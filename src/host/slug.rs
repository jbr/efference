//! Session ids: a random adjective-noun pair, minted at creation
//! (docs/session-title.md).
//!
//! The id is the session directory's name, the `--name` a client resolves, and
//! the address a peer will message; it never changes. The lists hold common words
//! only: every word is unambiguous to spell and safe to say aloud.

#[rustfmt::skip]
const ADJECTIVES: &[&str] = &[
    "airy", "amber", "ample", "apt", "avid", "bold", "brave", "breezy", "bright",
    "brisk", "broad", "busy", "calm", "candid", "cheery", "chill", "civil", "clean", "clear",
    "cool", "coral", "cozy", "crisp", "daring", "deft", "dense", "direct", "dry", "dusty",
    "eager", "early", "easy", "even", "exact", "fair", "fancy", "fast", "fine", "firm",
    "fleet", "fond", "fresh", "frosty", "full", "gentle", "glad", "gold", "golden", "grand",
    "green", "happy", "hardy", "hasty", "hearty", "high", "honest", "humble",
    "icy", "ideal", "inner", "ivory", "jolly", "keen", "kind", "late", "lean", "light",
    "lively", "lofty", "long", "loud", "loyal", "lucid", "lucky", "lush", "major", "mellow",
    "merry", "mild", "modest", "narrow", "neat", "new", "nimble", "noble", "odd", "olive",
    "open", "pale", "plain", "plucky", "polite", "prime", "proud", "pure", "quick", "quiet",
    "rare", "ready", "regal", "rich", "ripe", "rosy", "round", "rustic", "sandy", "serene",
    "sharp", "shy", "silver", "simple", "sleek", "slow", "small", "smooth", "snug", "soft",
    "solid", "sound", "spare", "spry", "stable", "steady", "steep", "still", "stout",
    "sturdy", "sunny", "swift", "tall", "tame", "tawny", "teal", "tender", "thin", "tidy",
    "tough", "trim", "true", "upper", "vast", "vivid", "warm", "wide", "wild", "wise",
    "witty", "young", "zesty",
];

#[rustfmt::skip]
const NOUNS: &[&str] = &[
    "acorn", "anvil", "arch", "ash", "aspen", "atlas", "aurora", "badger", "basin", "bay",
    "beacon", "birch", "bison", "bluff", "boulder", "breeze", "bridge", "brook",
    "canoe", "canyon", "cave", "cedar", "cinder", "cliff", "cloud", "clover",
    "comet", "cove", "crane", "creek", "crest", "daisy", "dawn", "delta", "dove", "drift",
    "dune", "eagle", "elm", "ember", "falcon", "fawn", "fern", "field", "fig", "finch",
    "fjord", "flax", "flint", "forest", "fox", "frost", "garden", "glade", "glen", "gorge",
    "grove", "gull", "harbor", "hawk", "hazel", "heath", "heron", "hill", "hollow", "inlet",
    "isle", "ivy", "jade", "jay", "juniper", "kestrel", "kite", "lagoon", "lake", "lantern",
    "larch", "lark", "lilac", "linnet", "loon", "lynx", "maple", "marsh", "meadow", "mesa",
    "mist", "mole", "moon", "moss", "moth", "oak", "orchard", "orchid", "osprey", "otter",
    "owl", "pebble", "pine", "plover", "pond", "poplar", "prairie", "prism", "puffin",
    "quail", "quartz", "raven", "reef", "ridge", "river", "robin", "rowan", "sable",
    "sage", "sand", "seal", "shoal", "shore", "slate", "sparrow", "spring", "spruce",
    "stone", "storm", "stream", "summit", "swan", "tarn", "thistle", "thorn", "thrush",
    "tide", "trail", "trout", "tulip", "tundra", "valley", "violet", "walnut", "wave",
    "wheat", "willow", "wolf", "woods", "wren",
];

/// Draws before the minter gives up on finding a free pair and suffixes the last
/// one instead. Two lists of about 150 words give some twenty thousand pairs; a host with so many
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
        assert!(crate::record::session_dir::plain_id(&id));
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
