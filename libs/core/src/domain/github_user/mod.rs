pub mod ports;

use chrono::{DateTime, Duration, Utc};
use matos_common::{CoreError, OrganizationId};

use crate::domain::agent::SecretToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitIdentity {
    #[default]
    App,
    Member,
}

impl GitIdentity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Member => "member",
        }
    }
}

impl TryFrom<&str> for GitIdentity {
    type Error = CoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "app" => Ok(Self::App),
            "member" => Ok(Self::Member),
            other => Err(CoreError::Internal(format!(
                "unknown git identity {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTokens {
    pub access_token: SecretToken,
    pub expires_at: Option<DateTime<Utc>>,
    pub refresh_token: Option<SecretToken>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
}

impl UserTokens {
    pub fn needs_refresh(&self, now: DateTime<Utc>, min_validity: Duration) -> bool {
        self.expires_at
            .is_some_and(|expires_at| expires_at < now + min_validity)
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    pub fn renewed_by(self, fresh: Self) -> Self {
        match fresh.refresh_token {
            Some(_) => fresh,
            None => Self {
                refresh_token: self.refresh_token,
                refresh_expires_at: self.refresh_expires_at,
                ..fresh
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubUser {
    pub id: i64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConnection {
    pub organization_id: OrganizationId,
    pub subject: String,
    pub github_user_id: i64,
    pub github_login: String,
    pub tokens: UserTokens,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalToken {
    pub organization_id: OrganizationId,
    pub subject: String,
    pub github_user_id: i64,
    pub github_login: String,
    pub token: SecretToken,
    pub preview: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PersonalToken {
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        self.active && self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalTokenCheck {
    pub user: GithubUser,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalTokenStatus {
    pub login: String,
    pub preview: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub active: bool,
}

impl From<&PersonalToken> for PersonalTokenStatus {
    fn from(token: &PersonalToken) -> Self {
        Self {
            login: token.github_login.clone(),
            preview: token.preview.clone(),
            expires_at: token.expires_at,
            active: token.active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConnectionStatus {
    pub connected: bool,
    pub login: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub personal_token: Option<PersonalTokenStatus>,
}

impl GithubConnectionStatus {
    pub fn disconnected() -> Self {
        Self {
            connected: false,
            login: None,
            connected_at: None,
            personal_token: None,
        }
    }
}

impl From<&GithubConnection> for GithubConnectionStatus {
    fn from(connection: &GithubConnection) -> Self {
        Self {
            connected: true,
            login: Some(connection.github_login.clone()),
            connected_at: Some(connection.created_at),
            personal_token: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(expires_in: Option<i64>, refresh: Option<&str>) -> UserTokens {
        UserTokens {
            access_token: SecretToken::new("ghu_access"),
            expires_at: expires_in.map(|seconds| Utc::now() + Duration::seconds(seconds)),
            refresh_token: refresh.map(SecretToken::new),
            refresh_expires_at: None,
        }
    }

    #[test]
    fn the_identity_round_trips_and_defaults_to_the_app() {
        for identity in [GitIdentity::App, GitIdentity::Member] {
            assert_eq!(GitIdentity::try_from(identity.as_str()).unwrap(), identity);
        }
        assert!(GitIdentity::try_from("user").is_err());
        assert_eq!(GitIdentity::default(), GitIdentity::App);
    }

    #[test]
    fn a_token_is_renewed_when_it_expires_before_the_minimum_validity() {
        let now = Utc::now();
        let window = Duration::minutes(40);
        assert!(tokens(Some(60), Some("r")).needs_refresh(now, window));
        assert!(tokens(Some(39 * 60), Some("r")).needs_refresh(now, window));
        assert!(!tokens(Some(41 * 60), Some("r")).needs_refresh(now, window));
    }

    #[test]
    fn a_token_without_expiry_is_used_as_is() {
        let token = tokens(None, None);
        assert!(!token.needs_refresh(Utc::now(), Duration::days(365)));
        assert!(!token.is_expired(Utc::now()));
    }

    #[test]
    fn a_token_is_expired_once_its_time_has_passed() {
        assert!(tokens(Some(-1), None).is_expired(Utc::now()));
        assert!(!tokens(Some(60), None).is_expired(Utc::now()));
    }

    #[test]
    fn the_rotated_refresh_token_replaces_the_old_one_and_a_missing_one_keeps_it() {
        let rotated = tokens(Some(60), Some("new")).renewed_by(tokens(Some(3600), Some("rotated")));
        assert_eq!(rotated.refresh_token.unwrap().expose(), "rotated");
        let kept = tokens(Some(60), Some("old")).renewed_by(tokens(Some(3600), None));
        assert_eq!(kept.refresh_token.unwrap().expose(), "old");
    }

    #[test]
    fn debug_never_shows_a_token() {
        let connection = GithubConnection {
            organization_id: OrganizationId::new(),
            subject: "user-1".into(),
            github_user_id: 1,
            github_login: "octo".into(),
            tokens: tokens(Some(60), Some("ghr_refresh_leak")),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let shown = format!("{connection:?}");
        assert!(!shown.contains("ghu_access"), "{shown}");
        assert!(!shown.contains("ghr_refresh_leak"), "{shown}");
    }

    #[test]
    fn a_connection_shows_its_login_and_when_it_was_made() {
        let now = Utc::now();
        let connection = GithubConnection {
            organization_id: OrganizationId::new(),
            subject: "user-1".into(),
            github_user_id: 1,
            github_login: "octo".into(),
            tokens: tokens(None, None),
            created_at: now,
            updated_at: now,
        };
        let status = GithubConnectionStatus::from(&connection);
        assert!(status.connected);
        assert_eq!(status.login.as_deref(), Some("octo"));
        assert_eq!(status.connected_at, Some(now));
        assert!(!GithubConnectionStatus::disconnected().connected);
        assert!(status.personal_token.is_none());
    }

    fn personal(active: bool, expires_in: Option<i64>) -> PersonalToken {
        let now = Utc::now();
        PersonalToken {
            organization_id: OrganizationId::new(),
            subject: "user-1".into(),
            github_user_id: 1,
            github_login: "octo".into(),
            token: SecretToken::new("github_pat_leak"),
            preview: "…leak".into(),
            expires_at: expires_in.map(|seconds| now + Duration::seconds(seconds)),
            active,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_personal_token_is_usable_when_active_and_not_expired() {
        let now = Utc::now();
        assert!(personal(true, None).is_usable(now));
        assert!(personal(true, Some(60)).is_usable(now));
        assert!(!personal(true, Some(-1)).is_usable(now));
        assert!(!personal(false, None).is_usable(now));
        assert!(!personal(false, Some(60)).is_usable(now));
    }

    #[test]
    fn a_personal_token_debug_and_status_never_show_the_token() {
        let token = personal(true, None);
        let shown = format!("{token:?}");
        assert!(!shown.contains("github_pat_leak"), "{shown}");
        let status = PersonalTokenStatus::from(&token);
        assert_eq!(status.login, "octo");
        assert_eq!(status.preview, "…leak");
        assert!(status.active);
        assert!(!format!("{status:?}").contains("github_pat_leak"));
    }
}
