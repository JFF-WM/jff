//! Tests for `jff_wm::primitives::tile_tree`.

use jff_wm::primitives::layout::bounds;
use jff_wm::primitives::tile_tree::{
    Axis, Edge, Node, Rect, Removal, SplitError, TileTree, DEFAULT_RATIO,
};

const A: u32 = 1;
const B: u32 = 2;
const C: u32 = 3;
const D: u32 = 4;
const NEW: u32 = 99;

const AREA: Rect = (0, 0, 100, 50);

/// A tree that starts with `first`, then splits `(leaf, axis, new window)` one after the other.
fn tree_of(first: u32, splits: &[(u32, Axis, u32)]) -> TileTree<u32> {
    let mut t = TileTree::new(first);
    for &(at, axis, new) in splits {
        assert_eq!(t.split(at, axis, new), Ok(()));
    }
    t
}

/// `A | B`, `B` on the right.
fn pair() -> TileTree<u32> {
    tree_of(A, &[(A, Axis::Horizontal, B)])
}

/// `A | (B / C)`: `A` on the left, `B` above `C` on the right.
fn three() -> TileTree<u32> {
    tree_of(A, &[(A, Axis::Horizontal, B), (B, Axis::Vertical, C)])
}

fn rect_of(t: &TileTree<u32>, area: Rect, gap: i32, window: u32) -> Rect {
    t.layout(area, gap)
        .into_iter()
        .find(|(w, _)| *w == window)
        .map(|(_, r)| r)
        .expect("the window has a rectangle")
}

fn root_ratio(t: &TileTree<u32>) -> f64 {
    match t.root() {
        Some(Node::Split { ratio, .. }) => *ratio,
        other => panic!("the root is not a split: {other:?}"),
    }
}

// A small deterministic generator, so the tests need no extra crate and fail the same way twice.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as u32) % n
    }
}

/// A tree of `leaves` windows (`0..leaves`), split at random and with a few dividers dragged.
fn random_tree(rng: &mut Lcg, leaves: u32, area: Rect, gap: i32) -> TileTree<u32> {
    let mut t = TileTree::new(0);
    for id in 1..leaves {
        let ids = t.leaves();
        let at = ids[rng.below(ids.len() as u32) as usize];
        let axis = if rng.below(2) == 0 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        assert_eq!(t.split(at, axis, id), Ok(()));
    }
    for _ in 0..rng.below(6) {
        let ids = t.leaves();
        let leaf = ids[rng.below(ids.len() as u32) as usize];
        let edge = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom][rng.below(4) as usize];
        let _ = t.resize(leaf, edge, rng.below(400) as i32 - 40, area, gap, 10);
    }
    t
}

fn ids_under(node: &Node<u32>, out: &mut Vec<u32>) {
    match node {
        Node::Leaf(w) => out.push(*w),
        Node::Split { first, second, .. } => {
            ids_under(first, out);
            ids_under(second, out);
        }
    }
}

/// The smallest rectangle around the leaves under `node`.
fn bounds_under(node: &Node<u32>, rects: &[(u32, Rect)]) -> Rect {
    let mut ids = Vec::new();
    ids_under(node, &mut ids);
    let of: Vec<Rect> = rects
        .iter()
        .filter(|(id, _)| ids.contains(id))
        .map(|(_, r)| *r)
        .collect();
    bounds(&of)
}

/// The strip between the two halves of every split, worked out from where the leaves ended up
/// and not from the tree's own arithmetic. Each must be `gap` px thick.
fn strips(node: &Node<u32>, rects: &[(u32, Rect)], gap: i32, out: &mut Vec<Rect>) {
    if let Node::Split {
        axis,
        first,
        second,
        ..
    } = node
    {
        let a = bounds_under(first, rects);
        let b = bounds_under(second, rects);
        let strip = match axis {
            Axis::Horizontal => {
                assert_eq!((a.1, a.3), (b.1, b.3), "the halves differ in height");
                let x = a.0 + a.2;
                (x, a.1, b.0 - x, a.3)
            }
            Axis::Vertical => {
                assert_eq!((a.0, a.2), (b.0, b.2), "the halves differ in width");
                let y = a.1 + a.3;
                (a.0, y, a.2, b.1 - y)
            }
        };
        assert_eq!(
            match axis {
                Axis::Horizontal => strip.2,
                Axis::Vertical => strip.3,
            },
            gap,
            "the strip is not one gap thick"
        );
        out.push(strip);
        strips(first, rects, gap, out);
        strips(second, rects, gap, out);
    }
}

