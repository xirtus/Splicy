//! Restarts: a phrase said, then said again a moment later.
//!
//! Adapted from Scarlet's `restarts.ts` (MIT, github.com/Forward-Future/scarlet), the same
//! rules as SexOS's `sex-splice` crate. A speaker who stumbles starts the sentence over
//! ("there's this like white, I don't know what that is, there's this like white stuff"), or a
//! retake sits beside the take that replaced it. Both leave the same words twice within a few
//! seconds. Whether the first saying should go takes the sentence to tell, so [`find_restarts`]
//! only locates them; Remove Restarts cuts the first saying and anything said before the second.
//!
//! What counts: three matching words or more (two of them different), or two different words
//! with at most one between; the second saying within 12 words and 12 seconds of the first's
//! end; a whole first attempt again with one word slipped in or left out after its first word;
//! a contraction compares as the word it starts with ("I will … I'll"); each saying is the
//! second of at most one phrase, and a third saying pairs with the second.

use std::collections::BTreeSet;

use filmcraft_time::{FrameRate, Tick, TimeRange};

use super::SeqWord;

/// `length` words from `at`, said again from `next` (`again` words long when the second saying
/// adds or drops a word, else `length`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restart {
    pub at: usize,
    pub length: usize,
    pub next: usize,
    pub again: Option<usize>,
}

/// How far after the first saying ends the second may begin, in seconds and in words.
pub const RESTART_WITHIN_SECONDS: f64 = 12.0;
pub const RESTART_WITHIN_WORDS: usize = 12;

const CONTRACTED: &[(&str, &str)] = &[
    ("ill", "i"), ("im", "i"), ("ive", "i"), ("id", "i"), ("youll", "you"), ("youre", "you"), ("youve", "you"),
    ("youd", "you"), ("theyre", "they"), ("theyll", "they"), ("theyve", "they"), ("itll", "it"), ("its", "it"),
    ("thats", "that"), ("whats", "what"), ("theres", "there"), ("heres", "here"), ("lets", "let"), ("weve", "we"),
    ("dont", "do"), ("doesnt", "does"), ("didnt", "did"), ("cant", "can"), ("wont", "will"), ("isnt", "is"),
];

/// A word's comparison key: normalised, apostrophes dropped, a contraction folded to its first word.
pub fn restart_key(text: &str) -> String {
    let k: String = filmcraft_project::transcript::normalize_word(text).chars().filter(|c| *c != '\'' && *c != '’').collect();
    CONTRACTED.iter().find(|(c, _)| *c == k).map_or(k, |(_, w)| w.to_string())
}

fn distinct(keys: &[String]) -> usize {
    keys.iter().collect::<BTreeSet<_>>().len()
}

/// Phrases said twice within a moment, in spoken order.
pub fn find_restarts(words: &[SeqWord]) -> Vec<Restart> {
    let keys: Vec<String> = words.iter().map(|w| restart_key(&w.text)).collect();
    let within = Tick::from_seconds_f64(RESTART_WITHIN_SECONDS);
    let n = keys.len();
    let same = |a: usize, b: usize| !keys[a].is_empty() && keys[a] == keys[b];
    let mut found = Vec::new();
    let mut paired = BTreeSet::new();
    let mut i = 0;
    while i + 1 < n {
        let mut best: Option<Restart> = None;
        if !keys[i].is_empty() {
            for j in i + 2..n.min(i + 2 * RESTART_WITHIN_WORDS + 3) {
                if !same(i, j) || paired.contains(&j) {
                    continue;
                }
                let mut length = 0;
                while i + length < j && j + length < n && same(i + length, j + length) {
                    length += 1;
                }
                let between = j - (i + length);
                let d = distinct(&keys[i..i + length]);
                if between <= RESTART_WITHIN_WORDS
                    && words[j].start - words[i + length - 1].end <= within
                    && ((length >= 3 && d >= 2) || (length == 2 && d == 2 && between <= 1))
                {
                    if best.is_none_or(|b| length > b.length) {
                        best = Some(Restart { at: i, length, next: j, again: None });
                    }
                    continue;
                }
                // The whole first attempt again, a word slipped in or left out after its first word.
                for (skip_a, skip_b) in [(0, 1), (1, 0)] {
                    let (mut a, mut b, mut more) = (i + length + skip_a, j + length + skip_b, 0);
                    while a < j && b < n && same(a, b) {
                        a += 1;
                        b += 1;
                        more += 1;
                    }
                    if a != j || length + more < 3 || more < 1 || distinct(&keys[i..j]) < 2 || words[j].start - words[j - 1].end > within {
                        continue;
                    }
                    if best.is_none_or(|r| j - i > r.length) {
                        best = Some(Restart { at: i, length: j - i, next: j, again: Some(b - j) });
                    }
                    break;
                }
            }
        }
        match best {
            Some(r) => {
                found.push(r);
                paired.insert(r.next);
                i = r.at + r.length;
            }
            None => i += 1,
        }
    }
    found
}

