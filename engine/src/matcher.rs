//! Running a [`Query`] against a Glass database the way Xapian 1.4's matcher
//! weighs it: BM25 (`bm25weight.cc`), synonyms weighted as one term with
//! Xapian's term-frequency estimate (`OrPostList`), exact phrases.
//!
//! Every matching document is scored exactly.
// ponytail: no max-weight pruning or early termination, so a very common term
// on a huge archive scores every posting; add Xapian's bounds if a measured
// query is too slow.

use crate::Result;
use crate::glass::Db;
use crate::query::Query;

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
    doclens: &'a [u32],
    n: f64,
    len_factor: f64,
    bm: Bm25,
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
    fn sumpart(&self, termweight: f64, wdf: u32, did: u32) -> f64 {
        let len = self.doclens.get(did as usize).copied().unwrap_or(0) as f64;
        let normlen = (len * self.len_factor).max(self.bm.min_normlen);
        let wdf = wdf as f64;
        termweight * (wdf / (self.bm.k1 * (normlen * self.bm.b + (1.0 - self.bm.b)) + wdf))
    }
}

/// A query ready to run: terms resolved to their statistics.
enum Node {
    /// A term (or a synonym of terms): weighted as one, wdfs summed.
    Leaf {
        terms: Vec<Vec<u8>>,
        termweight: f64,
        tf: u32,
    },
    And(Vec<Node>),
    Or(Vec<Node>),
    AndNot(Box<Node>, Box<Node>),
    Phrase(Vec<Node>, u32),
    Nothing,
}

/// Xapian's OR estimate, `l + r − l·r/n`, rounded.
fn or_est(l: u32, r: u32, n: f64) -> u32 {
    let (l, r) = (l as f64, r as f64);
    (l + r - l * r / n + 0.5) as u32
}

/// The synonym's term frequency estimate: OR nodes built like a Huffman
/// tree, smallest two first (`OrContext::postlist`).
fn synonym_tf(mut tfs: Vec<u32>, n: f64) -> u32 {
    while tfs.len() > 1 {
        tfs.sort_unstable_by(|a, b| b.cmp(a));
        let r = tfs.pop().unwrap();
        let l = tfs.pop().unwrap();
        tfs.push(or_est(l, r, n));
    }
    tfs.first().copied().unwrap_or(0)
}

impl Node {
    fn build(q: &Query, ctx: &Ctx) -> Result<Node> {
        Ok(match q {
            Query::Term(t, _) => {
                let tf = ctx.db.freqs(t.as_bytes())?.0;
                if tf == 0 {
                    Node::Nothing
                } else {
                    Node::Leaf {
                        terms: vec![t.as_bytes().to_vec()],
                        termweight: ctx.termweight(tf),
                        tf,
                    }
                }
            }
            Query::Wildcard(prefix, max) => {
                let mut terms = ctx.db.terms(prefix.as_bytes())?;
                if *max > 0 && terms.len() > *max {
                    // WILDCARD_LIMIT_MOST_FREQUENT.
                    terms.sort_by_key(|t| std::cmp::Reverse(t.1));
                    terms.truncate(*max);
                    terms.sort();
                }
                if terms.is_empty() {
                    return Ok(Node::Nothing);
                }
                let tf = synonym_tf(terms.iter().map(|t| t.1).collect(), ctx.n);
                Node::Leaf {
                    termweight: ctx.termweight(tf),
                    tf,
                    terms: terms.into_iter().map(|t| t.0).collect(),
                }
            }
            Query::And(v) => Node::And(
                v.iter()
                    .map(|q| Node::build(q, ctx))
                    .collect::<Result<_>>()?,
            ),
            Query::Or(v) => Node::Or(
                v.iter()
                    .map(|q| Node::build(q, ctx))
                    .collect::<Result<_>>()?,
            ),
            Query::AndNot(a, b) => Node::AndNot(
                Box::new(Node::build(a, ctx)?),
                Box::new(Node::build(b, ctx)?),
            ),
            Query::Phrase(v, w) => {
                let v: Vec<Node> = v
                    .iter()
                    .map(|q| Node::build(q, ctx))
                    .collect::<Result<_>>()?;
                if ctx.db.has_positions() {
                    Node::Phrase(v, *w)
                } else {
                    Node::And(v) // no positions: Xapian treats a phrase as AND
                }
            }
        })
    }