fn paint(grid: &mut [u8], area: Rect, r: Rect) {
    for y in r.1..r.1 + r.3 {
        for x in r.0..r.0 + r.2 {
            assert!(
                x >= area.0 && x < area.0 + area.2 && y >= area.1 && y < area.1 + area.3,
                "{r:?} is outside {area:?}"
            );
            grid[((y - area.1) * area.2 + (x - area.0)) as usize] += 1;
        }
    }
}

/// Checks that every pixel of `area` is in exactly one leaf or one gap strip, and none outside.
/// Returns false, having checked only that the leaves stay inside and do not overlap, when a
/// leaf has no size: its half was squeezed out, and the strips can't be told from the leaves.
fn check_tiling(t: &TileTree<u32>, area: Rect, gap: i32) -> bool {
    let rects = t.layout(area, gap);
    assert_eq!(rects.len(), t.len());

    let mut grid = vec![0u8; (area.2 * area.3) as usize];
    for (_, r) in &rects {
        paint(&mut grid, area, *r);
    }
    assert!(grid.iter().all(|&n| n <= 1), "two leaves overlap");
    if rects.iter().any(|(_, r)| r.2 == 0 || r.3 == 0) {
        return false;
    }

    let mut gaps = Vec::new();
    if let Some(root) = t.root() {
        strips(root, &rects, gap, &mut gaps);
    }
    for g in gaps {
        paint(&mut grid, area, g);
    }
    assert!(
        grid.iter().all(|&n| n == 1),
        "a pixel is in no leaf and no gap, or in a gap and a leaf"
    );
    true
}

#[test]
fn a_single_leaf_fills_the_area() {
    let t = TileTree::new(A);
    assert_eq!(t.layout((10, 20, 300, 200), 8), [(A, (10, 20, 300, 200))]);
    assert_eq!(t.leaves(), [A]);
    assert_eq!(t.len(), 1);
    assert!(!t.is_empty());
}

#[test]
fn split_puts_the_new_window_in_the_second_half() {
    assert_eq!(
        pair().layout(AREA, 0),
        [(A, (0, 0, 50, 50)), (B, (50, 0, 50, 50))]
    );
    let t = tree_of(A, &[(A, Axis::Vertical, B)]);
    assert_eq!(
        t.layout(AREA, 0),
        [(A, (0, 0, 100, 25)), (B, (0, 25, 100, 25))]
    );
}

#[test]
fn a_new_split_starts_at_the_default_ratio() {
    assert_eq!(root_ratio(&pair()), DEFAULT_RATIO);
}

#[test]
fn the_gap_goes_between_siblings_and_nowhere_else() {
    assert_eq!(
        pair().layout(AREA, 10),
        [(A, (0, 0, 45, 50)), (B, (55, 0, 45, 50))]
    );
    let t = tree_of(A, &[(A, Axis::Vertical, B)]);
    assert_eq!(
        t.layout(AREA, 10),
        [(A, (0, 0, 100, 20)), (B, (0, 30, 100, 20))]
    );
}

#[test]
fn an_odd_length_gives_the_spare_pixel_to_the_first_half() {
    assert_eq!(
        pair().layout((0, 0, 101, 50), 0),
        [(A, (0, 0, 51, 50)), (B, (51, 0, 50, 50))]
    );
}

#[test]
fn a_nested_split_tiles_inside_its_leaf() {
    assert_eq!(
        three().layout(AREA, 0),
        [
            (A, (0, 0, 50, 50)),
            (B, (50, 0, 50, 25)),
            (C, (50, 25, 50, 25))
        ]
    );
}

#[test]
fn layout_is_in_tree_order() {
    let t = tree_of(
        A,
        &[
            (A, Axis::Horizontal, B),
            (A, Axis::Vertical, C),
            (B, Axis::Vertical, D),
        ],
    );
    let ids: Vec<u32> = t.layout(AREA, 4).iter().map(|(w, _)| *w).collect();
    assert_eq!(ids, t.leaves());
    assert_eq!(ids, [A, C, B, D]);
}

