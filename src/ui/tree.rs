//! Shared path-tree machinery for panels that render `/`-separated names as a
//! collapsible tree: grouping names into folders, flattening around collapsed
//! state, and painting the guide lines, chevrons, and folder rows. Used by the
//! branches panel and the tree-mode file lists in the worktree column.

use std::collections::HashSet;

use eframe::egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2};

use crate::theme::{self, BG3, DIM};

/// Horizontal step per tree depth level.
pub const INDENT: f32 = 14.0;

/// A node in a path tree. A leaf points back into the caller's flat list by
/// index; a folder owns an ordered child list (which may mix folders and
/// leaves).
pub enum Node {
    Leaf(usize),
    Folder {
        name: String,
        /// Full `/`-joined path including the `build` key prefix -- the
        /// collapse-state key (`origin/feature`, `staged/src/ui`).
        path: String,
        children: Vec<Node>,
    },
}

/// Inserts one name -- pre-split into `/` segments -- into the tree, creating
/// folders along the way. The flat list is name-sorted, so all leaves under a
/// folder are contiguous and the live folder is whichever matching one sits
/// nearest the end; scanning from the back finds it.
fn insert(level: &mut Vec<Node>, segs: &[&str], prefix: &str, idx: usize) {
    if segs.len() == 1 {
        level.push(Node::Leaf(idx));
        return;
    }
    let folder = segs[0];
    let path = if prefix.is_empty() {
        folder.to_string()
    } else {
        format!("{prefix}/{folder}")
    };
    let pos = level
        .iter()
        .rposition(|n| matches!(n, Node::Folder { name, .. } if name == folder));
    match pos {
        Some(p) => {
            if let Node::Folder { children, .. } = &mut level[p] {
                insert(children, &segs[1..], &path, idx);
            }
        }
        None => {
            let mut children = Vec::new();
            insert(&mut children, &segs[1..], &path, idx);
            level.push(Node::Folder {
                name: folder.to_string(),
                path,
                children,
            });
        }
    }
}

/// Groups an already-ordered flat name list into a tree by `/` segments. Child
/// order is the input order, so folders and leaves interleave by name.
/// `key_prefix` seeds every folder's collapse-state key, so callers with
/// several trees can share one collapse set without key collisions.
pub fn build<'a>(names: impl Iterator<Item = &'a str>, key_prefix: &str) -> Vec<Node> {
    let mut roots = Vec::new();
    for (i, name) in names.enumerate() {
        let segs: Vec<&str> = name.split('/').collect();
        insert(&mut roots, &segs, key_prefix, i);
    }
    roots
}

/// One drawn line of the tree, after collapsed folders have hidden their
/// subtrees. `depth` drives indentation; `lanes`/`is_last` drive the guide
/// lines (see `connectors`).
#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub depth: usize,
    /// Bit `c` set = an ancestor's subtree continues past this row, so a
    /// vertical pass-through line is drawn in indent column `c`.
    pub lanes: u64,
    /// This node is the last child of its parent, so its elbow is an `L`
    /// rather than a `T`.
    pub is_last: bool,
    pub kind: RowKind<'a>,
}

#[derive(Clone, Copy)]
pub enum RowKind<'a> {
    Folder {
        name: &'a str,
        path: &'a str,
        collapsed: bool,
    },
    /// Index into the flat list `build` was fed.
    Leaf(usize),
}

/// Walks the tree into the visible row list, skipping the children of any
/// collapsed folder.
pub fn rows<'a>(nodes: &'a [Node], collapsed: &HashSet<String>) -> Vec<Row<'a>> {
    let mut out = Vec::new();
    flatten(nodes, collapsed, 0, 0, &mut out);
    out
}

/// `rows` recursion. `lanes` accumulates which indent columns still have a
/// vertical guide line at this depth.
fn flatten<'a>(
    nodes: &'a [Node],
    collapsed: &HashSet<String>,
    depth: usize,
    lanes: u64,
    out: &mut Vec<Row<'a>>,
) {
    let last = nodes.len().saturating_sub(1);
    for (i, n) in nodes.iter().enumerate() {
        let is_last = i == last;
        match n {
            Node::Leaf(j) => out.push(Row {
                depth,
                lanes,
                is_last,
                kind: RowKind::Leaf(*j),
            }),
            Node::Folder {
                name,
                path,
                children,
            } => {
                let is_collapsed = collapsed.contains(path.as_str());
                out.push(Row {
                    depth,
                    lanes,
                    is_last,
                    kind: RowKind::Folder {
                        name,
                        path,
                        collapsed: is_collapsed,
                    },
                });
                if !is_collapsed {
                    // A non-last folder leaves a pass-through lane in its own
                    // elbow column for everything below it.
                    let child_lanes = if depth >= 1 && !is_last {
                        lanes | 1 << (depth - 1)
                    } else {
                        lanes
                    };
                    flatten(children, collapsed, depth + 1, child_lanes, out);
                }
            }
        }
    }
}

