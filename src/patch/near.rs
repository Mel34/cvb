//! Finds where a hunk that matches nothing was probably meant to go.
//!
//! An LLM often quotes the lines around a change slightly wrong: a line from
//! memory, a renamed identifier, a context line left out or one invented. The
//! tools then fail with "Failed to find expected lines". This module ranks the
//! regions of the file that look most like what the hunk expects, so a person can
//! choose one. It only proposes; `reanchor` turns a choice into a corrected hunk.
//!
//! A candidate is a window of the file lined up with the hunk's old side (its
//! context and removed lines). The window may be one line longer (the hunk left a
//! line out) or one line shorter (the hunk quotes a line that is not there).

use std::collections::HashMap;

use crate::patch::hunks::{Hunk, LineKind};

/// A file longer than this gets no suggestions (the search would be slow).
pub(crate) const MAX_FILE_LINES: usize = 20_000;
/// A hunk with a longer old side gets no suggestions.
pub(crate) const MAX_HUNK_LINES: usize = 200;
/// At most this many candidates are offered.
pub(crate) const MAX_CANDIDATES: usize = 5;
/// Windows examined per hunk, at most.
const MAX_STARTS: usize = 2_000;
/// A line this similar (after trimming) to the expected one counts as a match.
const MATCH_RATIO: f64 = 0.6;
/// Lines are compared on their first this-many characters.
const LONG_LINE: usize = 300;

/// How one old-side line relates to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pair {
    /// Old-side line `old` (counting context and removed lines) lines up with
    /// file line `line` (0-based).
    Both { old: usize, line: usize },
    /// A file line the hunk does not mention; it is kept as context.
    Extra { line: usize },
    /// An old-side line with no counterpart in the file; it is dropped.
    Missing { old: usize },
}

/// A place in the file the hunk could be corrected to fit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    /// First file line of the window (0-based).
    pub start: usize,
    /// Number of file lines in the window.
    pub len: usize,
    pub pairs: Vec<Pair>,
    /// 0..=1, how much of the old side the window reproduces (exact lines count
    /// fully, near misses half).
    pub score: f64,
    /// A removed line differs (the real line would be deleted instead) or is
    /// missing from the file (that removal would be dropped): applying this
    /// changes what is deleted from what the patch named. Shown with a warning.
    pub loose: bool,
    /// Lines that differ, are extra or are missing.
    pub changed: usize,
}

/// Why no suggestions are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    FileTooLong,
    HunkTooLong,
    /// A hunk needs at least two old-side lines to be placed by similarity.
    TooShort,
}

impl Refusal {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Refusal::FileTooLong => "the file is too long to search for similar text",
            Refusal::HunkTooLong => "the hunk is too long to search for similar text",
            Refusal::TooShort => "the hunk has too few lines to place by similarity",
        }
    }
}

/// 1.0 when the lines are equal ignoring surrounding white space, otherwise the
/// edit-distance similarity of the trimmed lines.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (a.trim(), b.trim());
    if a == b {
        return 1.0;
    }
    let a: Vec<char> = a.chars().take(LONG_LINE).collect();
    let b: Vec<char> = b.chars().take(LONG_LINE).collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != cb));
            diagonal = above;
        }
    }
    1.0 - row[b.len()] as f64 / a.len().max(b.len()) as f64
}

/// What a pair is worth when ranking windows: an exact line counts fully, a near
/// miss half, so a window with more exact lines beats one with more near misses.
fn value(similarity: f64) -> f64 {
    if similarity >= 1.0 { 1.0 } else { similarity * 0.5 }
}

struct Sims<'a> {
    old: &'a [&'a str],
    file: &'a [&'a str],
    cache: HashMap<(usize, usize), f64>,
}

impl Sims<'_> {
    fn get(&mut self, old: usize, line: usize) -> f64 {
        let (old_text, file_text) = (self.old[old], self.file[line]);
        *self
            .cache
            .entry((old, line))
            .or_insert_with(|| similarity(old_text, file_text))
    }
}