#[test]
fn leaves_exactly_tile_the_area_minus_the_gaps() {
    let area = (37, -20, 320, 240);
    let mut rng = Lcg(7);
    let mut checked = 0;
    for _ in 0..200 {
        let gap = rng.below(7) as i32;
        let leaves = 1 + rng.below(6);
        let t = random_tree(&mut rng, leaves, area, gap);
        if check_tiling(&t, area, gap) {
            checked += 1;
        }
    }
    // Most trees must have gone through the exact check, or the test proves nothing.
    assert!(checked >= 100, "only {checked} of 200 trees were checked");
}

#[test]
fn the_tiling_is_exact_with_an_odd_size_and_a_gap() {
    let t = tree_of(
        A,
        &[
            (A, Axis::Horizontal, B),
            (B, Axis::Vertical, C),
            (A, Axis::Vertical, D),
        ],
    );
    assert!(check_tiling(&t, (3, 5, 101, 77), 7));
}

#[test]
fn no_leaf_is_outside_the_area_even_when_there_is_no_room() {
    let t = tree_of(
        A,
        &[
            (A, Axis::Horizontal, B),
            (B, Axis::Vertical, C),
            (A, Axis::Vertical, D),
        ],
    );
    let areas = [
        (0, 0, 0, 0),
        (5, 7, 3, 2),
        (0, 0, 1, 1),
        (0, 0, -10, -10),
        (-50, -50, 40, 0),
        (0, 0, 100, 50),
    ];
    for area in areas {
        for gap in [-5, 0, 1, 6, 1000] {
            let rects = t.layout(area, gap);
            assert_eq!(rects.len(), 4);
            let (w, h) = (area.2.max(0), area.3.max(0));
            for (_, r) in rects {
                assert!(r.2 >= 0 && r.3 >= 0, "{r:?} has a negative size");
                assert!(
                    r.0 >= area.0 && r.0 + r.2 <= area.0 + w,
                    "{r:?} is outside {area:?} (gap {gap})"
                );
                assert!(
                    r.1 >= area.1 && r.1 + r.3 <= area.1 + h,
                    "{r:?} is outside {area:?} (gap {gap})"
                );
            }
        }
    }
}

#[test]
fn split_needs_a_leaf_that_exists_and_a_new_window() {
    let mut t = pair();
    let before = t.clone();
    assert_eq!(t.split(C, Axis::Vertical, NEW), Err(SplitError::NoSuchLeaf));
    assert_eq!(t.split(A, Axis::Vertical, B), Err(SplitError::Duplicate));
    assert_eq!(t.split(A, Axis::Vertical, A), Err(SplitError::Duplicate));
    assert_eq!(t, before);
}

#[test]
fn split_does_nothing_on_an_empty_tree() {
    let mut t = TileTree::new(A);
    t.remove(A);
    assert_eq!(t.split(A, Axis::Horizontal, B), Err(SplitError::NoSuchLeaf));
    assert!(t.is_empty());
}

#[test]
fn insert_then_remove_restores_the_tree() {
    let mut rng = Lcg(11);
    for _ in 0..50 {
        let leaves = 1 + rng.below(6);
        let mut t = random_tree(&mut rng, leaves, AREA, 4);
        let before = t.clone();
        for at in before.leaves() {
            for axis in [Axis::Horizontal, Axis::Vertical] {
                assert_eq!(t.split(at, axis, NEW), Ok(()));
                assert_eq!(t.len(), before.len() + 1);
                assert_eq!(t.remove(NEW), Removal::Removed { focus: at });
                assert_eq!(t, before, "split {at} then remove did not restore");
            }
        }
    }
}

#[test]
fn insert_then_remove_keeps_the_ratios_of_every_other_split() {
    let mut t = three();
    t.resize(A, Edge::Right, 30, AREA, 0, 5);
    t.resize(B, Edge::Bottom, 10, AREA, 0, 5);
    let before = t.clone();
    t.split(C, Axis::Horizontal, NEW).unwrap();
    t.remove(NEW);
    assert_eq!(t, before);
}