    /// An estimate of how many documents match (for ordering ANDs).
    fn est(&self) -> u32 {
        match self {
            Node::Leaf { tf, .. } => *tf,
            Node::And(v) | Node::Phrase(v, _) => v.iter().map(Node::est).min().unwrap_or(0),
            Node::Or(v) => v.iter().map(Node::est).fold(0, u32::saturating_add),
            Node::AndNot(a, _) => a.est(),
            Node::Nothing => 0,
        }
    }

    /// Weighted leaves (`total_subqs`).
    fn subqs(&self) -> u32 {
        match self {
            Node::Leaf { .. } => 1,
            Node::And(v) | Node::Or(v) | Node::Phrase(v, _) => v.iter().map(Node::subqs).sum(),
            Node::AndNot(a, _) => a.subqs(),
            Node::Nothing => 0,
        }
    }

    /// The matches, by docid; only among `within` when given.
    fn eval(&self, ctx: &Ctx, within: Option<&[u32]>) -> Result<Vec<Match>> {
        match self {
            Node::Nothing => Ok(vec![]),
            Node::Leaf {
                terms, termweight, ..
            } => {
                // Sum the wdfs of every term in each document.
                let mut wdfs: Vec<(u32, u32)> = vec![];
                for t in terms {
                    let Some(mut pl) = ctx.db.postlist(t)? else {
                        continue;
                    };
                    let mut one = vec![];
                    match within {
                        None => {
                            while pl.next()? {
                                one.push((pl.did(), pl.wdf()));
                            }
                        }
                        Some(ds) => {
                            for &d in ds {
                                if !pl.skip_to(d)? {
                                    break;
                                }
                                if pl.did() == d {
                                    one.push((d, pl.wdf()));
                                }
                            }
                        }
                    }
                    wdfs = merge(wdfs, one, |a, b| a + b);
                }
                Ok(wdfs
                    .into_iter()
                    .map(|(did, wdf)| {
                        let len = ctx.doclens.get(did as usize).copied().unwrap_or(0);
                        let wdf = if terms.len() > 1 { wdf.min(len) } else { wdf };
                        Match {
                            did,
                            weight: ctx.sumpart(*termweight, wdf, did),
                            subqs: 1,
                        }
                    })
                    .collect())
            }
            Node::Or(v) => {
                let mut out: Vec<Match> = vec![];
                for n in v {
                    let m = n.eval(ctx, within)?;
                    out = merge_matches(out, m);
                }
                Ok(out)
            }
            Node::And(v) | Node::Phrase(v, _) => {
                let mut kids: Vec<&Node> = v.iter().collect();
                kids.sort_by_key(|n| n.est());
                let Some((first, rest)) = kids.split_first() else {
                    return Ok(vec![]);
                };
                let mut out = first.eval(ctx, within)?;
                for n in rest {
                    if out.is_empty() {
                        break;
                    }
                    let ds: Vec<u32> = out.iter().map(|m| m.did).collect();
                    let m = n.eval(ctx, Some(&ds))?;
                    out = intersect(out, m);
                }
                if let Node::Phrase(terms, window) = self {
                    let mut kept = vec![];
                    for m in out {
                        if phrase_at(ctx, terms, *window, m.did)? {
                            kept.push(m);
                        }
                    }
                    out = kept;
                }
                Ok(out)
            }
            Node::AndNot(a, b) => {
                let l = a.eval(ctx, within)?;
                let ds: Vec<u32> = l.iter().map(|m| m.did).collect();
                let r = b.eval(ctx, Some(&ds))?;
                let mut r = r.iter().map(|m| m.did).peekable();
                Ok(l.into_iter()
                    .filter(|m| {
                        while r.peek().is_some_and(|&d| d < m.did) {
                            r.next();
                        }
                        r.peek() != Some(&m.did)
                    })
                    .collect())
            }
        }
    }
}

