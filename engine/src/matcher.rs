// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! Running a [`Query`] against a Glass database the way Xapian 1.4's matcher
//! weighs it: BM25 (`bm25weight.cc`), synonyms weighted as one term with
//! Xapian's term-frequency estimate (`OrPostList`), exact phrases.
//!
//! Every matching document is scored exactly.
// ponytail: no max-weight pruning or early termination, so a very common term
// on a huge archive scores every posting; add Xapian's bounds if a measured
// query is too slow.

use crate::glass::Db;
use crate::glass::db::{PostList, PostingBudget};
use crate::query::Query;
use crate::{Error, Result};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// BM25 parameters (k1, k2 is always 0, k3, b, min_normlen).
#[derive(Clone, Copy)]
pub struct Bm25 {
    pub k1: f64,
    pub k3: f64,
    pub b: f64,
    pub min_normlen: f64,
}

/// Xapian's default, which libzim's full-text search uses.
pub const FULLTEXT: Bm25 = Bm25 {
    k1: 1.0,
    k3: 1.0,
    b: 0.5,
    min_normlen: 0.5,
};

/// libzim's suggestion weighting: `BM25Weight(0.001, 0, 1, 1, 0.5)`.
pub const SUGGEST: Bm25 = Bm25 {
    k1: 0.001,
    k3: 1.0,
    b: 1.0,
    min_normlen: 0.5,
};

/// A matching document: docid, weight, and how many weighted leaves matched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    pub did: u32,
    pub weight: f64,
    pub subqs: u32,
}

struct Ctx<'a> {
    db: &'a Db,
    cancelled: &'a dyn Fn() -> bool,
    n: f64,
    len_factor: f64,
    bm: Bm25,
    posting_budget: PostingBudget,
}

impl Ctx<'_> {
    /// `BM25Weight::init` for a term with frequency `tf` (wqf 1, factor 1).
    fn termweight(&self, tf: u32) -> f64 {
        let mut tw = (self.n - tf as f64 + 0.5) / (tf as f64 + 0.5);
        if tw < 2.0 {
            tw = tw * 0.5 + 1.0;
        }
        let wqf = 1.0;
        tw.ln() * (self.bm.k3 + 1.0) * wqf / (self.bm.k3 + wqf) * (self.bm.k1 + 1.0)
    }

    /// `BM25Weight::get_sumpart`.
    fn sumpart(&self, termweight: f64, wdf: u32, doclen: u32) -> f64 {
        let len = f64::from(doclen);
        let normlen = (len * self.len_factor).max(self.bm.min_normlen);
        let wdf = wdf as f64;
        termweight * (wdf / (self.bm.k1 * (normlen * self.bm.b + (1.0 - self.bm.b)) + wdf))
    }
}

/// Each branch owns forward-only cursors. A cached docid of 0 means it
/// has not yet been sought; None means the branch is exhausted.
struct Node<'a> {
    branch: Branch<'a>,
    did: Option<u32>,
    estimate: u32,
    subqs: u32,
}