#[test]
fn removing_a_leaf_collapses_its_parent_and_the_sibling_takes_its_place() {
    let mut t = three();
    assert_eq!(t.remove(B), Removal::Removed { focus: C });
    assert_eq!(t, pair_of(A, C));
    assert_eq!(
        t.layout(AREA, 0),
        [(A, (0, 0, 50, 50)), (C, (50, 0, 50, 50))]
    );

    let mut t = three();
    assert_eq!(t.remove(A), Removal::Removed { focus: B });
    assert_eq!(t, tree_of(B, &[(B, Axis::Vertical, C)]));
    assert_eq!(
        t.layout(AREA, 0),
        [(B, (0, 0, 100, 25)), (C, (0, 25, 100, 25))]
    );
}

fn pair_of(first: u32, second: u32) -> TileTree<u32> {
    tree_of(first, &[(first, Axis::Horizontal, second)])
}

#[test]
fn the_sibling_keeps_its_own_ratios_when_it_moves_up() {
    let mut t = three();
    // Move the divider between B and C, then remove A: the B/C split is the root now.
    assert_eq!(t.resize(B, Edge::Bottom, 10, AREA, 0, 5), Some(0.2));
    t.remove(A);
    assert_eq!(root_ratio(&t), 0.2);
    assert_eq!(
        t.layout(AREA, 0),
        [(B, (0, 0, 100, 10)), (C, (0, 10, 100, 40))]
    );
}

#[test]
fn remove_names_the_nearest_leaf_in_tree_order() {
    // (A / B) | (C / D), in tree order A B C D.
    let t = tree_of(
        A,
        &[
            (A, Axis::Horizontal, C),
            (A, Axis::Vertical, B),
            (C, Axis::Vertical, D),
        ],
    );
    assert_eq!(t.leaves(), [A, B, C, D]);

    // A first child: the first leaf of the sibling. A second child: its last leaf.
    let mut x = t.clone();
    assert_eq!(x.remove(A), Removal::Removed { focus: B });
    let mut x = t.clone();
    assert_eq!(x.remove(B), Removal::Removed { focus: A });
    let mut x = t.clone();
    assert_eq!(x.remove(C), Removal::Removed { focus: D });
    let mut x = t.clone();
    assert_eq!(x.remove(D), Removal::Removed { focus: C });

    // The sibling is a whole subtree.
    let mut x = three();
    assert_eq!(x.remove(A), Removal::Removed { focus: B });
    let mut x = tree_of(A, &[(A, Axis::Horizontal, B), (A, Axis::Vertical, C)]);
    assert_eq!(x.leaves(), [A, C, B]);
    assert_eq!(x.remove(B), Removal::Removed { focus: C });
}

#[test]
fn removing_the_last_leaf_empties_the_tree() {
    let mut t = pair();
    assert_eq!(t.remove(A), Removal::Removed { focus: B });
    assert!(!t.is_empty());
    assert_eq!(t.len(), 1);

    assert_eq!(t.remove(B), Removal::Emptied);
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert!(t.leaves().is_empty());
    assert!(t.layout(AREA, 4).is_empty());
    assert!(!t.contains(B));
    assert_eq!(t.root(), None);
}

#[test]
fn removing_a_window_that_is_not_there_changes_nothing() {
    let mut t = three();
    let before = t.clone();
    assert_eq!(t.remove(NEW), Removal::NotFound);
    assert_eq!(t, before);

    let mut e = TileTree::new(A);
    e.remove(A);
    assert_eq!(e.remove(A), Removal::NotFound);
    assert!(e.is_empty());
}

#[test]
fn dragging_the_right_edge_of_the_first_half_moves_the_divider() {
    let mut t = pair();
    assert_eq!(t.resize(A, Edge::Right, 60, AREA, 0, 10), Some(0.6));
    assert_eq!(root_ratio(&t), 0.6);
    assert_eq!(
        t.layout(AREA, 0),
        [(A, (0, 0, 60, 50)), (B, (60, 0, 40, 50))]
    );
}

#[test]
fn dragging_the_left_edge_of_the_second_half_puts_that_edge_on_the_pointer() {
    let mut t = pair();
    // With a gap the left edge of B is a gap further than the right edge of A.
    assert!(t.resize(B, Edge::Left, 70, AREA, 10, 10).is_some());
    assert_eq!(rect_of(&t, AREA, 10, B), (70, 0, 30, 50));
    assert_eq!(rect_of(&t, AREA, 10, A), (0, 0, 60, 50));

    // Both edges of the same divider give the same answer.
    let mut u = pair();
    assert!(u.resize(A, Edge::Right, 60, AREA, 10, 10).is_some());
    assert_eq!(t, u);
}

