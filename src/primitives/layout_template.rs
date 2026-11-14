//! The saved shape of a tiling container: a tree of free or tagged slots, pure, no Smithay types.

use crate::primitives::tile_tree::{Axis, Node, TileTree};

/// One node of a layout.
#[derive(Clone, Debug, PartialEq)]
pub enum Template {
    /// A slot: `None` takes any window, `Some(tag)` the window with that tag.
    Slot(Option<String>),
    Split {
        axis: Axis,
        /// Share of the room the first child gets, in `0.0..=1.0`.
        ratio: f64,
        first: Box<Template>,
        second: Box<Template>,
    },
}

/// What [`Template::fill`] made of a set of windows.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill<W> {
    /// The tree of the windows that found a slot, empty slots closed up.
    pub root: Node<W>,
    /// The windows that found no slot, in the order given.
    pub extra: Vec<W>,
    /// How many slots stayed empty.
    pub empty: usize,
}

impl Template {
    /// The layout of `tree`, each slot tagged like its window, `None` for an empty tree.
    pub fn from_tree<W: Copy + Eq>(
        tree: &TileTree<W>,
        tag: impl Fn(W) -> Option<String>,
    ) -> Option<Template> {
        fn walk<W: Copy>(node: &Node<W>, tag: &dyn Fn(W) -> Option<String>) -> Template {
            match node {
                Node::Leaf(w) => Template::Slot(tag(*w).filter(|t| !t.is_empty())),
                Node::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => Template::Split {
                    axis: *axis,
                    ratio: sane_ratio(*ratio),
                    first: Box::new(walk(first, tag)),
                    second: Box::new(walk(second, tag)),
                },
            }
        }
        tree.root().map(|root| walk(root, &tag))
    }

    /// The tag of every slot in tree order, `None` for a free one.
    pub fn slots(&self) -> Vec<Option<&str>> {
        let mut out = Vec::new();
        collect_slots(self, &mut out);
        out
    }

    /// Put `windows` in the slots: tag matches first, then free slots, then unmatched tagged slots, `None` for no windows.
    pub fn fill<W: Copy + Eq>(&self, windows: &[(W, Option<String>)]) -> Option<Fill<W>> {
        if windows.is_empty() {
            return None;
        }
        let slots = self.slots();
        let mut used = vec![false; windows.len()];
        let mut placed: Vec<Option<W>> = vec![None; slots.len()];

        let mut take = |placed: &mut Vec<Option<W>>, slot: usize, wanted: Option<&str>| {
            let found = windows.iter().enumerate().find(|(j, (_, tag))| {
                !used[*j] && wanted.is_none_or(|wanted| tag.as_deref() == Some(wanted))
            });
            if let Some((j, (window, _))) = found {
                used[j] = true;
                placed[slot] = Some(*window);
            }
        };
        for (i, slot) in slots.iter().enumerate() {
            if let Some(tag) = slot {
                take(&mut placed, i, Some(tag));
            }
        }
        for (i, slot) in slots.iter().enumerate() {
            if slot.is_none() && placed[i].is_none() {
                take(&mut placed, i, None);
            }
        }
        for (i, slot) in slots.iter().enumerate() {
            if slot.is_some() && placed[i].is_none() {
                take(&mut placed, i, None);
            }
        }

        let empty = placed.iter().filter(|p| p.is_none()).count();
        let extra = windows
            .iter()
            .enumerate()
            .filter(|(j, _)| !used[*j])
            .map(|(_, (window, _))| *window)
            .collect();
        let mut next = 0;
        let root = build(self, &mut next, &placed)?;
        Some(Fill { root, extra, empty })
    }

    /// The layout as one line like `h0.7(_,@browser)`: `h` side by side, `v` stacked, share of the first half, `_` free slot, `@tag` tagged slot (`%XX` escapes).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        write_text(self, &mut out);
        out
    }

    /// The layout a line of text says, or why it is not one.
    pub fn parse(text: &str) -> Result<Template, String> {
        let mut parser = Parser {
            bytes: text.trim().as_bytes(),
            at: 0,
        };
        let template = parser.node(0)?;
        if parser.at != parser.bytes.len() {
            return Err(format!("unexpected text at position {}", parser.at));
        }
        Ok(template)
    }
}

