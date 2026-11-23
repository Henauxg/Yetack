use serde::Deserialize;
use serde_json::Value;

use crate::parse_error::ParseError;

/// The answer to `mcp_authenticate`: how the server's OAuth sign-in goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSignInStart {
    /// Claude Code needs no sign-in from the user, as it already holds a token.
    NotNeeded,
    /// Open `url`. The sign-in ends in the browser, as for a claude.ai connector, and Claude Code
    /// expects no address back.
    InBrowser { url: String },
    /// Open `url`. Once the user approves, the browser goes to an address that carries the
    /// authorization code: `http://localhost:<port>/callback`, where Claude Code listens on its own
    /// machine. `port` is `None` when Claude Code did not say it.
    Callback { url: String, port: Option<u16> },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    auth_url: Option<String>,
    #[serde(default)]
    callback_expected: bool,
    callback_port: Option<u16>,
}

impl TryFrom<&Value> for McpSignInStart {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        let answer = Answer::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "MCP sign-in",
            error,
        })?;
        Ok(match answer.auth_url {
            None => Self::NotNeeded,
            Some(url) if answer.callback_expected => Self::Callback {
                url,
                port: answer.callback_port,
            },
            Some(url) => Self::InBrowser { url },
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_localhost_answer_gives_its_port() {
        let body = json!({
            "authUrl": "https://wiki.example/authorize?state=s1",
            "requiresUserAction": true,
            "callbackExpected": true,
            "redirectScheme": "localhost",
            "state": "s1",
            "callbackPort": 50123
        });

        assert_eq!(
            McpSignInStart::try_from(&body).ok(),
            Some(McpSignInStart::Callback {
                url: "https://wiki.example/authorize?state=s1".to_owned(),
                port: Some(50123),
            })
        );
    }

    #[test]
    fn a_connector_answer_ends_in_the_browser() {
        let body = json!({
            "authUrl": "https://claude.example/connect",
            "requiresUserAction": true,
            "callbackExpected": false
        });

        assert_eq!(
            McpSignInStart::try_from(&body).ok(),
            Some(McpSignInStart::InBrowser {
                url: "https://claude.example/connect".to_owned(),
            })
        );
    }

    #[test]
    fn an_answer_without_a_link_needs_no_sign_in() {
        let body = json!({"requiresUserAction": false, "callbackExpected": false});

        assert_eq!(
            McpSignInStart::try_from(&body).ok(),
            Some(McpSignInStart::NotNeeded)
        );
    }
}