#[test]
fn dragging_top_and_bottom_moves_a_vertical_divider() {
    let mut t = tree_of(A, &[(A, Axis::Vertical, B)]);
    assert_eq!(t.resize(B, Edge::Top, 10, AREA, 0, 5), Some(0.2));
    assert_eq!(
        t.layout(AREA, 0),
        [(A, (0, 0, 100, 10)), (B, (0, 10, 100, 40))]
    );
    assert_eq!(t.resize(A, Edge::Bottom, 40, AREA, 0, 5), Some(0.8));
    assert_eq!(
        t.layout(AREA, 0),
        [(A, (0, 0, 100, 40)), (B, (0, 40, 100, 10))]
    );
}

#[test]
fn a_resize_works_in_the_space_the_area_is_in() {
    let area = (1920, 40, 100, 50);
    let mut t = pair();
    assert_eq!(t.resize(A, Edge::Right, 1980, area, 0, 10), Some(0.6));
    assert_eq!(rect_of(&t, area, 0, A), (1920, 40, 60, 50));
}

#[test]
fn a_resize_past_the_ends_is_clamped_so_neither_half_collapses() {
    let mut t = pair();
    assert_eq!(t.resize(A, Edge::Right, -500, AREA, 0, 10), Some(0.1));
    assert_eq!(rect_of(&t, AREA, 0, A), (0, 0, 10, 50));
    assert_eq!(t.resize(A, Edge::Right, 10_000, AREA, 0, 10), Some(0.9));
    assert_eq!(rect_of(&t, AREA, 0, B), (90, 0, 10, 50));

    // With a gap the room to share is the length less the gap.
    let mut t = pair();
    assert!(t.resize(A, Edge::Right, -500, AREA, 10, 10).is_some());
    assert_eq!(rect_of(&t, AREA, 10, A), (0, 0, 10, 50));
    assert!(t.resize(A, Edge::Right, 10_000, AREA, 10, 10).is_some());
    assert_eq!(rect_of(&t, AREA, 10, A), (0, 0, 80, 50));
    assert_eq!(rect_of(&t, AREA, 10, B), (90, 0, 10, 50));
}

#[test]
fn a_resize_never_leaves_zero_to_one_whatever_the_position() {
    for edge in [Edge::Right, Edge::Left] {
        for to in [i32::MIN, -1, 0, 1, 50, 99, 100, 101, i32::MAX] {
            let mut t = pair();
            let leaf = if edge == Edge::Right { A } else { B };
            let ratio = t
                .resize(leaf, edge, to, AREA, 6, 4)
                .expect("the edge moves a split");
            assert!(ratio > 0.0 && ratio < 1.0, "ratio {ratio} for to {to}");
            assert_eq!(root_ratio(&t), ratio);
            let (a, b) = (rect_of(&t, AREA, 6, A), rect_of(&t, AREA, 6, B));
            assert!(a.2 >= 4 && b.2 >= 4, "{a:?} {b:?} for to {to}");
        }
    }
}

#[test]
fn a_resize_in_a_split_too_short_for_two_halves_goes_back_to_even() {
    let mut t = pair();
    t.resize(A, Edge::Right, 60, AREA, 0, 10);
    // 15 wide, 5 of gap: 10 px to share, less than two halves of 10.
    assert_eq!(
        t.resize(A, Edge::Right, 12, (0, 0, 15, 50), 5, 10),
        Some(DEFAULT_RATIO)
    );
    assert_eq!(t.resize(A, Edge::Right, 0, (0, 0, 0, 0), 0, 10), Some(0.5));
    assert_eq!(
        t.resize(A, Edge::Right, 0, (0, 0, -20, 50), 3, 10),
        Some(0.5)
    );
}

#[test]
fn a_negative_minimum_counts_as_zero() {
    let mut t = pair();
    assert_eq!(t.resize(A, Edge::Right, -5, AREA, 0, -3), Some(0.0));
    assert_eq!(rect_of(&t, AREA, 0, A), (0, 0, 0, 50));
}