/// Timeline ranges removing each restart's first saying and anything said before the second,
/// from the first word (snapped to the nearest frame, never into the word before) to the frame
/// the second saying starts in. A restart that spans two clips is left alone: the cut between
/// them is an edit already.
pub fn restart_ranges(words: &[SeqWord], restarts: &[Restart], rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for r in restarts {
        let (a, n) = (r.at, r.next);
        if n >= words.len() || words[a..=n].iter().any(|w| w.clip != words[a].clip) {
            continue;
        }
        let mut s = rate.snap_nearest(words[a].start);
        if a > 0 && s < words[a - 1].end {
            s = rate.snap(words[a - 1].end) + rate.frame_duration();
        }
        let e = rate.snap(words[n].start);
        if e > s {
            out.push(TimeRange::from_bounds(s, e));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ClipId, ItemId};
    use filmcraft_time::TICKS_PER_SECOND;

    const R: FrameRate = FrameRate { num: 25, den: 1 };

    fn said(text: &str) -> Vec<SeqWord> {
        let ms = |m: i64| Tick(m * TICKS_PER_SECOND / 1000);
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| SeqWord {
                text: w.into(),
                start: ms(i as i64 * 300),
                end: ms(i as i64 * 300 + 250),
                clip: ClipId(1),
                item: ItemId(1),
                index: i,
                track: 0,
                speaker: None,
                confidence: 1.0,
            })
            .collect()
    }

    fn found(text: &str) -> Vec<(usize, usize, usize, Option<usize>)> {
        find_restarts(&said(text)).iter().map(|r| (r.at, r.length, r.next, r.again)).collect()
    }

    #[test]
    fn restarts_are_found_as_scarlet_finds_them() {
        assert_eq!(found("there's this like white, I don't know what that is, there's this like white stuff"), [(0, 4, 10, None)]);
        assert_eq!(found("you can you can see it"), [(0, 2, 2, None)]);
        assert_eq!(found("you can do it and you can"), []);
        assert_eq!(found("no no no no"), []);
        assert_eq!(found("and then I will, and then I'll copy it"), [(0, 3, 4, None)]);
        assert_eq!(found("how mechanical watch, how a mechanical watch works"), [(0, 3, 3, Some(4))]);
        assert_eq!(found("how a mechanical watch, how mechanical watch works"), [(0, 4, 4, Some(3))]);
        assert_eq!(found("I think that I think that I think that is right"), [(0, 3, 3, None), (3, 3, 6, None)]);
        assert_eq!(found("we went there a b c d e f g h i j k l m we went there"), []);
    }

    #[test]
    fn too_late_in_seconds_is_ordinary_speech() {
        let mut w = said("we went there and we went there");
        for x in &mut w[4..] {
            x.start += Tick::from_seconds_f64(20.0);
            x.end += Tick::from_seconds_f64(20.0);
        }
        assert!(find_restarts(&w).is_empty());
    }

    #[test]
    fn the_cut_runs_from_the_first_saying_to_the_frame_the_second_starts() {
        let w = said("so there's this like white, I don't know what that is, there's this like white stuff");
        let r = find_restarts(&w);
        assert_eq!(r.len(), 1);
        let ranges = restart_ranges(&w, &r, R);
        // Word 1 starts at 0.3 s, frame 7.5 → nearest frame 8 (0.32 s) would cut into nothing
        // said before (word 0 ends 0.25 s); word 11 starts at 3.3 s, inside frame 82 (3.28 s).
        assert_eq!(ranges, [TimeRange::from_bounds(R.snap_nearest(w[1].start), R.snap(w[11].start))]);
        assert!(ranges[0].start >= w[0].end && ranges[0].end() <= w[11].start);
        // A restart across two clips is left to the editor.
        let mut w2 = w.clone();
        for x in &mut w2[6..] {
            x.clip = ClipId(2);
        }
        assert!(restart_ranges(&w2, &r, R).is_empty());
    }
}