/// The best way to line the old side up with the file from line `start`, if any
/// window there is worth offering.
fn best_at(start: usize, kinds: &[LineKind], sims: &mut Sims) -> Option<Candidate> {
    let n = kinds.len();
    let file_len = sims.file.len();
    // Prefix sums of the similarity of old line i against file line start+i
    // (a, same position), start+i+1 (b, a file line inserted before it) and
    // start+i-1 (c, an old line absent before it).
    let mut a = vec![0.0; n + 1];
    let mut b = vec![0.0; n + 1];
    let mut c = vec![0.0; n + 1];
    for i in 0..n {
        let at = start + i;
        a[i + 1] = a[i] + if at < file_len { value(sims.get(i, at)) } else { 0.0 };
        b[i + 1] = b[i] + if at + 1 < file_len { value(sims.get(i, at + 1)) } else { 0.0 };
        c[i + 1] = c[i] + if i >= 1 && at >= 1 && at - 1 < file_len { value(sims.get(i, at - 1)) } else { 0.0 };
    }
    // (total similarity, window length, shape): equal, extra at e, missing m.
    #[derive(Clone, Copy)]
    enum Shape {
        Equal,
        Extra(usize),
        Missing(usize),
    }
    let mut best: Option<(f64, usize, Shape)> = None;
    let mut consider = |score: f64, len: usize, shape: Shape| {
        let better = best.is_none_or(|(s, _, _)| score > s + 1e-9);
        if better {
            best = Some((score, len, shape));
        }
    };
    if start + n <= file_len {
        consider(a[n] / n as f64, n, Shape::Equal);
    }
    if n >= 2 && start + n < file_len {
        for e in 1..n {
            let total = a[e] + (b[n] - b[e]);
            consider(total / (n + 1) as f64, n + 1, Shape::Extra(e));
        }
    }
    if n >= 2 && start + n - 1 <= file_len {
        for m in 0..n {
            let total = a[m] + (c[n] - c[m + 1]);
            consider(total / n as f64, n - 1, Shape::Missing(m));
        }
    }
    let (score, len, shape) = best?;

    let mut pairs = Vec::with_capacity(n + 1);
    match shape {
        Shape::Equal => {
            pairs.extend((0..n).map(|old| Pair::Both { old, line: start + old }));
        }
        Shape::Extra(e) => {
            for old in 0..n {
                if old == e {
                    pairs.push(Pair::Extra { line: start + e });
                }
                let line = if old < e { start + old } else { start + old + 1 };
                pairs.push(Pair::Both { old, line });
            }
        }
        Shape::Missing(m) => {
            for old in 0..n {
                if old < m {
                    pairs.push(Pair::Both { old, line: start + old });
                } else if old == m {
                    pairs.push(Pair::Missing { old });
                } else {
                    pairs.push(Pair::Both { old, line: start + old - 1 });
                }
            }
        }
    }

    let (mut matched, mut exact, mut changed, mut loose) = (0usize, 0usize, 0usize, false);
    for pair in &pairs {
        match *pair {
            Pair::Both { old, line } => {
                let s = sims.get(old, line);
                if s >= 1.0 {
                    // A blank line or a lone brace is in every function: it ranks
                    // the window but does not count as evidence of the place.
                    if sims.old[old].chars().any(char::is_alphanumeric) {
                        exact += 1;
                        matched += 1;
                    }
                } else {
                    changed += 1;
                    loose |= kinds[old] == LineKind::Remove;
                    if s > MATCH_RATIO {
                        matched += 1;
                    }
                }
            }
            Pair::Extra { .. } => changed += 1,
            Pair::Missing { old } => {
                changed += 1;
                loose |= kinds[old] == LineKind::Remove;
            }
        }
    }
    if exact == 0 || matched * 2 < n {
        return None;
    }
    Some(Candidate { start, len, pairs, score, loose, changed })
}

