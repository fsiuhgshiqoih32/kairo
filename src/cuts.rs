//! Cuts inside a clip: the parts taken out between split points, and where a moment of the clip
//! lands in a copy without them.

/// Spans shorter than this are left alone (a split at nearly the same moment, rounding).
const TINY: f64 = 0.02;

/// The parts of `range` that stay, in order.
pub fn kept(range: (f64, f64), removed: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut at = range.0;
    for &(a, b) in removed {
        if b <= at || a >= range.1 {
            continue;
        }
        if a - at > TINY {
            out.push((at, a.min(range.1)));
        }
        at = at.max(b);
    }
    if range.1 - at > TINY {
        out.push((at, range.1));
    }
    out
}

/// How long the parts of `range` that stay last.
pub fn kept_length(range: (f64, f64), removed: &[(f64, f64)]) -> f64 {
    kept(range, removed).iter().map(|(a, b)| b - a).sum()
}

/// Whether anything inside `range` is cut out.
pub fn cuts_into(range: (f64, f64), removed: &[(f64, f64)]) -> bool {
    kept(range, removed) != [range]
}

/// The part between split points (inside `range`) that holds `t`.
pub fn part_at(range: (f64, f64), splits: &[f64], t: f64) -> (f64, f64) {
    let t = t.clamp(range.0, range.1);
    let start = splits
        .iter()
        .copied()
        .filter(|s| *s > range.0 && *s <= t)
        .fold(range.0, f64::max);
    let end = splits
        .iter()
        .copied()
        .filter(|s| *s < range.1 && *s > t)
        .fold(range.1, f64::min);
    (start, end)
}

/// Adds a split at `t`, kept in order; none at the same moment twice.
pub fn split(splits: &mut Vec<f64>, t: f64) -> bool {
    if splits.iter().any(|s| (s - t).abs() < TINY) {
        return false;
    }
    splits.push(t);
    splits.sort_by(f64::total_cmp);
    true
}

/// Takes `span` out, merging it with the spans it touches.
pub fn remove(removed: &mut Vec<(f64, f64)>, span: (f64, f64)) {
    removed.push(span);
    removed.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(removed.len());
    for &(a, b) in removed.iter() {
        match merged.last_mut() {
            Some(last) if a <= last.1 + TINY => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    *removed = merged;
}

/// The removed span holding `t`, if `t` is cut out.
pub fn removed_at(removed: &[(f64, f64)], t: f64) -> Option<(f64, f64)> {
    removed.iter().copied().find(|(a, b)| *a <= t && t < *b)
}

/// Where the clip's moment `t` lands in a copy of `range` without the removed parts (before any
/// speed change). A moment that's cut out lands where the copy goes on; `None` outside `range`.
pub fn to_copy(range: (f64, f64), removed: &[(f64, f64)], t: f64) -> Option<f64> {
    if t < range.0 || t > range.1 {
        return None;
    }
    let mut out = 0.0;
    for (a, b) in kept(range, removed) {
        if t <= b {
            return Some(out + (t - a).max(0.0));
        }
        out += b - a;
    }
    Some(out)
}

/// Where something shown from `a` to `b` in the clip shows in the copy, or `None` if all of it is
/// cut out or outside `range`.
pub fn window(range: (f64, f64), removed: &[(f64, f64)], a: f64, b: f64) -> Option<(f64, f64)> {
    let (a, b) = (a.max(range.0), b.min(range.1));
    if b <= a {
        return None;
    }
    let shown: f64 = kept(range, removed)
        .iter()
        .map(|(s, e)| (e.min(b) - s.max(a)).max(0.0))
        .sum();
    if shown <= TINY {
        return None;
    }
    let start = to_copy(range, removed, a)?;
    Some((start, start + shown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_removes_parts() {
        let range = (2.0, 12.0);
        let mut splits = Vec::new();
        assert!(split(&mut splits, 7.0));
        assert!(split(&mut splits, 4.0));
        assert!(!split(&mut splits, 4.005), "not twice at one moment");
        assert_eq!(splits, vec![4.0, 7.0]);
        assert_eq!(part_at(range, &splits, 5.0), (4.0, 7.0));
        assert_eq!(part_at(range, &splits, 1.0), (2.0, 4.0));
        assert_eq!(part_at(range, &splits, 9.0), (7.0, 12.0));

        let mut removed = Vec::new();
        remove(&mut removed, (4.0, 7.0));
        assert_eq!(kept(range, &removed), vec![(2.0, 4.0), (7.0, 12.0)]);
        assert_eq!(kept_length(range, &removed), 7.0);
        assert!(cuts_into(range, &removed));
        assert!(!cuts_into((7.5, 11.0), &removed));
        // Removing the next part merges the two spans.
        remove(&mut removed, (7.0, 9.0));
        assert_eq!(removed, vec![(4.0, 9.0)]);
        assert_eq!(removed_at(&removed, 5.0), Some((4.0, 9.0)));
        assert_eq!(removed_at(&removed, 9.0), None);
    }

    #[test]
    fn maps_clip_time_into_the_copy() {
        let range = (2.0, 12.0);
        let removed = vec![(4.0, 7.0)];
        assert_eq!(to_copy(range, &removed, 3.0), Some(1.0));
        assert_eq!(
            to_copy(range, &removed, 5.0),
            Some(2.0),
            "cut out: where it goes on"
        );
        assert_eq!(to_copy(range, &removed, 8.0), Some(3.0));
        assert_eq!(to_copy(range, &removed, 13.0), None);
        // A caption across the cut keeps its shown parts, back to back.
        assert_eq!(window(range, &removed, 3.0, 8.0), Some((1.0, 3.0)));
        assert_eq!(window(range, &removed, 4.5, 6.5), None, "all cut out");
        assert_eq!(window(range, &removed, 0.0, 3.0), Some((0.0, 1.0)));
    }
}
