//! Semver version handling for PubGrub.

use pubgrub::Range;
use semver::{Comparator, Op, Version, VersionReq};

/// Convert a semver VersionReq to a PubGrub Range.
pub fn version_req_to_range(req: &VersionReq) -> Range<Version> {
    if req.comparators.is_empty() {
        return Range::full();
    }

    let mut range = Range::full();

    for comp in &req.comparators {
        let comp_range = comparator_to_range(comp);
        range = range.intersection(&comp_range);
    }

    range
}

/// Convert a single semver Comparator to a PubGrub Range.
fn comparator_to_range(comp: &Comparator) -> Range<Version> {
    let major = comp.major;
    let minor = comp.minor.unwrap_or(0);
    let patch = comp.patch.unwrap_or(0);

    let version = Version::new(major, minor, patch);

    match comp.op {
        Op::Exact => Range::singleton(version),

        Op::Greater => Range::strictly_higher_than(version),

        Op::GreaterEq => Range::higher_than(version),

        Op::Less => Range::strictly_lower_than(version),

        Op::LessEq => {
            // <= x.y.z means < (x.y.z + 1)
            let next = bump_patch(&version);
            Range::strictly_lower_than(next)
        }

        Op::Tilde => {
            // ~x.y.z allows patch-level changes
            // ~1.2.3 means >=1.2.3 <1.3.0
            let upper = if comp.minor.is_some() {
                Version::new(major, minor + 1, 0)
            } else {
                Version::new(major + 1, 0, 0)
            };

            Range::between(version, upper)
        }

        Op::Caret => {
            // ^x.y.z allows changes that don't modify the left-most non-zero digit
            // ^1.2.3 means >=1.2.3 <2.0.0
            // ^0.2.3 means >=0.2.3 <0.3.0
            // ^0.0.3 means >=0.0.3 <0.0.4
            let upper = if major > 0 {
                Version::new(major + 1, 0, 0)
            } else if minor > 0 {
                Version::new(0, minor + 1, 0)
            } else {
                Version::new(0, 0, patch + 1)
            };

            Range::between(version, upper)
        }

        Op::Wildcard => {
            // x.y.* means >=x.y.0 <x.(y+1).0
            if comp.minor.is_some() {
                let upper = Version::new(major, minor + 1, 0);
                Range::between(version, upper)
            } else {
                let upper = Version::new(major + 1, 0, 0);
                Range::between(version, upper)
            }
        }

        _ => Range::full(),
    }
}

/// Bump the patch version.
fn bump_patch(v: &Version) -> Version {
    Version::new(v.major, v.minor, v.patch + 1)
}

/// Parse a version string, allowing for incomplete versions.
pub fn parse_version_lenient(s: &str) -> Option<Version> {
    // Try exact parse first
    if let Ok(v) = s.parse() {
        return Some(v);
    }

    // Try adding missing components
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        1 => {
            let major: u64 = parts[0].parse().ok()?;
            Some(Version::new(major, 0, 0))
        }
        2 => {
            let major: u64 = parts[0].parse().ok()?;
            let minor: u64 = parts[1].parse().ok()?;
            Some(Version::new(major, minor, 0))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caret_range() {
        let req: VersionReq = "^1.2.3".parse().unwrap();
        let range = version_req_to_range(&req);

        assert!(range.contains(&Version::new(1, 2, 3)));
        assert!(range.contains(&Version::new(1, 2, 4)));
        assert!(range.contains(&Version::new(1, 9, 0)));
        assert!(!range.contains(&Version::new(2, 0, 0)));
        assert!(!range.contains(&Version::new(1, 2, 2)));
    }

    #[test]
    fn test_caret_range_zero_major() {
        let req: VersionReq = "^0.2.3".parse().unwrap();
        let range = version_req_to_range(&req);

        assert!(range.contains(&Version::new(0, 2, 3)));
        assert!(range.contains(&Version::new(0, 2, 9)));
        assert!(!range.contains(&Version::new(0, 3, 0)));
    }

    #[test]
    fn test_tilde_range() {
        let req: VersionReq = "~1.2.3".parse().unwrap();
        let range = version_req_to_range(&req);

        assert!(range.contains(&Version::new(1, 2, 3)));
        assert!(range.contains(&Version::new(1, 2, 9)));
        assert!(!range.contains(&Version::new(1, 3, 0)));
    }

    #[test]
    fn test_exact_range() {
        let req: VersionReq = "=1.2.3".parse().unwrap();
        let range = version_req_to_range(&req);

        assert!(range.contains(&Version::new(1, 2, 3)));
        assert!(!range.contains(&Version::new(1, 2, 4)));
    }

    #[test]
    fn test_comparison_range() {
        let req: VersionReq = ">=1.0, <2.0".parse().unwrap();
        let range = version_req_to_range(&req);

        assert!(range.contains(&Version::new(1, 0, 0)));
        assert!(range.contains(&Version::new(1, 9, 9)));
        assert!(!range.contains(&Version::new(2, 0, 0)));
        assert!(!range.contains(&Version::new(0, 9, 9)));
    }

    #[test]
    fn test_parse_version_lenient() {
        assert_eq!(
            parse_version_lenient("1"),
            Some(Version::new(1, 0, 0))
        );
        assert_eq!(
            parse_version_lenient("1.2"),
            Some(Version::new(1, 2, 0))
        );
        assert_eq!(
            parse_version_lenient("1.2.3"),
            Some(Version::new(1, 2, 3))
        );
    }
}
