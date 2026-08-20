//! Local-credential detection for the two built-in provider instances.
//!
//! Detection is a pair of small file probes: is there a Codex OAuth file with
//! usable tokens, and is there a Claude CLI credentials file. It answers with a
//! [`DetectionStatus`] instead of an error, because "not logged in" is a state
//! the Owner should see in Settings, not a failure.
//!
//! Nothing here ever reads a token into the answer. `detail` carries a short
//! non-secret reason, `account_hint` a non-secret identity (a Codex account id,
//! a Claude subscription type or account email) — never credential material.

use std::path::{Path, PathBuf};

use hirsel_proto::DetectionStatus;
use serde::Deserialize;

/// The Codex OAuth file, relative to the home directory.
const CODEX_AUTH_RELATIVE: &str = ".codex/auth.json";
/// The Claude CLI credential locations, probed in order. The CLI has written
/// both over its life; the first that exists is the one that counts.
const CLAUDE_CREDENTIAL_RELATIVE: &[&str] = &[
    ".claude/.credentials.json",
    ".config/claude/.credentials.json",
];

/// The Codex OAuth file as both the boot path and detection read it. One
/// definition: `lash_runtime::provider` builds the live provider from it.
#[derive(Debug, Deserialize)]
pub(crate) struct CodexAuthFile {
    pub(crate) tokens: CodexAuthTokens,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CodexAuthTokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    #[serde(default)]
    pub(crate) account_id: Option<String>,
    #[serde(default)]
    pub(crate) expires_at: Option<u64>,
}

/// The Claude CLI credentials file. Both shapes the CLI has used are accepted;
/// only presence and non-emptiness of the token is ever consulted.
#[derive(Debug, Deserialize)]
struct ClaudeCredentials {
    #[serde(default, rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauth>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account: Option<ClaudeAccount>,
}

#[derive(Debug, Deserialize)]
struct ClaudeOauth {
    #[serde(default, rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default, rename = "subscriptionType")]
    subscription_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeAccount {
    #[serde(default)]
    email_address: Option<String>,
}

/// Probe `<home>/.codex/auth.json`. Detected requires both an access and a
/// refresh token, because either alone cannot carry a session.
pub async fn detect_codex(home: &Path) -> DetectionStatus {
    let path = home.join(CODEX_AUTH_RELATIVE);
    let display = path.display().to_string();
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return undetected(display, "no Codex auth file at this path");
    };
    let Ok(auth) = serde_json::from_str::<CodexAuthFile>(&text) else {
        return undetected(display, "Codex auth file is not the expected JSON shape");
    };
    if auth.tokens.access_token.is_empty() || auth.tokens.refresh_token.is_empty() {
        return undetected(display, "Codex auth file has no access or refresh token");
    }
    DetectionStatus {
        detected: true,
        path: display,
        account_hint: auth.tokens.account_id.filter(|hint| !hint.is_empty()),
        detail: None,
    }
}

/// Probe the Claude CLI credential locations in order and report the first one
/// that exists. `path` is the located file when found, and the first location
/// probed when nothing was.
pub async fn detect_claude(home: &Path) -> DetectionStatus {
    for relative in CLAUDE_CREDENTIAL_RELATIVE {
        let path = home.join(relative);
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let display = path.display().to_string();
        let Ok(credentials) = serde_json::from_str::<ClaudeCredentials>(&text) else {
            return undetected(display, "Claude credentials file is not valid JSON");
        };
        let oauth_token = credentials
            .claude_ai_oauth
            .as_ref()
            .and_then(|oauth| oauth.access_token.as_deref());
        let token = oauth_token
            .or(credentials.access_token.as_deref())
            .unwrap_or_default();
        if token.is_empty() {
            return undetected(display, "Claude credentials file has no access token");
        }
        let account_hint = credentials
            .claude_ai_oauth
            .and_then(|oauth| oauth.subscription_type)
            .or_else(|| {
                credentials
                    .account
                    .and_then(|account| account.email_address)
            })
            .filter(|hint| !hint.is_empty());
        return DetectionStatus {
            detected: true,
            path: display,
            account_hint,
            detail: None,
        };
    }
    undetected(
        home.join(CLAUDE_CREDENTIAL_RELATIVE[0])
            .display()
            .to_string(),
        "no Claude CLI credentials file at any known path",
    )
}

/// What detection reports when the host has no home directory to probe.
pub fn home_unset(relative_hint: &str) -> DetectionStatus {
    undetected(
        format!("~/{relative_hint}"),
        "HOME is not set, so local credentials cannot be located",
    )
}

/// The `~`-relative path each built-in probes, for the HOME-unset report.
pub fn codex_path_hint() -> &'static str {
    CODEX_AUTH_RELATIVE
}

pub fn claude_path_hint() -> &'static str {
    CLAUDE_CREDENTIAL_RELATIVE[0]
}

