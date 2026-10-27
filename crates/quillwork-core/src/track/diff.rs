//! Word-level diff (Myers) for readable redlines.
//!
//! Tokens: runs of letters, runs of digits (a digit/letter boundary splits, so `1ste` is `1` +
//! `ste`), runs of whitespace, and single other characters. Hunks separated only by whitespace are
//! joined, so a rewritten phrase reads as one deletion and one insertion. When more than 60% of
//! the tokens change, the whole changed middle becomes a single hunk.

use std::ops::Range;

/// One change: old bytes `old` become new bytes `new` (either may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
    /// Token index ranges of the same change.
    pub old_tokens: Range<usize>,
    pub new_tokens: Range<usize>,
}

/// Above this share of changed tokens the diff is one delete + one insert.
pub const REWRITE_RATIO: f64 = 0.6;
/// Beyond this many tokens (old + new) the diff falls back to prefix/suffix trimming.
const MAX_TOKENS: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Alpha,
    Digit,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_numeric() {
        Class::Digit
    } else if c.is_alphabetic() || c == '\u{AD}' || c == '\u{2011}' {
        Class::Alpha
    } else {
        Class::Other
    }
}

/// Byte ranges of the tokens of `s`.
pub fn tokenize(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev: Option<Class> = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        match (prev, out.last_mut()) {
            (Some(p), Some(last)) if p == k && k != Class::Other => last.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        prev = Some(k);
    }
    out
}