/// Up to [`MAX_CANDIDATES`] places in `file` where `hunk` could belong, best
/// first. Candidates that only change context lines come first; ones that change
/// which line is removed are offered only when there is no other kind.
pub(crate) fn candidates(hunk: &Hunk, file: &[&str]) -> Result<Vec<Candidate>, Refusal> {
    candidates_counted(hunk, file).map(|(found, _)| found)
}

/// What the offered list leaves out, so a reader is not led to think it is complete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NotShown {
    /// Candidates beyond [`MAX_CANDIDATES`] that were dropped.
    pub capped: usize,
    /// Loose candidates not offered because strict ones exist.
    pub loose_hidden: usize,
}

/// [`candidates`], with what it left out.
pub(crate) fn candidates_counted(
    hunk: &Hunk,
    file: &[&str],
) -> Result<(Vec<Candidate>, NotShown), Refusal> {
    let old = hunk.old_side();
    if old.len() > MAX_HUNK_LINES {
        return Err(Refusal::HunkTooLong);
    }
    if file.len() > MAX_FILE_LINES {
        return Err(Refusal::FileTooLong);
    }
    if old.len() < 2 {
        return Err(Refusal::TooShort);
    }
    let kinds: Vec<LineKind> = old.iter().map(|(k, _)| *k).collect();
    let texts: Vec<&str> = old.iter().map(|(_, t)| *t).collect();
    // Windows are worth examining only where some old line is found, ignoring
    // white space, at its place or one line either side.
    let mut by_text: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, line) in file.iter().enumerate() {
        by_text.entry(line.trim()).or_default().push(index);
    }
    // A start is supported once for every old line found at its place or one line
    // either side. When there are more starts than `MAX_STARTS`, the best
    // supported are scored; position in the file must not decide which are
    // dropped, or a match late in the file would lose to earlier look-alikes.
    let mut support: HashMap<usize, usize> = HashMap::new();
    for (i, text) in texts.iter().enumerate() {
        if let Some(positions) = by_text.get(text.trim()) {
            for &at in positions {
                for shift in 0..=1usize {
                    if at >= i + shift {
                        *support.entry(at - i - shift).or_default() += 1;
                    }
                }
            }
        }
    }
    let mut starts: Vec<usize> = support.keys().copied().collect();
    starts.sort_unstable_by(|a, b| support[b].cmp(&support[a]).then(a.cmp(b)));
    starts.truncate(MAX_STARTS);

    let mut sims = Sims { old: &texts, file, cache: HashMap::new() };
    let mut found: Vec<Candidate> = starts
        .into_iter()
        .filter_map(|start| best_at(start, &kinds, &mut sims))
        .collect();
    found.sort_by(|x, y| y.score.total_cmp(&x.score).then(x.start.cmp(&y.start)));

    // Strict and loose candidates are kept apart *before* overlapping windows are
    // dropped: a loose window that scores higher must not remove an overlapping
    // strict one, or the strict place would never be offered.
    let (strict, loose): (Vec<Candidate>, Vec<Candidate>) = found.into_iter().partition(|c| !c.loose);
    let strict = without_overlaps(strict);
    let loose_hidden = if strict.is_empty() { 0 } else { without_overlaps(loose.clone()).len() };
    let mut result = if strict.is_empty() { without_overlaps(loose) } else { strict };
    if let Some(hint) = hunk.old_start {
        let distance = |c: &Candidate| (c.start + 1).abs_diff(hint);
        result.sort_by(|x, y| y.score.total_cmp(&x.score).then(distance(x).cmp(&distance(y))));
    }
    let capped = result.len().saturating_sub(MAX_CANDIDATES);
    result.truncate(MAX_CANDIDATES);
    Ok((result, NotShown { capped, loose_hidden }))
}