enum Branch<'a> {
    Leaf {
        postings: Vec<PostList<'a>>,
        termweight: f64,
    },
    And {
        children: Vec<Node<'a>>,
        phrase: Option<(Vec<&'a str>, u32)>,
    },
    Or(Vec<Node<'a>>),
    AndNot(Box<Node<'a>>, Box<Node<'a>>),
    Nothing,
}

/// Xapian's OR estimate, `l + r − l·r/n`, rounded.
fn or_est(l: u32, r: u32, n: f64) -> u32 {
    let (l, r) = (l as f64, r as f64);
    (l + r - l * r / n + 0.5) as u32
}

/// The synonym's term frequency estimate: OR nodes built like a Huffman
/// tree, smallest two first (`OrContext::postlist`).
fn synonym_tf(tfs: Vec<u32>, n: f64) -> u32 {
    let mut heap: BinaryHeap<_> = tfs.into_iter().map(Reverse).collect();
    while heap.len() > 1 {
        let Reverse(left) = heap.pop().unwrap();
        let Reverse(right) = heap.pop().unwrap();
        heap.push(Reverse(or_est(left, right, n)));
    }
    heap.pop().map_or(0, |Reverse(tf)| tf)
}

impl<'a> Node<'a> {
    fn build(q: &'a Query, ctx: &'a Ctx<'a>, remaining_terms: &mut usize) -> Result<Self> {
        crate::check_cancel(ctx.cancelled)?;
        let term_limit = || {
            Error::Invalid(format!(
                "expanded query exceeds the {} term limit",
                ctx.db.limits().max_query_terms
            ))
        };
        let (branch, estimate, subqs) = match q {
            Query::Term(term, _) => {
                *remaining_terms = remaining_terms.checked_sub(1).ok_or_else(term_limit)?;
                match ctx
                    .db
                    .budgeted_postlist(term.as_bytes(), &ctx.posting_budget)?
                {
                    Some(pl) => {
                        let tf = pl.tf;
                        (
                            Branch::Leaf {
                                postings: vec![pl],
                                termweight: ctx.termweight(tf),
                            },
                            tf,
                            1,
                        )
                    }
                    None => (Branch::Nothing, 0, 0),
                }
            }
            Query::Wildcard(prefix, max) => {
                let mut heap = BinaryHeap::new();
                ctx.db
                    .visit_terms(prefix.as_bytes(), ctx.cancelled, |term, tf| {
                        // At the root: lowest frequency, then largest lexical
                        // term. Equal frequencies retain the first terms in the scan.
                        if *max == 0 || heap.len() < *max {
                            if heap.len() == *remaining_terms {
                                return Err(term_limit());
                            }
                            heap.push((Reverse(tf), term));
                        } else if heap.peek().is_some_and(|(Reverse(worst), _)| tf > *worst) {
                            heap.pop();
                            heap.push((Reverse(tf), term));
                        }
                        Ok(())
                    })?;
                *remaining_terms -= heap.len();
                let terms: Vec<_> = heap
                    .into_iter()
                    .map(|(Reverse(tf), term)| (term, tf))
                    .collect();
                let tf = synonym_tf(terms.iter().map(|t| t.1).collect(), ctx.n);
                let mut postings = Vec::new();
                for (term, _) in terms {
                    if let Some(pl) = ctx.db.budgeted_postlist(&term, &ctx.posting_budget)? {
                        postings.push(pl);
                    }
                }
                if postings.is_empty() {
                    (Branch::Nothing, 0, 0)
                } else {
                    (
                        Branch::Leaf {
                            postings,
                            termweight: ctx.termweight(tf),
                        },
                        tf,
                        1,
                    )
                }
            }
            Query::And(v) | Query::Phrase(v, _) | Query::Or(v) => {
                let mut children = v
                    .iter()
                    .map(|q| Self::build(q, ctx, remaining_terms))
                    .collect::<Result<Vec<_>>>()?;
                let subqs = children.iter().map(|c| c.subqs).sum();
                if matches!(q, Query::Or(_)) {
                    let estimate = children
                        .iter()
                        .map(|c| c.estimate)
                        .fold(0, u32::saturating_add);
                    (Branch::Or(children), estimate, subqs)
                } else {
                    let phrase = if let Query::Phrase(_, window) = q
                        && ctx.db.has_positions()
                    {
                        v.iter()
                            .map(|q| match q {
                                Query::Term(term, _) => Some(term.as_str()),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                            .map(|terms| (terms, *window))
                    } else {
                        None
                    };
                    // The intersection jumps ahead with its rarest list first.
                    // Use the same order for weight addition as Xapian parity fixtures.
                    children.sort_by_key(|c| c.estimate);
                    let estimate = children.first().map_or(0, |c| c.estimate);
                    (Branch::And { children, phrase }, estimate, subqs)
                }
            }
            Query::AndNot(a, b) => {
                let a = Self::build(a, ctx, remaining_terms)?;
                let (estimate, subqs) = (a.estimate, a.subqs);
                (
                    Branch::AndNot(Box::new(a), Box::new(Self::build(b, ctx, remaining_terms)?)),
                    estimate,
                    subqs,
                )
            }
        };
        Ok(Self {
            branch,
            did: Some(0),
            estimate,
            subqs,
        })
    }

    /// First matching docid at or after target.
    fn seek(&mut self, target: u32, ctx: &Ctx) -> Result<Option<u32>> {
        if self.did.is_none_or(|d| d >= target) {
            return Ok(self.did);
        }
        self.did = match &mut self.branch {
            Branch::Nothing => None,
            Branch::Leaf { postings, .. } => {
                let mut first: Option<u32> = None;
                for pl in postings {
                    if pl.skip_to_checked(target, ctx.cancelled)? {
                        first = Some(first.map_or(pl.did(), |d| d.min(pl.did())));
                    }
                }
                first
            }
            Branch::Or(children) => {
                let mut first: Option<u32> = None;
                for child in children {
                    if let Some(d) = child.seek(target, ctx)? {
                        first = Some(first.map_or(d, |prev| prev.min(d)));
                    }
                }
                first
            }
            Branch::And { children, phrase } => {
                let mut candidate = target;
                'intersect: loop {
                    crate::check_cancel(ctx.cancelled)?;
                    if children.is_empty() {
                        break None;
                    }
                    for (i, child) in children.iter_mut().enumerate() {
                        let Some(did) = child.seek(candidate, ctx)? else {
                            break 'intersect None;
                        };
                        if did > candidate {
                            candidate = did;
                            if i != 0 {
                                continue 'intersect;
                            }
                        }
                    }
                    if let Some((terms, window)) = phrase
                        && !phrase_at(ctx, terms, *window, candidate)?
                    {
                        let Some(next) = candidate.checked_add(1) else {
                            break None;
                        };
                        candidate = next;
                        continue;
                    }
                    break Some(candidate);
                }
            }
            Branch::AndNot(left, right) => {
                let mut candidate = target;
                loop {
                    crate::check_cancel(ctx.cancelled)?;
                    let Some(did) = left.seek(candidate, ctx)? else {
                        break None;
                    };
                    if right.seek(did, ctx)? != Some(did) {
                        break Some(did);
                    }
                    let Some(next) = did.checked_add(1) else {
                        break None;
                    };
                    candidate = next;
                }
            }
        };
        Ok(self.did)
    }

    /// Score only docs accepted by the whole query. The doclength cursor
    /// therefore advances exactly once in increasing docid order.
    fn score(&self, ctx: &Ctx, did: u32, doclen: u32) -> (f64, u32) {
        match &self.branch {
            Branch::Leaf {
                postings,
                termweight,
            } => {
                let sum: u64 = postings
                    .iter()
                    .filter(|p| p.did() == did)
                    .map(|p| u64::from(p.wdf()))
                    .sum();
                let wdf = if postings.len() > 1 {
                    sum.min(u64::from(doclen))
                } else {
                    sum
                };
                (ctx.sumpart(*termweight, wdf as u32, doclen), 1)
            }
            Branch::And { children, .. } | Branch::Or(children) => children
                .iter()
                .filter(|c| c.did == Some(did))
                .map(|c| c.score(ctx, did, doclen))
                .fold((0.0, 0), |(aw, ac), (bw, bc)| (aw + bw, ac + bc)),
            Branch::AndNot(left, _) => left.score(ctx, did, doclen),
            Branch::Nothing => (0.0, 0),
        }
    }
}

fn phrase_at(ctx: &Ctx, terms: &[&str], window: u32, did: u32) -> Result<bool> {
    let db = ctx.db;
    let Some((first, rest)) = terms.split_first() else {
        return Ok(false);
    };
    // Keep only potential phrase starts and their earliest possible end.
    // Both are nondecreasing, so each subsequent position list needs one pass.
    let starts = db.positions_checked(did, first.as_bytes(), ctx.cancelled)?;
    let mut spans = Vec::with_capacity(starts.len());
    for (i, start) in starts.into_iter().enumerate() {
        if i.is_multiple_of(256) {
            crate::check_cancel(ctx.cancelled)?;
        }
        spans.push((start, start));
    }
    for term in rest {
        if spans.is_empty() {
            return Ok(false);
        }
        let positions = db.positions_checked(did, term.as_bytes(), ctx.cancelled)?;
        let mut ix = 0;
        let mut kept = 0;
        for i in 0..spans.len() {
            if i.is_multiple_of(256) {
                crate::check_cancel(ctx.cancelled)?;
            }
            let (start, end) = spans[i];
            while ix < positions.len() && positions[ix] <= end {
                if ix.is_multiple_of(256) {
                    crate::check_cancel(ctx.cancelled)?;
                }
                ix += 1;
            }
            if let Some(&next) = positions.get(ix)
                && next - start < window
            {
                spans[kept] = (start, next);
                kept += 1;
            }
        }
        spans.truncate(kept);
    }
    Ok(!spans.is_empty())
}

/// Visit matches by docid; intermediate branches retain cursors, never a
/// collection of every matching document. Return the query's leaf count.
pub fn for_each_match(
    db: &Db,
    q: &Query,
    bm: Bm25,
    cancelled: &dyn Fn() -> bool,
    mut visit: impl FnMut(Match) -> Result<()>,
) -> Result<u32> {
    let n = f64::from(db.stats.doccount);
    let avg = if n > 0.0 {
        db.stats.total_doclen as f64 / n
    } else {
        0.0
    };
    let ctx = Ctx {
        db,
        cancelled,
        n,
        len_factor: if avg > 0.0 { 1.0 / avg } else { 0.0 },
        bm,
        posting_budget: PostingBudget::new(db.limits().max_posting_bytes),
    };
    let mut remaining_terms = db.limits().max_query_terms;
    let mut node = Node::build(q, &ctx, &mut remaining_terms)?;
    let Some(mut did) = node.seek(1, &ctx)? else {
        return Ok(node.subqs);
    };
    let mut lens = db
        .budgeted_postlist(b"", &ctx.posting_budget)?
        .ok_or_else(|| Error::Format(format!("missing Glass document lengths for docid {did}")))?;
    loop {
        crate::check_cancel(cancelled)?;
        if !lens.skip_to_checked(did, cancelled)? || lens.did() != did {
            return Err(Error::Format(format!(
                "missing Glass document length for docid {did}"
            )));
        }
        let (weight, subqs) = node.score(&ctx, did, lens.wdf());
        visit(Match { did, weight, subqs })?;
        let Some(target) = did.checked_add(1) else {
            break;
        };
        let Some(next) = node.seek(target, &ctx)? else {
            break;
        };
        did = next;
    }
    Ok(node.subqs)
}

/// Xapian's percentages: the best document's share of matched leaves,
/// scaled by weight (`convert_to_percent_internal`).
pub fn percent(weight: f64, best: &Match, total_subqs: u32) -> u8 {
    if best.weight <= 0.0 || total_subqs == 0 {
        return 100;
    }
    let factor = best.subqs as f64 / total_subqs as f64 / best.weight * 100.0;
    let p = (weight * factor + 100.0 * f64::EPSILON) as i64;
    p.clamp(if weight > 0.0 { 1 } else { 0 }, 100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates() {
        assert_eq!(or_est(10, 10, 100.0), 19);
        assert_eq!(synonym_tf(vec![5], 100.0), 5);
        assert_eq!(synonym_tf(vec![1, 2, 3], 1000.0), 6);
    }
}
