//! Did-you-mean matching: the candidate nearest a misspelled name.

/// The candidate closest to `name` by edit distance, if one is close
/// enough: one edit for names of up to three characters, else two.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let limit = if name.chars().count() <= 3 { 1 } else { 2 };
    candidates
        .into_iter()
        .filter(|candidate| *candidate != name)
        .map(|candidate| (levenshtein(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min()
        .map(|(_, candidate)| candidate.to_owned())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::closest;

    #[test]
    fn closest_scales_with_the_name() {
        assert_eq!(
            closest("lenght", ["length", "left"]),
            Some("length".to_owned())
        );
        assert_eq!(closest("ab", ["xy"]), None);
        assert_eq!(closest("ab", ["ac", "xy"]), Some("ac".to_owned()));
        assert_eq!(closest("same", ["same"]), None);
    }
}
