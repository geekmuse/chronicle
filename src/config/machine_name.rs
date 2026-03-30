use rand::seq::SliceRandom as _;

/// Curated adjective word list for machine name generation.
static ADJECTIVES: &[&str] = &[
    "bold",
    "brave",
    "bright",
    "calm",
    "cheerful",
    "clever",
    "crimson",
    "curious",
    "dazzling",
    "eager",
    "elegant",
    "energetic",
    "fierce",
    "gentle",
    "golden",
    "graceful",
    "happy",
    "jolly",
    "kind",
    "lively",
    "lucky",
    "mighty",
    "nimble",
    "peaceful",
    "playful",
    "polished",
    "proud",
    "quiet",
    "radiant",
    "rustic",
    "serene",
    "silent",
    "silver",
    "sleek",
    "smart",
    "snappy",
    "sparkling",
    "spirited",
    "stellar",
    "strong",
    "sunny",
    "swift",
    "tidy",
    "tiny",
    "vibrant",
    "wise",
    "witty",
];

/// Curated animal word list for machine name generation.
static ANIMALS: &[&str] = &[
    "albatross",
    "alpaca",
    "anaconda",
    "antelope",
    "armadillo",
    "axolotl",
    "badger",
    "barracuda",
    "basilisk",
    "bear",
    "beaver",
    "bison",
    "bobcat",
    "capybara",
    "chameleon",
    "cheetah",
    "chinchilla",
    "chipmunk",
    "condor",
    "cougar",
    "coyote",
    "crane",
    "crocodile",
    "dingo",
    "dolphin",
    "dormouse",
    "dragonfly",
    "eagle",
    "echidna",
    "emu",
    "falcon",
    "ferret",
    "flamingo",
    "fox",
    "gazelle",
    "gecko",
    "giraffe",
    "gnu",
    "gorilla",
    "heron",
    "iguana",
    "jackal",
    "jaguar",
    "jellyfish",
    "kangaroo",
    "lemur",
    "leopard",
    "lynx",
    "macaw",
    "marmot",
    "meerkat",
    "mongoose",
    "monitor",
    "moose",
    "narwhal",
    "numbat",
    "ocelot",
    "octopus",
    "okapi",
    "orca",
    "osprey",
    "otter",
    "pangolin",
    "parrot",
    "pelican",
    "penguin",
    "platypus",
    "puffin",
    "quokka",
    "quoll",
    "raccoon",
    "raven",
    "salamander",
    "seahorse",
    "serval",
    "skunk",
    "sloth",
    "sparrow",
    "stingray",
    "stoat",
    "tapir",
    "tardigrade",
    "toucan",
    "viper",
    "wallaby",
    "weasel",
    "wolverine",
    "wombat",
    "woodpecker",
    "yak",
    "zebra",
];

/// Generate a random `{adjective}-{animal}` machine name.
///
/// Examples: `cheerful-chinchilla`, `bold-barracuda`, `gentle-gecko`.
///
/// The word lists contain 47 adjectives and 90 animals, yielding over 4,200
/// possible combinations. Names are not guaranteed to be unique across calls,
/// but the collision probability is low enough for per-machine identity.
#[must_use]
pub fn generate() -> String {
    let mut rng = rand::thread_rng();
    let adj = ADJECTIVES.choose(&mut rng).copied().unwrap_or("bold");
    let animal = ANIMALS.choose(&mut rng).copied().unwrap_or("barracuda");
    format!("{adj}-{animal}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_has_adjective_animal_format() {
        let name = generate();
        assert!(name.contains('-'), "name should contain a hyphen: {name}");
        let (adj, animal) = name.split_once('-').expect("hyphen present");
        assert!(!adj.is_empty(), "adjective should not be empty");
        assert!(!animal.is_empty(), "animal should not be empty");
    }

    #[test]
    fn generate_uses_words_from_word_lists() {
        for _ in 0..50 {
            let name = generate();
            let (adj, animal) = name.split_once('-').expect("hyphen present");
            assert!(
                ADJECTIVES.contains(&adj),
                "unrecognised adjective: {adj} (from name: {name})"
            );
            assert!(
                ANIMALS.contains(&animal),
                "unrecognised animal: {animal} (from name: {name})"
            );
        }
    }

    #[test]
    fn generate_produces_varied_names() {
        // With 4,000+ combinations, 20 calls should rarely produce the same name.
        let names: HashSet<String> = (0..20).map(|_| generate()).collect();
        assert!(
            names.len() > 1,
            "name generator produced identical results for 20 calls"
        );
    }

    #[test]
    fn word_lists_are_non_empty() {
        assert!(!ADJECTIVES.is_empty(), "adjective list must not be empty");
        assert!(!ANIMALS.is_empty(), "animal list must not be empty");
    }
}