/// The deepest nesting `parse` follows.
const MAX_DEPTH: usize = 64;

fn sane_ratio(ratio: f64) -> f64 {
    if ratio.is_nan() {
        0.5
    } else {
        ratio.clamp(0.0, 1.0)
    }
}

fn collect_slots<'a>(node: &'a Template, out: &mut Vec<Option<&'a str>>) {
    match node {
        Template::Slot(tag) => out.push(tag.as_deref()),
        Template::Split { first, second, .. } => {
            collect_slots(first, out);
            collect_slots(second, out);
        }
    }
}

/// `node` filled with `placed` (one per slot, tree order), empty slots and their splits dropped.
fn build<W: Copy>(node: &Template, next: &mut usize, placed: &[Option<W>]) -> Option<Node<W>> {
    match node {
        Template::Slot(_) => {
            let slot = *next;
            *next += 1;
            placed.get(slot).copied().flatten().map(Node::Leaf)
        }
        Template::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = build(first, next, placed);
            let second = build(second, next, placed);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

fn write_text(node: &Template, out: &mut String) {
    match node {
        Template::Slot(None) => out.push('_'),
        Template::Slot(Some(tag)) => {
            out.push('@');
            for byte in tag.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                    out.push(char::from(byte));
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        Template::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            out.push(match axis {
                Axis::Horizontal => 'h',
                Axis::Vertical => 'v',
            });
            out.push_str(&format_ratio(*ratio));
            out.push('(');
            write_text(first, out);
            out.push(',');
            write_text(second, out);
            out.push(')');
        }
    }
}

/// The ratio with at most four decimals and no trailing zeros.
fn format_ratio(ratio: f64) -> String {
    let text = format!("{:.4}", sane_ratio(ratio));
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expect(&mut self, want: u8) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!(
                "expected {:?} at position {}",
                char::from(want),
                self.at
            ))
        }
    }

    fn node(&mut self, depth: usize) -> Result<Template, String> {
        if depth > MAX_DEPTH {
            return Err("the layout nests too deeply".to_owned());
        }
        match self.peek() {
            Some(b'_') => {
                self.at += 1;
                Ok(Template::Slot(None))
            }
            Some(b'@') => {
                self.at += 1;
                self.tag().map(|tag| Template::Slot(Some(tag)))
            }
            Some(letter @ (b'h' | b'v')) => {
                self.at += 1;
                let axis = if letter == b'h' {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let ratio = self.ratio()?;
                self.expect(b'(')?;
                let first = self.node(depth + 1)?;
                self.expect(b',')?;
                let second = self.node(depth + 1)?;
                self.expect(b')')?;
                Ok(Template::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                })
            }
            _ => Err(format!("expected _, @tag, h or v at position {}", self.at)),
        }
    }

    fn ratio(&mut self) -> Result<f64, String> {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit() || b == b'.') {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).unwrap_or("");
        let ratio: f64 = text
            .parse()
            .map_err(|_| format!("{text:?} is not a ratio (position {start})"))?;
        if !(0.0..=1.0).contains(&ratio) {
            return Err(format!("the ratio {text} is not between 0 and 1"));
        }
        Ok(ratio)
    }

    fn tag(&mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        while let Some(byte) = self.peek() {
            match byte {
                b'%' => {
                    let hex = self
                        .bytes
                        .get(self.at + 1..self.at + 3)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                        .ok_or_else(|| format!("bad %XX in the tag at position {}", self.at))?;
                    bytes.push(hex);
                    self.at += 3;
                }
                b if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') => {
                    bytes.push(b);
                    self.at += 1;
                }
                _ => break,
            }
        }
        if bytes.is_empty() {
            return Err(format!("@ needs a tag, at position {}", self.at));
        }
        String::from_utf8(bytes).map_err(|_| "the tag is not valid UTF-8".to_owned())
    }
}