/// The Codex tokens the boot path needs, read from an explicit home directory.
pub(crate) async fn read_codex_tokens(home: &Path) -> Result<CodexAuthTokens, String> {
    let auth_path = home.join(CODEX_AUTH_RELATIVE);
    let text = tokio::fs::read_to_string(&auth_path)
        .await
        .map_err(|error| format!("failed to read {}: {error}", auth_path.display()))?;
    let auth: CodexAuthFile =
        serde_json::from_str(&text).map_err(|error| format!("invalid Codex auth JSON: {error}"))?;
    if auth.tokens.access_token.is_empty() || auth.tokens.refresh_token.is_empty() {
        return Err("Codex OAuth tokens are missing access_token or refresh_token".to_string());
    }
    Ok(auth.tokens)
}

/// The process home directory, when the environment names one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn undetected(path: String, detail: &str) -> DetectionStatus {
    DetectionStatus {
        detected: false,
        path,
        account_hint: None,
        detail: Some(detail.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake token that must never surface in what detection reports.
    const FAKE_TOKEN: &str = "fake-token-must-not-leak";

    fn write(home: &Path, relative: &str, contents: &str) {
        let path = home.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn assert_no_token(status: &DetectionStatus) {
        let repr = format!("{status:?}");
        assert!(
            !repr.contains(FAKE_TOKEN),
            "detection leaked a token: {repr}"
        );
    }

    #[tokio::test]
    async fn codex_detection_reports_the_account_without_the_tokens() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            CODEX_AUTH_RELATIVE,
            &format!(
                r#"{{"tokens":{{"access_token":"{FAKE_TOKEN}","refresh_token":"{FAKE_TOKEN}-r","account_id":"acct-42"}}}}"#
            ),
        );

        let status = detect_codex(dir.path()).await;
        assert!(status.detected);
        assert_eq!(status.account_hint.as_deref(), Some("acct-42"));
        assert_eq!(status.detail, None);
        assert!(status.path.ends_with(CODEX_AUTH_RELATIVE));
        assert_no_token(&status);
    }

    #[tokio::test]
    async fn codex_detection_reports_missing_and_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = detect_codex(dir.path()).await;
        assert!(!missing.detected);
        assert!(missing.detail.is_some());
        assert!(missing.account_hint.is_none());

        write(dir.path(), CODEX_AUTH_RELATIVE, "{ not json");
        let malformed = detect_codex(dir.path()).await;
        assert!(!malformed.detected);
        assert!(
            malformed
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("expected JSON shape"))
        );

        write(
            dir.path(),
            CODEX_AUTH_RELATIVE,
            &format!(r#"{{"tokens":{{"access_token":"{FAKE_TOKEN}","refresh_token":""}}}}"#),
        );
        let half = detect_codex(dir.path()).await;
        assert!(!half.detected);
        assert_no_token(&half);
    }

    #[tokio::test]
    async fn claude_detection_accepts_both_shapes_at_both_locations() {
        let primary = tempfile::tempdir().unwrap();
        write(
            primary.path(),
            CLAUDE_CREDENTIAL_RELATIVE[0],
            &format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{FAKE_TOKEN}","subscriptionType":"max"}}}}"#
            ),
        );
        let status = detect_claude(primary.path()).await;
        assert!(status.detected);
        assert_eq!(status.account_hint.as_deref(), Some("max"));
        assert!(status.path.ends_with(CLAUDE_CREDENTIAL_RELATIVE[0]));
        assert_no_token(&status);

        let fallback = tempfile::tempdir().unwrap();
        write(
            fallback.path(),
            CLAUDE_CREDENTIAL_RELATIVE[1],
            &format!(
                r#"{{"access_token":"{FAKE_TOKEN}","account":{{"email_address":"owner@example.com"}}}}"#
            ),
        );
        let status = detect_claude(fallback.path()).await;
        assert!(status.detected);
        assert_eq!(status.account_hint.as_deref(), Some("owner@example.com"));
        assert!(status.path.ends_with(CLAUDE_CREDENTIAL_RELATIVE[1]));
        assert_no_token(&status);
    }

    #[tokio::test]
    async fn claude_detection_reports_the_first_probed_path_when_nothing_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let status = detect_claude(dir.path()).await;
        assert!(!status.detected);
        assert!(status.path.ends_with(CLAUDE_CREDENTIAL_RELATIVE[0]));
        assert!(status.detail.is_some());
        assert!(status.account_hint.is_none());
    }

    #[test]
    fn a_missing_home_is_a_status_not_a_failure() {
        let status = home_unset(codex_path_hint());
        assert!(!status.detected);
        assert_eq!(status.path, "~/.codex/auth.json");
        assert!(
            status
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("HOME"))
        );
    }
}
