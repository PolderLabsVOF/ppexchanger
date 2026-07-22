//! `lanchat` library crate.
//!
//! Modules:
//!   * [`crypto`]   — facade over the audited crypto crates
//!   * [`protocol`] — wire format (beacon + length-prefixed frames)
//!   * [`net`]      — TCP handshake
//!
//! Additional layers (UDP multicast discovery, TCP listener, dial, encrypted
//! session stream, identity persistence, peer DB, ratatui-driven UI, CLI wiring)
//! are planned but not yet implemented. See `~/.claude/plans/whimsical-orbiting-tower.md`.

pub mod crypto;
pub mod net;
pub mod protocol;