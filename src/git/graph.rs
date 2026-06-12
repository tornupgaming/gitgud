//! Commit-graph lane layout: pure data transform `&[CommitInfo] -> Graph`.
//!
//! Runs on the UI thread whenever the commit list changes (a few thousand
//! commits is microseconds). The renderer in `ui::graph` consumes the rows
//! verbatim; all topology decisions are made here so drawing stays dumb.

use super::CommitInfo;

/// Lane layout for one loaded commit list. `rows[i]` corresponds to
/// `commits[i]` in the same order.
#[derive(Debug, Default)]
pub struct Graph {
    pub rows: Vec<GraphRow>,
    /// Total number of lanes ever occupied; the renderer sizes the graph
    /// column from this.
    pub lanes: u16,
}

#[derive(Debug)]
pub struct GraphRow {
    /// Lane the commit node sits in.
    pub lane: u16,
    /// Segments drawn between this row's centerline and the next row's
    /// centerline (includes lanes merely passing through this row).
    pub edges: Vec<Edge>,
}

/// One segment from `(from, row i center)` to `(to, row i+1 center)`.
/// `color` indexes `theme::LANES` (mod len). Edges starting at the row's
/// node have `from == row.lane`; pass-throughs have `from == to`.
/// Edges on the last row (parents not loaded yet) are drawn as stubs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: u16,
    pub to: u16,
    pub color: u16,
}

/// Assign lanes with the standard streaming algorithm: keep a vector of
/// "active" lanes, each expecting a parent id; a commit claims the leftmost
/// lane expecting it (or a free one), merges duplicates in, and spawns new
/// lanes for extra parents.
pub fn compute(commits: &[CommitInfo]) -> Graph {
    let mut active: Vec<Option<&str>> = Vec::new();
    let mut rows: Vec<GraphRow> = Vec::with_capacity(commits.len());
    let mut max_lanes = 0usize;

    // Edge emission lags one row: lanes expecting commit i+1 must bend into
    // its node, so row i's edges are only known once row i+1's node lane is.
    let mut prev_node = 0u16;
    let mut fresh: Vec<u16> = Vec::new(); // lanes born at the previous node (branch-outs)
    let mut dups: Vec<u16> = Vec::new(); // pre-existing lanes the previous node also feeds

    for (i, c) in commits.iter().enumerate() {
        let id = c.id.as_str();
        let expected = active.iter().position(|s| *s == Some(id));

        if i > 0 {
            rows[i - 1].edges = edges_between(&active, id, expected, prev_node, &fresh, &dups);
        }

        let node = match expected {
            Some(k) => {
                // Every lane expecting this commit converges into the node;
                // the leftmost becomes the node lane, the rest free up.
                for slot in active.iter_mut() {
                    if *slot == Some(id) {
                        *slot = None;
                    }
                }
                k
            }
            None => claim_free(&mut active),
        };
        active[node] = c.parents.first().map(String::as_str);

        fresh.clear();
        dups.clear();
        for p in c.parents.iter().skip(1) {
            match active.iter().position(|s| *s == Some(p.as_str())) {
                Some(k) => dups.push(k as u16),
                None => {
                    let t = claim_free(&mut active);
                    active[t] = Some(p);
                    fresh.push(t as u16);
                }
            }
        }

        prev_node = node as u16;
        // Measured before trimming so the node lane and lanes that just
        // converged still count toward the column width.
        max_lanes = max_lanes.max(active.len());
        while active.last() == Some(&None) {
            active.pop();
        }
        rows.push(GraphRow {
            lane: node as u16,
            edges: Vec::new(),
        });
    }

    // Stubs running off the bottom: lanes still waiting on unloaded parents.
    if let Some(last) = rows.last_mut() {
        last.edges = active
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(k, _)| Edge {
                from: k as u16,
                to: k as u16,
                color: k as u16,
            })
            .collect();
    }

    Graph {
        rows,
        lanes: max_lanes as u16,
    }
}

/// Edges from row i to row i+1, given the lane state after row i. Any lane
/// expecting `next_id` bends to `next_node` (row i+1's node lane); everything
/// else runs straight down. `fresh` lanes start at the node instead of their
/// own column; `dups` are extra diagonals into already-expected lanes.
fn edges_between(
    active: &[Option<&str>],
    next_id: &str,
    next_node: Option<usize>,
    node: u16,
    fresh: &[u16],
    dups: &[u16],
) -> Vec<Edge> {
    let dest = |k: usize| match next_node {
        Some(n) if active[k] == Some(next_id) => n as u16,
        _ => k as u16,
    };
    let mut edges = Vec::with_capacity(active.len() + dups.len());
    for (k, slot) in active.iter().enumerate() {
        if slot.is_none() {
            continue;
        }
        let from = if fresh.contains(&(k as u16)) {
            node
        } else {
            k as u16
        };
        edges.push(Edge {
            from,
            to: dest(k),
            color: k as u16,
        });
    }
    for &k in dups {
        edges.push(Edge {
            from: node,
            to: dest(k as usize),
            color: k,
        });
    }
    edges
}