/// Draws the tree guide lines for one row: vertical pass-throughs for ancestor
/// lanes, plus the row's own elbow (`├` when more siblings follow, `└` when
/// last). Columns align under the parent chevron one level up.
pub fn connectors(p: &Painter, rect: Rect, depth: usize, lanes: u64, is_last: bool) {
    if depth == 0 {
        return;
    }
    let stroke = Stroke::new(1.0_f32, theme::alpha(DIM, 150));
    let col_x = |c: usize| rect.left() + 8.0 + c as f32 * INDENT;
    let mid = rect.center().y;
    // Ancestor pass-throughs occupy columns below the elbow.
    for c in 0..depth - 1 {
        if lanes & (1 << c) != 0 {
            p.line_segment(
                [pos2(col_x(c), rect.top()), pos2(col_x(c), rect.bottom())],
                stroke,
            );
        }
    }
    // Elbow: vertical down to the row centre (or full height if siblings
    // follow), then a horizontal stub reaching toward this row's marker.
    let ex = col_x(depth - 1);
    let v_bottom = if is_last { mid } else { rect.bottom() };
    p.line_segment([pos2(ex, rect.top()), pos2(ex, v_bottom)], stroke);
    p.line_segment([pos2(ex, mid), pos2(col_x(depth) - 5.0, mid)], stroke);
}

/// A folder's collapse marker: a small filled triangle, right-pointing when
/// collapsed and down-pointing when open. Painted rather than glyph'd --
/// Rajdhani lacks the glyph.
fn chevron(p: &Painter, c: Pos2, collapsed: bool) {
    let s = 3.5;
    let pts = if collapsed {
        vec![
            pos2(c.x - s * 0.6, c.y - s),
            pos2(c.x - s * 0.6, c.y + s),
            pos2(c.x + s * 0.8, c.y),
        ]
    } else {
        vec![
            pos2(c.x - s, c.y - s * 0.6),
            pos2(c.x + s, c.y - s * 0.6),
            pos2(c.x, c.y + s * 0.8),
        ]
    };
    p.add(Shape::convex_polygon(pts, DIM, Stroke::NONE));
}

/// Paints one folder row: hover fill, chevron, the open-folder descender down
/// to the first child's elbow, and the DIM name. The caller handles the click.
pub fn folder_row(
    p: &Painter,
    rect: Rect,
    hovered: bool,
    indent: f32,
    name: &str,
    collapsed: bool,
    font: FontId,
) {
    if hovered {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    }
    let y = rect.center().y;
    let cx = rect.left() + 8.0 + indent;
    chevron(p, pos2(cx, y), collapsed);
    if !collapsed {
        p.line_segment(
            [pos2(cx, y), pos2(cx, rect.bottom())],
            Stroke::new(1.0_f32, theme::alpha(DIM, 150)),
        );
    }
    p.text(
        pos2(rect.left() + 20.0 + indent, y),
        Align2::LEFT_CENTER,
        name,
        font,
        DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(rows: &[Row], flat: &[&str]) -> Vec<String> {
        rows.iter()
            .map(|r| match r.kind {
                RowKind::Folder { name, .. } => format!("{}/", name),
                RowKind::Leaf(i) => flat[i].to_string(),
            })
            .collect()
    }

    #[test]
    fn groups_sorted_paths_into_folders() {
        let flat = ["a/b.rs", "a/c.rs", "d.rs"];
        let tree = build(flat.iter().copied(), "");
        let rows = rows(&tree, &HashSet::new());
        assert_eq!(names(&rows, &flat), ["a/", "a/b.rs", "a/c.rs", "d.rs"]);
        assert_eq!(
            rows.iter().map(|r| r.depth).collect::<Vec<_>>(),
            [0, 1, 1, 0]
        );
    }

    #[test]
    fn collapsed_folder_hides_children_and_prefix_keys_the_state() {
        let flat = ["a/b.rs", "a/c.rs", "d.rs"];
        let tree = build(flat.iter().copied(), "staged");
        let mut collapsed = HashSet::new();
        collapsed.insert("staged/a".to_string());
        let rows = rows(&tree, &collapsed);
        assert_eq!(names(&rows, &flat), ["a/", "d.rs"]);
    }

    #[test]
    fn nested_folders_carry_full_paths() {
        let flat = ["x/y/z.rs"];
        let tree = build(flat.iter().copied(), "");
        let rows = rows(&tree, &HashSet::new());
        let RowKind::Folder { path, .. } = rows[1].kind else {
            panic!("expected folder");
        };
        assert_eq!(path, "x/y");
    }
}
