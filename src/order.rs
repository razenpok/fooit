//! Positions of siblings as fractional keys: strings that sort in sibling order, where a new
//! key always fits between two others. Moving an item then changes only that item, which keeps
//! what has to be synced small. Keys are base-62 fractions that never end in the lowest digit

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: usize = DIGITS.len();

fn digit(c: u8) -> usize {
    DIGITS.iter().position(|d| *d == c).unwrap_or(0)
}

/// A key that sorts after `a` and before `b`, where either end may be open. When `a` is not
/// below `b` (two clients picked the same key) the result only sorts after `a`
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("");
    match b {
        Some(b) if a < b => midpoint(a.as_bytes(), Some(b.as_bytes())),
        _ => midpoint(a.as_bytes(), None),
    }
}

fn midpoint(a: &[u8], b: Option<&[u8]>) -> String {
    if let Some(b) = b {
        // a shared prefix, reading a missing digit of `a` as the lowest one
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(DIGITS[0]) == b[n] {
            n += 1;
        }
        if n > 0 {
            let rest = if a.len() > n { &a[n..] } else { &[] };
            return String::from_utf8_lossy(&b[..n]).into_owned() + &midpoint(rest, Some(&b[n..]));
        }
    }
    let low = a.first().map_or(0, |c| digit(*c));
    let high = b.map_or(BASE, |b| digit(b[0]));
    if high - low > 1 {
        return (DIGITS[(low + high) / 2] as char).to_string();
    }
    match b {
        // b's first digit alone sorts below b and above a
        Some(b) if b.len() > 1 => (b[0] as char).to_string(),
        _ => {
            let rest = if a.len() > 1 { &a[1..] } else { &[] };
            (DIGITS[low] as char).to_string() + &midpoint(rest, None)
        }
    }
}

/// `n` increasing keys spread evenly, for renumbering a whole list of siblings
pub fn spread(n: usize) -> Vec<String> {
    let mut width = 1;
    while BASE.pow(width) <= n {
        width += 1;
    }
    let span = BASE.pow(width);
    (1..=n)
        .map(|i| {
            let mut value = i * span / (n + 1);
            let mut key = vec![DIGITS[0]; width as usize];
            for place in (0..width as usize).rev() {
                key[place] = DIGITS[value % BASE];
                value /= BASE;
            }
            while key.last() == Some(&DIGITS[0]) {
                key.pop();
            }
            String::from_utf8(key).unwrap()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_fit_between() {
        let first = between(None, None);
        let mut keys = vec![first.clone()];
        // keep inserting at the front, at the back and in the middle
        for i in 0..200 {
            let key = match i % 3 {
                0 => between(None, Some(&keys[0])),
                1 => between(Some(keys.last().unwrap()), None),
                _ => {
                    let m = keys.len() / 2;
                    between(Some(&keys[m - 1]), Some(&keys[m]))
                }
            };
            match i % 3 {
                0 => keys.insert(0, key),
                1 => keys.push(key),
                _ => keys.insert(keys.len() / 2, key),
            }
        }
        for pair in keys.windows(2) {
            assert!(pair[0] < pair[1], "{:?}", pair);
        }
        assert!(keys.iter().all(|k| !k.is_empty() && !k.ends_with('0')));
    }

    #[test]
    fn spread_is_increasing() {
        for n in [0, 1, 2, 61, 62, 63, 500] {
            let keys = spread(n);
            assert_eq!(keys.len(), n);
            assert!(keys.windows(2).all(|p| p[0] < p[1]));
            assert!(keys.iter().all(|k| !k.is_empty() && !k.ends_with('0')));
        }
        let keys = spread(3);
        assert!(between(Some(&keys[0]), Some(&keys[1])) > keys[0]);
    }

    #[test]
    fn equal_neighbours_still_give_a_later_key() {
        assert!(between(Some("V"), Some("V")).as_str() > "V");
    }
}