#[test]
fn an_outer_edge_moves_nothing() {
    let mut t = pair();
    let before = t.clone();
    assert_eq!(t.resize(A, Edge::Left, 10, AREA, 0, 10), None);
    assert_eq!(t.resize(B, Edge::Right, 90, AREA, 0, 10), None);
    // The wrong axis: there is no vertical divider.
    assert_eq!(t.resize(A, Edge::Top, 10, AREA, 0, 10), None);
    assert_eq!(t.resize(B, Edge::Bottom, 10, AREA, 0, 10), None);
    assert_eq!(t, before);

    assert_eq!(
        TileTree::new(A).resize(A, Edge::Right, 10, AREA, 0, 10),
        None
    );
}

#[test]
fn a_resize_of_a_missing_leaf_or_an_empty_tree_moves_nothing() {
    let mut t = pair();
    let before = t.clone();
    assert_eq!(t.resize(NEW, Edge::Right, 60, AREA, 0, 10), None);
    assert_eq!(t, before);

    let mut e = TileTree::new(A);
    e.remove(A);
    assert_eq!(e.resize(A, Edge::Right, 60, AREA, 0, 10), None);
}

#[test]
fn an_edge_moves_the_nearest_split_around_the_leaf_on_its_axis() {
    // A | (B | C) in 200 px: the root cuts at 100, the inner split at 150.
    let area = (0, 0, 200, 50);
    let t = tree_of(A, &[(A, Axis::Horizontal, B), (B, Axis::Horizontal, C)]);

    // The right edge of B is the inner divider.
    let mut x = t.clone();
    assert!(x.resize(B, Edge::Right, 120, area, 0, 10).is_some());
    assert_eq!(rect_of(&x, area, 0, A), (0, 0, 100, 50));
    assert_eq!(rect_of(&x, area, 0, B), (100, 0, 20, 50));
    assert_eq!(rect_of(&x, area, 0, C), (120, 0, 80, 50));

    // The left edge of B is the root divider, the inner split has B in its first half.
    let mut x = t.clone();
    assert_eq!(x.resize(B, Edge::Left, 60, area, 0, 10), Some(0.3));
    assert_eq!(root_ratio(&x), 0.3);
    assert_eq!(rect_of(&x, area, 0, A), (0, 0, 60, 50));
    assert_eq!(rect_of(&x, area, 0, B), (60, 0, 70, 50));
    assert_eq!(rect_of(&x, area, 0, C), (130, 0, 70, 50));

    // The left edge of C is the inner divider.
    let mut x = t.clone();
    assert!(x.resize(C, Edge::Left, 180, area, 0, 10).is_some());
    assert_eq!(rect_of(&x, area, 0, B), (100, 0, 80, 50));
    assert_eq!(rect_of(&x, area, 0, C), (180, 0, 20, 50));
}

#[test]
fn an_edge_skips_splits_on_the_other_axis() {
    // A | (B / C): the right edge of B is the border of the container.
    let mut t = three();
    assert_eq!(t.resize(B, Edge::Right, 90, AREA, 0, 5), None);
    // The left edge of B is the root divider although the split around B is vertical.
    assert_eq!(t.resize(B, Edge::Left, 40, AREA, 0, 5), Some(0.4));
    assert_eq!(
        t.layout(AREA, 0),
        [
            (A, (0, 0, 40, 50)),
            (B, (40, 0, 60, 25)),
            (C, (40, 25, 60, 25))
        ]
    );
    // Its bottom edge is the divider between B and C, in the rectangle the root left it.
    assert_eq!(t.resize(B, Edge::Bottom, 10, AREA, 0, 5), Some(0.2));
    assert_eq!(rect_of(&t, AREA, 0, B), (40, 0, 60, 10));
    assert_eq!(rect_of(&t, AREA, 0, C), (40, 10, 60, 40));
}

#[test]
fn the_same_drag_position_gives_the_same_tree() {
    let mut t = three();
    t.resize(B, Edge::Left, 33, AREA, 4, 5);
    let once = t.clone();
    t.resize(B, Edge::Left, 33, AREA, 4, 5);
    assert_eq!(t, once);
}

