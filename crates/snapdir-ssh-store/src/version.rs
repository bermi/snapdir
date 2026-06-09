//! OpenSSH version-floor check (`ssh -V`, fail-closed, >= 8.5).
//!
//! 8.5 is the oldest release carrying every algorithm on the pinned security
//! floor (notably `sntrup761x25519-sha512@openssh.com`), so emission fails
//! closed on anything older — or anything unparsable (an unknown client
//! cannot be assumed to honor the floor). There is deliberately **no
//! override env**.

use std::process::Command;

use crate::Error;

/// The minimum supported OpenSSH client version, as `(major, minor)`.
pub const MIN_OPENSSH: (u32, u32) = (8, 5);

/// Parses an `ssh -V` banner (`OpenSSH_9.6p1, LibreSSL …`, `OpenSSH_8.4p1
/// Debian…`) into `(major, minor)`.
///
/// # Errors
///
/// Fails closed on any banner that does not contain `OpenSSH_<major>.<minor>`
/// (including non-OpenSSH clients and `OpenSSH_for_Windows_…` spellings —
/// unknown clients cannot be assumed to honor the floor).
pub fn parse_openssh_version(banner: &str) -> Result<(u32, u32), Error> {
    let unparsable = || {
        Error::new(format!(
            "cannot parse OpenSSH version from `ssh -V` output {banner:?}; \
             refusing to proceed (the security floor requires OpenSSH >= \
             {}.{})",
            MIN_OPENSSH.0, MIN_OPENSSH.1
        ))
    };
    let rest = banner
        .find("OpenSSH_")
        .map(|idx| &banner[idx + "OpenSSH_".len()..])
        .ok_or_else(unparsable)?;
    let (major, rest) = take_number(rest).ok_or_else(unparsable)?;
    let rest = rest.strip_prefix('.').ok_or_else(unparsable)?;
    let (minor, _) = take_number(rest).ok_or_else(unparsable)?;
    Ok((major, minor))
}

/// Parses the banner and enforces the [`MIN_OPENSSH`] floor.
///
/// # Errors
///
/// Fails closed on an unparsable banner ([`parse_openssh_version`]) or a
/// version below 8.5, naming the detected version and the floor.
pub fn check_openssh_floor(banner: &str) -> Result<(u32, u32), Error> {
    let (major, minor) = parse_openssh_version(banner)?;
    if (major, minor) < MIN_OPENSSH {
        return Err(Error::new(format!(
            "OpenSSH {major}.{minor} is too old: the security floor requires \
             OpenSSH >= {}.{} (it pins algorithms older clients do not \
             ship); upgrade the local OpenSSH client",
            MIN_OPENSSH.0, MIN_OPENSSH.1
        )));
    }
    Ok((major, minor))
}

/// Runs the real `ssh -V` (version banner arrives on **stderr**) and enforces
/// the floor. Thin untested wrapper over the table-tested
/// [`check_openssh_floor`].
///
/// # Errors
///
/// Fails closed if `ssh` cannot be spawned, or per [`check_openssh_floor`].
pub fn detect_openssh_floor() -> Result<(u32, u32), Error> {
    let output = Command::new("ssh")
        .arg("-V")
        .output()
        .map_err(|e| Error::new(format!("failed to run `ssh -V`: {e}")))?;
    let banner = String::from_utf8_lossy(&output.stderr);
    check_openssh_floor(&banner)
}

/// Splits a leading decimal number off `s`; `None` if it does not start with
/// a digit.
fn take_number(s: &str) -> Option<(u32, &str)> {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(idx, _)| idx);
    let number = s.get(..end)?.parse().ok()?;
    Some((number, &s[end..]))
}
