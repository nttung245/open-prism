//! Accounts of server mode (`user-accounts` spec).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What an account may do on the whole server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum UserRole {
    /// Manages accounts, shared providers and default models.
    Admin,
    User,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// A signed-in account, as the web app sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        for role in [UserRole::Admin, UserRole::User] {
            assert_eq!(UserRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(UserRole::parse("root"), None);
    }
}
