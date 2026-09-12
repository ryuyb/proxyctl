//! Kernel version identity.

use crate::shared::error::DomainError;

/// A kernel version string, e.g. `v1.19.30`.
///
/// Kept as an opaque, validated string: the domain compares and orders versions
/// but does not need to understand upstream's scheme, and pretending otherwise
/// would couple us to a format that is not ours.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MihomoVersion(String);

impl MihomoVersion {
    /// Parses a version string.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] when the input is blank or contains
    /// whitespace, both of which indicate a misparsed value rather than a real
    /// version.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::invalid_input("version must not be empty"));
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(DomainError::invalid_input(
                "version must not contain whitespace",
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MihomoVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which kernel family reported itself.
///
/// `meta` is the only flavor this project targets; the variant exists so that
/// an unexpected flavor is recorded rather than silently assumed away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelFlavor {
    /// The Meta (Mihomo) kernel.
    Meta,
    /// Something else.
    Unknown,
}

/// A version plus the raw string it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MihomoBuild {
    /// The parsed version.
    pub version: MihomoVersion,
    /// The detected flavor.
    pub flavor: KernelFlavor,
    /// The upstream version endpoint's raw payload, for diagnostics.
    pub raw: String,
}

impl MihomoBuild {
    /// Builds a kernel description.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] when `version` is blank.
    pub fn new(
        version: impl Into<String>,
        flavor: KernelFlavor,
        raw: impl Into<String>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            version: MihomoVersion::parse(version)?,
            flavor,
            raw: raw.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_version() {
        let v = MihomoVersion::parse("v1.19.30").expect("valid");
        assert_eq!(v.as_str(), "v1.19.30");
        assert_eq!(v.to_string(), "v1.19.30");
    }

    #[test]
    fn trims_incidental_whitespace() {
        let v = MihomoVersion::parse("  v1.19.30\n").expect("valid");
        assert_eq!(v.as_str(), "v1.19.30");
    }

    #[test]
    fn rejects_blank_and_embedded_whitespace() {
        assert!(MihomoVersion::parse("").is_err());
        assert!(MihomoVersion::parse("   ").is_err());
        assert!(MihomoVersion::parse("v1.19 30").is_err());
    }

    #[test]
    fn orders_lexicographically_for_equality_checks() {
        let a = MihomoVersion::parse("v1.19.30").expect("valid");
        let b = MihomoVersion::parse("v1.19.30").expect("valid");
        let c = MihomoVersion::parse("v1.19.31").expect("valid");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn build_keeps_raw_for_diagnostics() {
        let b =
            MihomoBuild::new("v1.19.30", KernelFlavor::Meta, r#"{"meta":true}"#).expect("valid");
        assert_eq!(b.flavor, KernelFlavor::Meta);
        assert_eq!(b.raw, r#"{"meta":true}"#);
    }

    #[test]
    fn build_rejects_bad_version() {
        assert!(MihomoBuild::new("", KernelFlavor::Meta, "").is_err());
    }
}
