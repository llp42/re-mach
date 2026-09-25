// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Status codes returned by firmware functions.

use core::fmt;

/// Error bit.
const ERROR: usize = 1 << (usize::BITS - 1);

/// `EFI_STATUS`. Any value is valid, including codes not listed here.
#[repr(transparent)]
#[derive(PartialEq, Eq)]
pub struct Status(usize);

impl Status {
    pub const SUCCESS: Self = Self(0);

    // Warnings
    pub const WARN_UNKNOWN_GLYPH: Self = Self(1);
    pub const WARN_DELETE_FAILURE: Self = Self(2);
    pub const WARN_WRITE_FAILURE: Self = Self(3);
    pub const WARN_BUFFER_TOO_SMALL: Self = Self(4);
    pub const WARN_STALE_DATA: Self = Self(5);
    pub const WARN_FILE_SYSTEM: Self = Self(6);
    pub const WARN_RESET_REQUIRED: Self = Self(7);

    // Errors
    pub const LOAD_ERROR: Self = Self(ERROR | 1);
    pub const INVALID_PARAMETER: Self = Self(ERROR | 2);
    pub const UNSUPPORTED: Self = Self(ERROR | 3);
    pub const BAD_BUFFER_SIZE: Self = Self(ERROR | 4);
    pub const BUFFER_TOO_SMALL: Self = Self(ERROR | 5);
    pub const NOT_READY: Self = Self(ERROR | 6);
    pub const DEVICE_ERROR: Self = Self(ERROR | 7);
    pub const WRITE_PROTECTED: Self = Self(ERROR | 8);
    pub const OUT_OF_RESOURCES: Self = Self(ERROR | 9);
    pub const VOLUME_CORRUPTED: Self = Self(ERROR | 10);
    pub const VOLUME_FULL: Self = Self(ERROR | 11);
    pub const NO_MEDIA: Self = Self(ERROR | 12);
    pub const MEDIA_CHANGED: Self = Self(ERROR | 13);
    pub const NOT_FOUND: Self = Self(ERROR | 14);
    pub const ACCESS_DENIED: Self = Self(ERROR | 15);
    pub const NO_RESPONSE: Self = Self(ERROR | 16);
    pub const NO_MAPPING: Self = Self(ERROR | 17);
    pub const TIMEOUT: Self = Self(ERROR | 18);
    pub const NOT_STARTED: Self = Self(ERROR | 19);
    pub const ALREADY_STARTED: Self = Self(ERROR | 20);
    pub const ABORTED: Self = Self(ERROR | 21);
    pub const ICMP_ERROR: Self = Self(ERROR | 22);
    pub const TFTP_ERROR: Self = Self(ERROR | 23);
    pub const PROTOCOL_ERROR: Self = Self(ERROR | 24);
    pub const INCOMPATIBLE_VERSION: Self = Self(ERROR | 25);
    pub const SECURITY_VIOLATION: Self = Self(ERROR | 26);
    pub const CRC_ERROR: Self = Self(ERROR | 27);
    pub const END_OF_MEDIA: Self = Self(ERROR | 28);
    pub const END_OF_FILE: Self = Self(ERROR | 31);
    pub const INVALID_LANGUAGE: Self = Self(ERROR | 32);
    pub const COMPROMISED_DATA: Self = Self(ERROR | 33);
    pub const IP_ADDRESS_CONFLICT: Self = Self(ERROR | 34);
    pub const HTTP_ERROR: Self = Self(ERROR | 35);
}

/// Constant name, or hex if not listed.
impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::SUCCESS => "SUCCESS",

            Self::WARN_UNKNOWN_GLYPH => "WARN_UNKNOWN_GLYPH",
            Self::WARN_DELETE_FAILURE => "WARN_DELETE_FAILURE",
            Self::WARN_WRITE_FAILURE => "WARN_WRITE_FAILURE",
            Self::WARN_BUFFER_TOO_SMALL => "WARN_BUFFER_TOO_SMALL",
            Self::WARN_STALE_DATA => "WARN_STALE_DATA",
            Self::WARN_FILE_SYSTEM => "WARN_FILE_SYSTEM",
            Self::WARN_RESET_REQUIRED => "WARN_RESET_REQUIRED",

            Self::LOAD_ERROR => "LOAD_ERROR",
            Self::INVALID_PARAMETER => "INVALID_PARAMETER",
            Self::UNSUPPORTED => "UNSUPPORTED",
            Self::BAD_BUFFER_SIZE => "BAD_BUFFER_SIZE",
            Self::BUFFER_TOO_SMALL => "BUFFER_TOO_SMALL",
            Self::NOT_READY => "NOT_READY",
            Self::DEVICE_ERROR => "DEVICE_ERROR",
            Self::WRITE_PROTECTED => "WRITE_PROTECTED",
            Self::OUT_OF_RESOURCES => "OUT_OF_RESOURCES",
            Self::VOLUME_CORRUPTED => "VOLUME_CORRUPTED",
            Self::VOLUME_FULL => "VOLUME_FULL",
            Self::NO_MEDIA => "NO_MEDIA",
            Self::MEDIA_CHANGED => "MEDIA_CHANGED",
            Self::NOT_FOUND => "NOT_FOUND",
            Self::ACCESS_DENIED => "ACCESS_DENIED",
            Self::NO_RESPONSE => "NO_RESPONSE",
            Self::NO_MAPPING => "NO_MAPPING",
            Self::TIMEOUT => "TIMEOUT",
            Self::NOT_STARTED => "NOT_STARTED",
            Self::ALREADY_STARTED => "ALREADY_STARTED",
            Self::ABORTED => "ABORTED",
            Self::ICMP_ERROR => "ICMP_ERROR",
            Self::TFTP_ERROR => "TFTP_ERROR",
            Self::PROTOCOL_ERROR => "PROTOCOL_ERROR",
            Self::INCOMPATIBLE_VERSION => "INCOMPATIBLE_VERSION",
            Self::SECURITY_VIOLATION => "SECURITY_VIOLATION",
            Self::CRC_ERROR => "CRC_ERROR",
            Self::END_OF_MEDIA => "END_OF_MEDIA",
            Self::END_OF_FILE => "END_OF_FILE",
            Self::INVALID_LANGUAGE => "INVALID_LANGUAGE",
            Self::COMPROMISED_DATA => "COMPROMISED_DATA",
            Self::IP_ADDRESS_CONFLICT => "IP_ADDRESS_CONFLICT",
            Self::HTTP_ERROR => "HTTP_ERROR",

            _ => return write!(f, "{:#x}", self.0),
        };
        f.write_str(name)
    }
}