/// Word-level hunks turning `old` into `new`, in order.
pub fn diff(old: &str, new: &str) -> Vec<Hunk> {
    let ot = tokenize(old);
    let nt = tokenize(new);
    let os: Vec<&str> = ot.iter().map(|r| &old[r.clone()]).collect();
    let ns: Vec<&str> = nt.iter().map(|r| &new[r.clone()]).collect();

    // Trim the common prefix and suffix first: cheap and keeps Myers small.
    let mut pre = 0;
    while pre < os.len() && pre < ns.len() && os[pre] == ns[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < os.len() - pre && suf < ns.len() - pre && os[os.len() - 1 - suf] == ns[ns.len() - 1 - suf] {
        suf += 1;
    }
    let (oa, ob) = (pre, os.len() - suf);
    let (na, nb) = (pre, ns.len() - suf);
    if oa == ob && na == nb {
        return Vec::new();
    }

    // Token-index hunks within the trimmed middle.
    let mut hunks: Vec<(Range<usize>, Range<usize>)> = if (ob - oa) + (nb - na) > MAX_TOKENS {
        vec![(oa..ob, na..nb)]
    } else {
        myers(&os[oa..ob], &ns[na..nb])
            .into_iter()
            .map(|(o, n)| (o.start + oa..o.end + oa, n.start + na..n.end + na))
            .collect()
    };

    // Join hunks separated only by whitespace tokens.
    let mut joined: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for h in hunks.drain(..) {
        if let Some(last) = joined.last_mut() {
            let gap_old = last.0.end..h.0.start;
            let all_space = gap_old.clone().all(|i| os[i].chars().all(char::is_whitespace));
            if all_space && gap_old.len() == h.1.start - last.1.end {
                last.0.end = h.0.end;
                last.1.end = h.1.end;
                continue;
            }
        }
        joined.push(h);
    }

    // Heavy rewrite: one hunk.
    let changed: usize = joined.iter().map(|(o, n)| o.len() + n.len()).sum();
    let total = os.len() + ns.len();
    if joined.len() > 1 && total > 0 && changed as f64 / total as f64 > REWRITE_RATIO {
        let first = joined.first().cloned().expect("non-empty");
        let last = joined.last().cloned().expect("non-empty");
        joined = vec![(first.0.start..last.0.end, first.1.start..last.1.end)];
    }

    let byte = |toks: &[Range<usize>], r: &Range<usize>, len: usize| -> Range<usize> {
        let s = toks.get(r.start).map_or(len, |t| t.start);
        let s = if r.start == toks.len() { len } else { s };
        let e = if r.end == 0 { 0 } else { toks[r.end - 1].end };
        if r.is_empty() { s..s } else { s..e }
    };
    joined
        .into_iter()
        .map(|(o, n)| {
            let mut ob = byte(&ot, &o, old.len());
            let mut nb_ = byte(&nt, &n, new.len());
            // Pure insertions/deletions sit at the token boundary of the other side.
            if o.is_empty() {
                let at = if o.start == 0 { 0 } else { ot[o.start - 1].end };
                ob = at..at;
            }
            if n.is_empty() {
                let at = if n.start == 0 { 0 } else { nt[n.start - 1].end };
                nb_ = at..at;
            }
            Hunk { old: ob, new: nb_, old_tokens: o, new_tokens: n }
        })
        .collect()
}

/// Myers O((N+M)D) diff over tokens; returns maximal (old, new) change ranges.
fn myers(a: &[&str], b: &[&str]) -> Vec<(Range<usize>, Range<usize>)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = (n + m) as usize;
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) { v[idx + 1] } else { v[idx - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                trace.push(v.clone());
                break 'outer;
            }
            k += 2;
        }
    }
    // Backtrack into an edit script of (equal | del | ins) steps.
    #[derive(Clone, Copy, PartialEq)]
    enum E {
        Eq,
        Del,
        Ins,
    }
    let mut script: Vec<E> = Vec::new();
    let (mut x, mut y) = (n, m);
    // trace[d] holds v before round d; the final v is the last entry.
    let rounds = trace.len() - 1;
    for d in (1..rounds as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let idx = (k + off) as usize;
        let prev_k = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            script.push(E::Eq);
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            script.push(E::Ins);
        } else {
            script.push(E::Del);
        }
        x = prev_x;
        y = prev_y;
    }
    while x > 0 && y > 0 {
        script.push(E::Eq);
        x -= 1;
        y -= 1;
    }
    script.reverse();

    let mut out: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut cur: Option<(Range<usize>, Range<usize>)> = None;
    for e in script {
        match e {
            E::Eq => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                i += 1;
                j += 1;
            }
            E::Del => {
                let c = cur.get_or_insert((i..i, j..j));
                c.0.end = i + 1;
                i += 1;
            }
            E::Ins => {
                let c = cur.get_or_insert((i..i, j..j));
                c.1.end = j + 1;
                j += 1;
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, new: &str, hunks: &[Hunk]) -> String {
        let mut out = String::new();
        let mut at = 0;
        for h in hunks {
            out.push_str(&old[at..h.old.start]);
            out.push_str(&new[h.new.clone()]);
            at = h.old.end;
        }
        out.push_str(&old[at..]);
        out
    }

    #[test]
    fn tokens() {
        let s = "de 1ste week, 38uur";
        let t: Vec<&str> = tokenize(s).into_iter().map(|r| &s[r]).collect();
        assert_eq!(t, ["de", " ", "1", "ste", " ", "week", ",", " ", "38", "uur"]);
    }

    #[test]
    fn word_level() {
        let old = "voor 40 uur per week";
        let new = "voor 38 uur per week";
        let h = diff(old, new);
        assert_eq!(h.len(), 1);
        assert_eq!(&old[h[0].old.clone()], "40");
        assert_eq!(&new[h[0].new.clone()], "38");
    }

    #[test]
    fn joins_across_spaces_and_rewrites() {
        let old = "a b c d e f g h";
        let new = "a x y d e f g h";
        let h = diff(old, new);
        assert_eq!(h.len(), 1);
        assert_eq!(&old[h[0].old.clone()], "b c");
        let h = diff("one two three four", "alpha beta gamma four");
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn roundtrip_random() {
        let cases = [
            ("", "abc"),
            ("abc", ""),
            ("Werknemer treedt op in dienst", "Werkgever treedt per direct in dienst"),
            ("1ste lid", "2de lid"),
            ("same", "same"),
            ("a, b, c", "a; b; c; d"),
            ("x y z", "y z x"),
        ];
        for (o, n) in cases {
            let h = diff(o, n);
            assert_eq!(apply(o, n, &h), n, "{o:?} -> {n:?}: {h:?}");
        }
    }
}
