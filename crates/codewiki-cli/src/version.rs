// Minimal semantic-version type for the `upgrade` command and installer logic.
//
// We deliberately avoid pulling in the `semver` crate as a *direct* dependency:
// CodeWiki tags follow plain `MAJOR.MINOR.PATCH` (optionally `v`-prefixed, with
// an optional `-prerelease`/`+build` suffix we ignore), so a tiny parser fully
// covers our needs without growing the dependency surface.
//
// Ordering is a correct *numeric* compare of (major, minor, patch) — NOT a
// lexical string compare — so `v0.10.0 > v0.9.0` holds.

use std::cmp::Ordering;
use std::fmt;

/// A parsed `MAJOR.MINOR.PATCH` version. Pre-release / build metadata is
/// intentionally dropped during parsing (we only care about release ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse a version string.
    ///
    /// Accepts an optional leading `v`/`V`, then `MAJOR.MINOR.PATCH`. A trailing
    /// `-prerelease` or `+build` suffix (after the patch) is ignored. A missing
    /// minor/patch defaults to 0 (so `v1` == `1.0.0`, `v1.2` == `1.2.0`).
    ///
    /// Returns `None` if the core numeric portion is absent or non-numeric.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        // Strip a single leading `v`/`V`.
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);

        // Cut off any pre-release (`-`) or build (`+`) metadata.
        let core = s
            .split(['-', '+'])
            .next()
            .map(str::trim)
            .filter(|c| !c.is_empty())?;

        let mut parts = core.split('.');
        let major = parse_num(parts.next())?;
        // minor / patch are optional and default to 0.
        let minor = match parts.next() {
            Some(p) => parse_num(Some(p))?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(p) => parse_num(Some(p))?,
            None => 0,
        };
        // Reject extra dotted components (e.g. `1.2.3.4`) as unparseable.
        if parts.next().is_some() {
            return None;
        }

        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

/// Parse a single numeric component, rejecting empty / non-digit input.
fn parse_num(part: Option<&str>) -> Option<u64> {
    let p = part?.trim();
    if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    p.parse::<u64>().ok()
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_triple() {
        assert_eq!(
            Version::parse("1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
    }

    #[test]
    fn strips_v_prefix() {
        assert_eq!(Version::parse("v0.9.0"), Version::parse("0.9.0"));
        assert_eq!(Version::parse("V0.9.0"), Version::parse("0.9.0"));
    }

    #[test]
    fn numeric_not_lexical_ordering() {
        // The key regression: 10 > 9 numerically, even though "0.10.0" < "0.9.0"
        // as a string.
        let a = Version::parse("v0.10.0").unwrap();
        let b = Version::parse("v0.9.0").unwrap();
        assert!(a > b, "v0.10.0 must be greater than v0.9.0");
    }

    #[test]
    fn equal_with_and_without_prefix() {
        assert_eq!(
            Version::parse("v1.2.3").unwrap(),
            Version::parse("1.2.3").unwrap()
        );
        assert_eq!(
            Version::parse("1.2.3")
                .unwrap()
                .cmp(&Version::parse("v1.2.3").unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn ignores_prerelease_and_build() {
        assert_eq!(
            Version::parse("1.2.3-rc.1"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        assert_eq!(
            Version::parse("v1.2.3+build.5"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        // A pre-release is treated as equal to its release for our purposes
        // (we never compare pre-releases against each other in practice).
        assert_eq!(
            Version::parse("1.2.3-rc.1").unwrap(),
            Version::parse("1.2.3").unwrap()
        );
    }

    #[test]
    fn missing_minor_patch_default_zero() {
        assert_eq!(
            Version::parse("v1"),
            Some(Version {
                major: 1,
                minor: 0,
                patch: 0
            })
        );
        assert_eq!(
            Version::parse("1.2"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 0
            })
        );
    }

    #[test]
    fn unparseable_returns_none() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("v"), None);
        assert_eq!(Version::parse("not-a-version"), None);
        assert_eq!(Version::parse("1.x.0"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("..."), None);
        // Embedded text in a component is rejected.
        assert_eq!(Version::parse("1.2.3abc"), None);
    }

    #[test]
    fn comparison_chain() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("2.0.0") > v("1.99.99"));
        assert!(v("1.0.1") > v("1.0.0"));
        assert!(v("1.1.0") > v("1.0.99"));
        assert!(v("0.0.0") < v("0.0.1"));
    }

    #[test]
    fn display_normalizes() {
        assert_eq!(Version::parse("v1.2").unwrap().to_string(), "1.2.0");
    }
}
