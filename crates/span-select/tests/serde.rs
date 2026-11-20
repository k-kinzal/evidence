#![cfg(feature = "serde")]
#![allow(clippy::unwrap_used)]

use span_select::Selector;

#[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq)]
struct Wrapper {
    select: Selector,
}

#[test]
fn toml_inline_tables() {
    let cases = [
        (
            "select = { lines = \"40-58\" }",
            Selector::Lines("40-58".parse().unwrap()),
        ),
        (
            "select = { lines = 7 }",
            Selector::Lines("7".parse().unwrap()),
        ),
        (
            "select = { heading = \"## Impact\" }",
            Selector::Heading("## Impact".into()),
        ),
        (
            "select = { json_pointer = \"/a/0\" }",
            Selector::JsonPointer("/a/0".into()),
        ),
        (
            "select = { marker = \"intro\" }",
            Selector::Marker("intro".into()),
        ),
        ("select = { whole = true }", Selector::Whole(true)),
    ];
    for (input, want) in cases {
        let got: Wrapper = toml::from_str(input).unwrap();
        assert_eq!(got.select, want, "{input}");
        let back = toml::to_string(&got).unwrap();
        let again: Wrapper = toml::from_str(&back).unwrap();
        assert_eq!(again.select, want);
    }
    assert!(toml::from_str::<Wrapper>("select = { lines = \"0-1\" }").is_err());
    assert!(toml::from_str::<Wrapper>("select = { nope = 1 }").is_err());
}
