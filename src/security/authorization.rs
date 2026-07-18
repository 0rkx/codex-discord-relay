#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityContext {
    pub owner_id: u64,
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    /// Must be calculated from actual Discord overwrites, not user input.
    pub is_private_channel: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustBoundary {
    pub owner_id: u64,
    pub guild_id: u64,
}

impl TrustBoundary {
    /// # Errors
    ///
    /// Returns [`AccessDenied::InvalidConfiguration`] for zero-valued Discord ids.
    pub fn new(owner_id: u64, guild_id: u64) -> Result<Self, AccessDenied> {
        if owner_id == 0 || guild_id == 0 {
            return Err(AccessDenied::InvalidConfiguration);
        }
        Ok(Self { owner_id, guild_id })
    }

    /// # Errors
    ///
    /// Returns the precise trust-boundary violation. Authorization always fails closed.
    pub fn authorize(&self, context: &SecurityContext) -> Result<(), AccessDenied> {
        if context.owner_id != self.owner_id {
            return Err(AccessDenied::WrongOwner);
        }
        if context.guild_id != Some(self.guild_id) {
            return Err(AccessDenied::WrongGuild);
        }
        if !context.is_private_channel {
            return Err(AccessDenied::ChannelNotPrivate);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum AccessDenied {
    #[error("security trust boundary is invalid")]
    InvalidConfiguration,
    #[error("GOD mode is restricted to the configured owner")]
    WrongOwner,
    #[error("GOD mode is restricted to the configured guild")]
    WrongGuild,
    #[error("GOD mode requires a private channel")]
    ChannelNotPrivate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_exact_owner_guild_and_private_channel() {
        let policy = TrustBoundary::new(1, 2).unwrap();
        let valid = SecurityContext {
            owner_id: 1,
            guild_id: Some(2),
            channel_id: 3,
            is_private_channel: true,
        };
        assert_eq!(policy.authorize(&valid), Ok(()));
        assert_eq!(
            policy.authorize(&SecurityContext {
                is_private_channel: false,
                ..valid
            }),
            Err(AccessDenied::ChannelNotPrivate)
        );
        assert_eq!(
            policy.authorize(&SecurityContext {
                owner_id: 9,
                ..valid
            }),
            Err(AccessDenied::WrongOwner)
        );
    }
}
