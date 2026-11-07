//! Tests for the launch counts kept by `jff_wm::feature::app_index`.

use std::collections::HashMap;

use jff_wm::feature::app_index::{format_launches, parse_launches};

#[test]
fn counts_round_trip() {
    let mut launches = HashMap::new();
    launches.insert("firefox".to_owned(), 12);
    launches.insert("org.gnome.Nautilus".to_owned(), 3);
    let text = format_launches(&launches);
    assert_eq!(text, "12 firefox\n3 org.gnome.Nautilus\n");
    assert_eq!(parse_launches(&text), launches);
}

#[test]
fn bad_lines_are_skipped() {
    let launches = parse_launches("5 good\nnot a number\n\n7\nx bad\n2 also good\n");
    assert_eq!(launches.len(), 2);
    assert_eq!(launches["good"], 5);
    assert_eq!(launches["also good"], 2);
}