/// Best first (the input is already ordered); a window that shares a line with a
/// better one is dropped.
fn without_overlaps(sorted: Vec<Candidate>) -> Vec<Candidate> {
    let mut chosen: Vec<Candidate> = Vec::new();
    for candidate in sorted {
        let end = candidate.start + candidate.len;
        if chosen.iter().all(|c| end <= c.start || c.start + c.len <= candidate.start) {
            chosen.push(candidate);
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::hunks::read;
    use crate::patch::parse::Format;

    fn hunk(body: &str) -> Hunk {
        let text = format!("*** Begin Patch\n*** Update File: f\n@@\n{body}*** End Patch\n");
        read(&text, Format::ApplyPatch).remove(0).hunks.remove(0)
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    const FILE: &str = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    run(a, b, c);\n}\n";

    #[test]
    fn similarity_is_exact_ignoring_surrounding_white_space() {
        assert_eq!(similarity("  x = 1;", "x = 1;  "), 1.0);
        assert!(similarity("let total = 1;", "let totals = 1;") > 0.9);
        assert!(similarity("alpha", "omega") < MATCH_RATIO);
        assert_eq!(similarity("", "x"), 0.0);
        assert_eq!(similarity("", "  "), 1.0);
    }

    #[test]
    fn a_misquoted_context_line_gives_one_strict_candidate() {
        let h = hunk(" fn main() {\n     let a = 1;\n-    let b = 22;\n+    let b = 5;\n     let c = 3;\n");
        // `let b = 22;` is quoted wrong, and it is a removed line: loose.
        let found = candidates(&h, &lines(FILE)).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].loose);
        assert_eq!(found[0].start, 0);
        assert_eq!(found[0].len, 4);
        assert_eq!(found[0].changed, 1);

        let h = hunk(" fn main() {\n     let a = 11;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let found = candidates(&h, &lines(FILE)).unwrap();
        assert_eq!(found.len(), 1);
        assert!(!found[0].loose, "only a context line differs");
        assert_eq!(found[0].changed, 1);
        assert_eq!(
            found[0].pairs[1],
            Pair::Both { old: 1, line: 1 }
        );
    }

    #[test]
    fn a_context_line_the_hunk_left_out_is_an_extra_file_line() {
        // The file has `let b` between `let a` and `let c`; the hunk skips it.
        let h = hunk(" fn main() {\n     let a = 1;\n-    let c = 3;\n+    let c = 4;\n     run(a, b, c);\n");
        let found = candidates(&h, &lines(FILE)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let c = &found[0];
        assert_eq!(c.len, 5);
        assert_eq!(
            c.pairs,
            vec![
                Pair::Both { old: 0, line: 0 },
                Pair::Both { old: 1, line: 1 },
                Pair::Extra { line: 2 },
                Pair::Both { old: 2, line: 3 },
                Pair::Both { old: 3, line: 4 },
            ]
        );
        assert!(!c.loose, "the extra line is kept, not removed");
    }

    #[test]
    fn a_context_line_that_is_not_in_the_file_is_missing() {
        let h = hunk(" fn main() {\n     let a = 1;\n     let zz = 9;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let found = candidates(&h, &lines(FILE)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let c = &found[0];
        assert_eq!(c.len, 4);
        assert_eq!(
            c.pairs,
            vec![
                Pair::Both { old: 0, line: 0 },
                Pair::Both { old: 1, line: 1 },
                Pair::Missing { old: 2 },
                Pair::Both { old: 3, line: 2 },
                Pair::Both { old: 4, line: 3 },
            ]
        );
        assert!(!c.loose);
    }

    #[test]
    fn a_missing_removed_line_is_loose() {
        let h = hunk(" fn main() {\n     let a = 1;\n-    let zz = 9;\n+    let b = 5;\n     let b = 2;\n");
        let found = candidates(&h, &lines(FILE)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].loose);
        assert!(found[0].pairs.contains(&Pair::Missing { old: 2 }));
    }

    #[test]
    fn loose_candidates_are_offered_only_when_no_strict_one_exists() {
        // Two similar blocks: in the first only a context line differs, in the
        // second a removed line differs.
        let file = "a1\nb1\nc1\nd1\n---\na1\nbX\nc1\nd1\n";
        let h = hunk(" a1\n-b9\n c1\n d1\n");
        // Block 2 differs on the removed line only; block 1 too: both loose.
        let found = candidates(&h, &lines(file)).unwrap();
        assert!(found.iter().all(|c| c.loose), "{found:?}");
        assert_eq!(found.len(), 2);

        let h = hunk(" a9\n-b1\n c1\n d1\n");
        let found = candidates(&h, &lines(file)).unwrap();
        assert!(found.iter().all(|c| !c.loose), "{found:?}");
        assert_eq!(found[0].start, 0);

        // A strict candidate hides the loose ones.
        let file = "a1\nb1\nc1\nd1\n---\na9\nbX\nc1\nd1\n";
        let h = hunk(" a1\n-b1\n c1\n dX\n");
        let found = candidates(&h, &lines(file)).unwrap();
        assert!(found.iter().all(|c| !c.loose), "{found:?}");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, 0);
    }

    #[test]
    fn nothing_similar_gives_no_candidates() {
        let h = hunk(" alpha\n-beta\n gamma\n");
        assert_eq!(candidates(&h, &lines(FILE)).unwrap(), vec![]);
    }

    #[test]
    fn one_exact_line_is_needed() {
        // Every line is close but none is equal: not offered.
        let h = hunk(" fn main()  {{\n     let a = 11;\n-    let b = 22;\n");
        assert_eq!(candidates(&h, &lines(FILE)).unwrap(), vec![]);
    }

    #[test]
    fn half_of_the_old_side_must_match() {
        let file = "a1\nqqqq\nwwww\neeee\n";
        // One line in four matches: not offered.
        let h = hunk(" a1\n b9x9x\n c9y9y\n d9z9z\n");
        assert_eq!(candidates(&h, &lines(file)).unwrap(), vec![]);
        // One line in two matches: offered (the boundary).
        let h = hunk(" a1\n-b9x9x\n");
        assert_eq!(candidates(&h, &lines(file)).unwrap().len(), 1);
        // Two in four: offered.
        let h = hunk(" a1\n qqqq\n c9y9y\n d9z9z\n");
        assert_eq!(candidates(&h, &lines(file)).unwrap().len(), 1);
    }

    #[test]
    fn candidates_do_not_overlap_and_are_best_first() {
        let file = "p\nq\nr\nx\np9\nq\nr\n";
        let h = hunk(" p\n q\n-r\n");
        let found = candidates(&h, &lines(file)).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].score >= found[1].score);
        assert_eq!(found[0].start, 0, "the exact-text block first");
        // Windows never share lines.
        let (a, b) = (&found[0], &found[1]);
        assert!(a.start + a.len <= b.start || b.start + b.len <= a.start);
    }

    #[test]
    fn at_most_five_candidates_are_returned() {
        let mut file = String::new();
        for n in 0..8 {
            file.push_str(&format!("p\nq\nr{n}\n--\n"));
        }
        let h = hunk(" p\n q\n-rX\n");
        let found = candidates(&h, &lines(&file)).unwrap();
        assert_eq!(found.len(), MAX_CANDIDATES);
    }

    #[test]
    fn the_hunk_line_number_breaks_ties_in_a_diff() {
        let mut file = String::new();
        for _ in 0..3 {
            file.push_str("p\nq\nr1\n--\n");
        }
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -5,3 +5,3 @@\n p\n q\n-r2\n+r3\n";
        let h = read(diff, Format::GitDiff).remove(0).hunks.remove(0);
        let found = candidates(&h, &lines(&file)).unwrap();
        assert_eq!(found.len(), 3);
        // Equal scores: the block starting at line 5 (0-based 4) is first.
        assert_eq!(found[0].start, 4);
    }

    #[test]
    fn limits_refuse_instead_of_searching() {
        let h = hunk(" a\n-b\n");
        let big = vec!["x"; MAX_FILE_LINES + 1];
        assert_eq!(candidates(&h, &big), Err(Refusal::FileTooLong));
        let mut body = String::new();
        for n in 0..=MAX_HUNK_LINES {
            body.push_str(&format!(" l{n}\n"));
        }
        assert_eq!(candidates(&hunk(&body), &lines(FILE)), Err(Refusal::HunkTooLong));
        assert_eq!(candidates(&hunk("-only\n"), &lines(FILE)), Err(Refusal::TooShort));
        assert!(Refusal::TooShort.describe().contains("too few"));
    }

    #[test]
    fn an_overlapping_loose_window_does_not_hide_a_strict_one() {
        // The patch deletes `c1`, which is on the last line, and quotes one context
        // line too many after it. A window at lines 2-4 would delete `d1` instead:
        // it scores higher, shares line 4 with the strict window, and used to
        // remove it before the strict/loose split, so only the `d1` place was shown.
        let file = "b1\nb1\nd1\nb1\nc1\n";
        let h = hunk(" b1\n-c1\n b1\n+new\n");
        let (found, not_shown) = candidates_counted(&h, &lines(file)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].loose, "{found:?}");
        assert_eq!(found[0].start, 3);
        assert_eq!(not_shown, NotShown { capped: 0, loose_hidden: 1 });
    }

    #[test]
    fn a_late_match_is_not_dropped_for_the_sake_of_earlier_look_alikes() {
        // 1,500 look-alike functions give far more candidate starts than are
        // scored. The true place, further down, is supported by one more old line
        // than any of them (the exact `fn target() {`) and must be among those scored.
        let mut file = String::new();
        for i in 0..1500 {
            file.push_str(&format!("fn f{i}() {{\n    let a = {i};\n    call(a);\n}}\n\n"));
        }
        let late = file.lines().count();
        file.push_str("fn target() {\n    let a = 7;\n    call(a);\n}\n\n");
        let h = hunk(" fn target() {\n     let a = 70;\n-    call(a);\n+    call(a + 1);\n }\n \n");
        let found = candidates(&h, &lines(&file)).unwrap();
        assert_eq!(found[0].start, late, "{:?}", found.iter().map(|c| (c.start, c.score)).collect::<Vec<_>>());
        assert!(!found[0].loose);
    }

    #[test]
    fn what_the_cap_leaves_out_is_counted() {
        let mut file = String::new();
        for n in 0..8 {
            file.push_str(&format!("p\nq\nr{n}\n--\n"));
        }
        let h = hunk(" p\n q\n-rX\n");
        let (found, not_shown) = candidates_counted(&h, &lines(&file)).unwrap();
        assert_eq!(found.len(), MAX_CANDIDATES);
        assert_eq!(not_shown.capped, 3);
    }

    #[test]
    fn blank_and_brace_lines_alone_do_not_place_a_hunk() {
        // Nothing the hunk says about `handle` is in the file; its blank line and
        // closing brace match every function, and those must not be enough.
        let file = "fn other() {\n    stuff();\n}\n\nfn another() {\n    more();\n}\n";
        let h = hunk(" \n fn handle() {\n-    old();\n }\n+new\n");
        assert_eq!(candidates(&h, &lines(file)).unwrap(), vec![]);
    }

    #[test]
    fn a_window_may_not_run_past_the_end_of_the_file() {
        let file = "a\nb\n";
        let h = hunk(" a\n b\n-c\n");
        let found = candidates(&h, &lines(file)).unwrap();
        // The third line is absent: a window of two lines with one missing.
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].len, 2);
        assert!(found[0].pairs.contains(&Pair::Missing { old: 2 }));
    }
}