#[test]
fn many_small_drags_leave_no_rounding_behind() {
    let mut dragged = pair();
    for i in 0..1000 {
        let to = if i % 2 == 0 { 67 } else { 33 };
        dragged.resize(A, Edge::Right, to, AREA, 6, 5);
    }
    let mut once = pair();
    once.resize(A, Edge::Right, 33, AREA, 6, 5);
    assert_eq!(dragged, once);
    assert_eq!(rect_of(&dragged, AREA, 6, A), (0, 0, 33, 50));
}

#[test]
fn next_and_previous_follow_tree_order_and_wrap() {
    // A | (B / C), then D beside C: tree order A B C D.
    let mut t = three();
    t.split(C, Axis::Horizontal, D).unwrap();
    assert_eq!(t.leaves(), [A, B, C, D]);

    assert_eq!(t.next_leaf(A), Some(B));
    assert_eq!(t.next_leaf(B), Some(C));
    assert_eq!(t.next_leaf(C), Some(D));
    assert_eq!(t.next_leaf(D), Some(A));

    assert_eq!(t.prev_leaf(D), Some(C));
    assert_eq!(t.prev_leaf(C), Some(B));
    assert_eq!(t.prev_leaf(B), Some(A));
    assert_eq!(t.prev_leaf(A), Some(D));
}

#[test]
fn next_is_the_tree_order_and_not_the_nearest_on_screen() {
    // (A / B) | C: B is below A, so it is next, although C is the one on the right.
    let t = tree_of(A, &[(A, Axis::Horizontal, C), (A, Axis::Vertical, B)]);
    assert_eq!(t.next_leaf(A), Some(B));
    assert_eq!(t.next_leaf(B), Some(C));
}

#[test]
fn next_and_previous_on_a_single_leaf_are_that_leaf() {
    let t = TileTree::new(A);
    assert_eq!(t.next_leaf(A), Some(A));
    assert_eq!(t.prev_leaf(A), Some(A));
}

#[test]
fn next_and_previous_of_a_missing_leaf_or_an_empty_tree_are_none() {
    let t = three();
    assert_eq!(t.next_leaf(NEW), None);
    assert_eq!(t.prev_leaf(NEW), None);

    let mut e = TileTree::new(A);
    e.remove(A);
    assert_eq!(e.next_leaf(A), None);
    assert_eq!(e.prev_leaf(A), None);
}

#[test]
fn going_next_then_previous_comes_back_for_every_leaf() {
    let mut rng = Lcg(3);
    for _ in 0..50 {
        let leaves = 1 + rng.below(7);
        let t = random_tree(&mut rng, leaves, AREA, 2);
        for w in t.leaves() {
            let next = t.next_leaf(w).expect("the leaf is in the tree");
            assert_eq!(t.prev_leaf(next), Some(w));
        }
    }
}

#[test]
fn contains_tells_which_windows_are_leaves() {
    let t = three();
    assert!(t.contains(A) && t.contains(B) && t.contains(C));
    assert!(!t.contains(NEW));
}

#[test]
fn swapping_two_leaves_exchanges_their_rectangles() {
    let mut t = three();
    let (a, b, c) = (
        rect_of(&t, AREA, 0, A),
        rect_of(&t, AREA, 0, B),
        rect_of(&t, AREA, 0, C),
    );
    assert!(t.swap_leaves(A, C));
    assert_eq!(rect_of(&t, AREA, 0, C), a);
    assert_eq!(rect_of(&t, AREA, 0, A), c);
    assert_eq!(rect_of(&t, AREA, 0, B), b);
    assert_eq!(t.leaves(), [C, B, A]);
}

#[test]
fn swapping_keeps_the_shape_and_the_ratios() {
    let mut t = three();
    let before = root_ratio(&t);
    assert!(t.swap_leaves(B, C));
    assert_eq!(root_ratio(&t), before);
    assert_eq!(t.len(), 3);
    assert!(check_tiling(&t, AREA, 0));
    assert!(t.swap_leaves(B, C));
    assert_eq!(t, three());
}

#[test]
fn swapping_a_leaf_that_is_not_there_or_with_itself_changes_nothing() {
    let mut t = three();
    assert!(!t.swap_leaves(A, NEW));
    assert!(!t.swap_leaves(NEW, A));
    assert!(!t.swap_leaves(B, B));
    assert_eq!(t, three());
    let mut single = TileTree::new(A);
    assert!(!single.swap_leaves(A, B));
}