/// Whether the phrase's terms occur in order within `window` positions in `did`.
fn phrase_at(ctx: &Ctx, terms: &[Node], window: u32, did: u32) -> Result<bool> {
    let mut lists = vec![];
    for t in terms {
        let Node::Leaf { terms, .. } = t else {
            return Ok(true);
        }; // not a plain term: don't filter
        lists.push(ctx.db.positions(did, &terms[0])?);
    }
    let Some((first, rest)) = lists.split_first() else {
        return Ok(false);
    };
    'start: for &p0 in first {
        let mut prev = p0;
        for (i, l) in rest.iter().enumerate() {
            let next = if window as usize == terms.len() {
                // Exact: each term right after the previous.
                let want = p0 + i as u32 + 1;
                l.binary_search(&want).ok().map(|_| want)
            } else {
                l.iter().copied().find(|&p| p > prev)
            };
            match next {
                Some(p) if p < p0 + window => prev = p,
                _ => continue 'start,
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn merge(a: Vec<(u32, u32)>, b: Vec<(u32, u32)>, f: impl Fn(u32, u32) -> u32) -> Vec<(u32, u32)> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x.0 == y.0 => {
                out.push((x.0, f(x.1, y.1)));
                a.next();
                b.next();
            }
            (Some(x), Some(y)) if x.0 < y.0 => out.push(a.next().unwrap()),
            (Some(_), Some(_)) | (None, Some(_)) => out.push(b.next().unwrap()),
            (Some(_), None) => out.push(a.next().unwrap()),
            (None, None) => return out,
        }
    }
}

fn merge_matches(a: Vec<Match>, b: Vec<Match>) -> Vec<Match> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x.did == y.did => {
                out.push(Match {
                    did: x.did,
                    weight: x.weight + y.weight,
                    subqs: x.subqs + y.subqs,
                });
                a.next();
                b.next();
            }
            (Some(x), Some(y)) if x.did < y.did => out.push(a.next().unwrap()),
            (Some(_), Some(_)) | (None, Some(_)) => out.push(b.next().unwrap()),
            (Some(_), None) => out.push(a.next().unwrap()),
            (None, None) => return out,
        }
    }
}

/// `a` ∩ `b` (b ⊆ a's docids), weights summed.
fn intersect(a: Vec<Match>, b: Vec<Match>) -> Vec<Match> {
    let mut b = b.into_iter().peekable();
    let mut out = vec![];
    for x in a {
        while b.peek().is_some_and(|y| y.did < x.did) {
            b.next();
        }
        if let Some(y) = b.peek()
            && y.did == x.did
        {
            out.push(Match {
                did: x.did,
                weight: x.weight + y.weight,
                subqs: x.subqs + y.subqs,
            });
        }
    }
    out
}

/// Every document matching `q`, by docid, and the query's weighted leaf count.
pub fn matches(db: &Db, q: &Query, bm: Bm25) -> Result<(Vec<Match>, u32)> {
    let doclens = db.doclen_table()?;
    let n = db.stats.doccount as f64;
    let avg = if n > 0.0 {
        db.stats.total_doclen as f64 / n
    } else {
        0.0
    };
    let ctx = Ctx {
        db,
        doclens,
        n,
        len_factor: if avg > 0.0 { 1.0 / avg } else { 0.0 },
        bm,
    };
    let node = Node::build(q, &ctx)?;
    Ok((node.eval(&ctx, None)?, node.subqs()))
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
