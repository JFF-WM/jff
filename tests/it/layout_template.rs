//! Tests for `jff_wm::primitives::layout_template`.

use jff_wm::desk::layout_store::{format_layouts, parse_layouts, Layouts};
use jff_wm::primitives::layout_template::Template;
use jff_wm::primitives::tile_tree::{Axis, Node, TileTree};

const A: u32 = 1;
const B: u32 = 2;
const C: u32 = 3;
const D: u32 = 4;

fn tagged(tag: &str) -> Option<String> {
    Some(tag.to_owned())
}

/// `_ | @browser`, the left one 70% wide.
fn terminal_and_browser() -> Template {
    Template::parse("h0.7(_,@browser)").expect("parses")
}

fn leaves(node: &Node<u32>) -> Vec<u32> {
    let mut tree_leaves = Vec::new();
    fn walk(node: &Node<u32>, out: &mut Vec<u32>) {
        match node {
            Node::Leaf(w) => out.push(*w),
            Node::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    walk(node, &mut tree_leaves);
    tree_leaves
}

#[test]
fn a_tree_is_saved_as_slots_with_the_tags_of_its_windows() {
    let mut tree = TileTree::new(A);
    tree.split(A, Axis::Horizontal, B).unwrap();
    tree.split(B, Axis::Vertical, C).unwrap();
    let tags = |w: u32| (w == C).then(|| "browser".to_owned());

    let template = Template::from_tree(&tree, tags).expect("a tree with windows");

    assert_eq!(template.slots(), vec![None, None, Some("browser")]);
    assert_eq!(template.to_text(), "h0.5(_,v0.5(_,@browser))");
}

#[test]
fn an_empty_tag_is_a_free_slot() {
    let tree = TileTree::new(A);
    let template = Template::from_tree(&tree, |_| tagged("")).unwrap();
    assert_eq!(template, Template::Slot(None));
}

#[test]
fn the_ratios_are_kept() {
    let mut tree = TileTree::new(A);
    tree.split(A, Axis::Horizontal, B).unwrap();
    tree.resize(
        A,
        jff_wm::primitives::tile_tree::Edge::Right,
        70,
        (0, 0, 100, 50),
        0,
        1,
    )
    .unwrap();
    let template = Template::from_tree(&tree, |_| None).unwrap();
    assert_eq!(template.to_text(), "h0.7(_,_)");
}

#[test]
fn two_free_windows_fill_two_free_slots_in_the_order_given() {
    let template = Template::parse("h0.7(_,_)").unwrap();
    let fill = template.fill(&[(A, None), (B, None)]).unwrap();
    assert_eq!(leaves(&fill.root), vec![A, B]);
    assert!(fill.extra.is_empty());
    assert_eq!(fill.empty, 0);
    // Whichever comes first is on the left: it does not matter which, they are clones.
    let fill = template.fill(&[(B, None), (A, None)]).unwrap();
    assert_eq!(leaves(&fill.root), vec![B, A]);
}

#[test]
fn a_tagged_window_goes_to_the_slot_with_its_tag_whatever_its_order() {
    let template = terminal_and_browser();
    let fill = template.fill(&[(A, tagged("browser")), (B, None)]).unwrap();
    // The browser was first, but its slot is the second one.
    assert_eq!(leaves(&fill.root), vec![B, A]);
    match fill.root {
        Node::Split { axis, ratio, .. } => {
            assert_eq!(axis, Axis::Horizontal);
            assert!((ratio - 0.7).abs() < 1e-9);
        }
        other => panic!("not a split: {other:?}"),
    }
}

#[test]
fn a_tag_decides_before_the_free_slots_take_what_is_left() {
    // The free slot is first in the tree, and window A is first in line, but A has the tag.
    let template = terminal_and_browser();
    let fill = template
        .fill(&[(A, tagged("browser")), (B, None), (C, None)])
        .unwrap();
    assert_eq!(leaves(&fill.root), vec![B, A]);
    assert_eq!(fill.extra, vec![C]);
}

#[test]
fn two_windows_with_one_tag_share_one_slot_and_the_other_goes_free() {
    let template = terminal_and_browser();
    let fill = template
        .fill(&[(A, tagged("browser")), (B, tagged("browser"))])
        .unwrap();
    // A gets the browser slot, B has no tag that fits and takes the free one.
    assert_eq!(leaves(&fill.root), vec![B, A]);
    assert!(fill.extra.is_empty());
}

#[test]
fn a_slot_nobody_fills_closes_up_and_the_other_half_takes_its_room() {
    let template = Template::parse("h0.7(_,v0.4(@browser,_))").unwrap();
    // One window only: it takes the free slot, nothing else is left.
    let fill = template.fill(&[(A, None)]).unwrap();
    assert_eq!(fill.root, Node::Leaf(A));
    assert_eq!(fill.empty, 2);
    // Two windows: the browser slot is empty, the split around it goes, ratios of the rest stay.
    let fill = template.fill(&[(A, None), (B, None)]).unwrap();
    match &fill.root {
        Node::Split {
            axis, ratio, first, ..
        } => {
            assert_eq!(*axis, Axis::Horizontal);
            assert!((ratio - 0.7).abs() < 1e-9);
            assert_eq!(**first, Node::Leaf(A));
        }
        other => panic!("not a split: {other:?}"),
    }
    assert_eq!(fill.empty, 1);
}

#[test]
fn windows_without_a_matching_tag_still_fill_a_tagged_slot() {
    // Nothing is tagged: the shape is kept, in order, so no window is lost and none is stuck.
    let template = terminal_and_browser();
    let fill = template.fill(&[(A, None), (B, None)]).unwrap();
    assert_eq!(leaves(&fill.root), vec![A, B]);
    assert_eq!(fill.empty, 0);
}

#[test]
fn a_tagged_slot_nobody_has_is_filled_last_so_a_free_slot_goes_first() {
    let template = Template::parse("h0.5(@browser,_)").unwrap();
    let fill = template.fill(&[(A, None), (B, None)]).unwrap();
    // The free slot is second in the tree but it is taken before the tagged one.
    assert_eq!(leaves(&fill.root), vec![B, A]);
}

#[test]
fn there_is_nothing_to_fill_with_no_windows() {
    assert!(terminal_and_browser().fill::<u32>(&[]).is_none());
}

#[test]
fn every_window_is_in_the_result_once() {
    let template = Template::parse("h0.5(@a,v0.5(_,@b))").unwrap();
    let windows = [
        (A, tagged("b")),
        (B, None),
        (C, tagged("zzz")),
        (D, tagged("a")),
    ];
    let fill = template.fill(&windows).unwrap();
    let mut all = leaves(&fill.root);
    all.extend(&fill.extra);
    all.sort_unstable();
    assert_eq!(all, vec![A, B, C, D]);
}

#[test]
fn the_text_form_round_trips() {
    for text in [
        "_",
        "@browser",
        "h0.7(_,_)",
        "v0.3333(@a,h1(_,@b))",
        "h0(_,_)",
        "h0.5(@a%20b,@%C3%A9)",
    ] {
        let template = Template::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(template.to_text(), text);
    }
}

#[test]
fn a_tag_with_odd_characters_is_escaped_and_comes_back() {
    let template = Template::Slot(tagged("my app, v2 (é)"));
    let text = template.to_text();
    assert!(!text.contains([' ', ',', '(', ')', '\t']));
    assert_eq!(Template::parse(&text), Ok(template));
}

#[test]
fn text_that_is_not_a_layout_is_refused() {
    for text in [
        "",
        "x",
        "h(_,_)",
        "h0.5(_)",
        "h0.5(_,_",
        "h0.5(_,_)x",
        "h1.5(_,_)",
        "h0.5(@,_)",
        "h0.5(@a%2,_)",
        "h0.5(@a%zz,_)",
        "h..(_,_)",
    ] {
        assert!(Template::parse(text).is_err(), "{text:?} was accepted");
    }
}

#[test]
fn a_very_deep_layout_is_refused_and_does_not_overflow_the_stack() {
    let mut text = String::new();
    for _ in 0..10_000 {
        text.push_str("h0.5(");
    }
    assert!(Template::parse(&text).is_err());
}

#[test]
fn layouts_are_written_sorted_and_read_back() {
    let mut layouts = Layouts::new();
    layouts.insert("Work".into(), terminal_and_browser());
    layouts.insert("A\tb\nc".into(), Template::Slot(None));
    let text = format_layouts(&layouts);
    assert_eq!(text.lines().count(), 2);
    assert_eq!(parse_layouts(&text), layouts);
    assert_eq!(format_layouts(&parse_layouts(&text)), text);
}

#[test]
fn a_bad_line_of_the_file_is_skipped_and_the_rest_is_kept() {
    let text = "Good\th0.5(_,_)\nno tab here\nBad\th0.5(_\n\th0.5(_,_)\nAlso good\t@x\n";
    let layouts = parse_layouts(text);
    let names: Vec<&str> = layouts.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["Also good", "Good"]);
}