fn claim_free(active: &mut Vec<Option<&str>>) -> usize {
    match active.iter().position(Option::is_none) {
        Some(k) => k,
        None => {
            active.push(None);
            active.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, parents: &[&str]) -> CommitInfo {
        CommitInfo {
            id: id.into(),
            short: String::new(),
            summary: String::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
            is_merge: parents.len() > 1,
            refs: Vec::new(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn e(from: u16, to: u16, color: u16) -> Edge {
        Edge { from, to, color }
    }

    #[test]
    fn linear_chain() {
        let g = compute(&[c("a", &["b"]), c("b", &["c"]), c("c", &[])]);
        assert_eq!(g.lanes, 1);
        assert_eq!(g.rows.len(), 3);
        for row in &g.rows[..2] {
            assert_eq!(row.lane, 0);
            assert_eq!(row.edges, vec![e(0, 0, 0)]);
        }
        assert_eq!(g.rows[2].lane, 0);
        assert!(g.rows[2].edges.is_empty(), "root has no outgoing edges");
    }

    #[test]
    fn diamond() {
        let g = compute(&[
            c("m", &["b", "c"]),
            c("b", &["r"]),
            c("c", &["r"]),
            c("r", &[]),
        ]);
        assert_eq!(g.lanes, 2);
        assert_eq!(
            g.rows.iter().map(|r| r.lane).collect::<Vec<_>>(),
            vec![0, 0, 1, 0]
        );
        // M continues to B in lane 0 and branches out to lane 1 for C.
        assert_eq!(g.rows[0].edges, vec![e(0, 0, 0), e(0, 1, 1)]);
        assert_eq!(g.rows[1].edges, vec![e(0, 0, 0), e(1, 1, 1)]);
        // Both lanes converge into R at lane 0.
        assert_eq!(g.rows[2].edges, vec![e(0, 0, 0), e(1, 0, 1)]);
        assert!(g.rows[3].edges.is_empty(), "no dangling lanes after root");
    }

    #[test]
    fn lane_reuse_after_free() {
        // M branches to lane 1 (B); A is parentless so lane 0 frees while
        // lane 1 is still live; new tip T must reuse lane 0.
        let g = compute(&[
            c("m", &["a", "b"]),
            c("a", &[]),
            c("t", &["b"]),
            c("b", &[]),
        ]);
        assert_eq!(g.lanes, 2);
        assert_eq!(
            g.rows.iter().map(|r| r.lane).collect::<Vec<_>>(),
            vec![0, 0, 0, 0]
        );
        assert_eq!(g.rows[0].edges, vec![e(0, 0, 0), e(0, 1, 1)]);
        // Only B's lane survives the parentless A.
        assert_eq!(g.rows[1].edges, vec![e(1, 1, 1)]);
        // T (reused lane 0) and lane 1 both expect B -> converge at lane 0.
        assert_eq!(g.rows[2].edges, vec![e(0, 0, 0), e(1, 0, 1)]);
        assert!(g.rows[3].edges.is_empty());
    }

    #[test]
    fn truncated_tail_emits_stubs() {
        // Last commit is a merge whose parents fall outside the loaded page.
        let g = compute(&[c("a", &["b"]), c("b", &["x", "y"])]);
        assert_eq!(g.lanes, 2);
        assert_eq!(g.rows[0].edges, vec![e(0, 0, 0)]);
        // Both unloaded-parent lanes run straight off the bottom.
        assert_eq!(g.rows[1].edges, vec![e(0, 0, 0), e(1, 1, 1)]);
    }

    #[test]
    fn independent_tips_stay_parallel() {
        let g = compute(&[c("a", &["c"]), c("x", &["z"]), c("c", &[]), c("z", &[])]);
        assert_eq!(g.lanes, 2);
        assert_eq!(
            g.rows.iter().map(|r| r.lane).collect::<Vec<_>>(),
            vec![0, 1, 0, 1]
        );
        assert_eq!(g.rows[0].edges, vec![e(0, 0, 0)]);
        assert_eq!(g.rows[1].edges, vec![e(0, 0, 0), e(1, 1, 1)]);
        assert_eq!(g.rows[2].edges, vec![e(1, 1, 1)]);
        assert!(g.rows[3].edges.is_empty());
        // Two unrelated histories never produce diagonals.
        for row in &g.rows {
            assert!(row.edges.iter().all(|ed| ed.from == ed.to));
        }
    }

    #[test]
    fn extra_parent_already_expected_gets_no_new_lane() {
        // A already waits on P in lane 0; merge M's second parent is P, so M
        // draws a diagonal into lane 0 instead of allocating lane 2.
        let g = compute(&[
            c("a", &["p"]),
            c("m", &["b", "p"]),
            c("b", &[]),
            c("p", &[]),
        ]);
        assert_eq!(g.lanes, 2);
        assert_eq!(
            g.rows.iter().map(|r| r.lane).collect::<Vec<_>>(),
            vec![0, 1, 1, 0]
        );
        // Lane 0 passes through, M continues to B, plus the diagonal M -> lane 0.
        assert_eq!(g.rows[1].edges, vec![e(0, 0, 0), e(1, 1, 1), e(1, 0, 0)]);
        assert_eq!(g.rows[2].edges, vec![e(0, 0, 0)]);
        assert!(g.rows[3].edges.is_empty());
    }

    #[test]
    fn empty_input() {
        let g = compute(&[]);
        assert!(g.rows.is_empty());
        assert_eq!(g.lanes, 0);
    }
}
