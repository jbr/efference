use super::*;

#[derive(Deserialize)]
struct Fixture {
    tree: Vec<Node>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    why: String,
    query: String,
    chips: Vec<String>,
    role: String,
    expect: Vec<String>,
}

#[test]
fn the_shared_fixture_ranks_as_expected() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../match_cases.json")).expect("the fixture parses");
    for case in fixture.cases {
        let chips: Vec<Chip> = case
            .chips
            .iter()
            .enumerate()
            .map(|(depth, label)| match depth {
                0 => Chip::Connection(label.clone()),
                _ => Chip::Group(label.clone()),
            })
            .collect();
        let got: Vec<String> = rank(&fixture.tree, &chips, &case.query, case.role == "main")
            .into_iter()
            .map(|index| fixture.tree[index].id())
            .collect();
        assert_eq!(got, case.expect, "{} (query {:?})", case.why, case.query);
    }
}
