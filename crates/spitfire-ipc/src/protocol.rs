use serde::{Deserialize, Serialize};

/// A request sent over the control socket. One request per connection —
/// `spitfirectl` connects, sends one line, reads one response line, exits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    /// `spitfirectl reload` — drops the current Lua state and re-runs
    /// `config.lua` from scratch, same as `spitfire.reload()`.
    ReloadConfig,
    /// `spitfirectl quit` — same as `spitfire.quit()`.
    Quit,
    /// `spitfirectl layout <tile|floating|fibonacci|monocle>`.
    SetLayout { mode: String },
    /// `spitfirectl list-windows`.
    ListWindows,
    /// `spitfirectl list-outputs`.
    ListOutputs,
    /// `spitfirectl workspace focus <n>` — 1-based, matching the Lua
    /// `spitfire.workspace.focus(n)` API.
    FocusWorkspace { index: usize },
    /// `spitfirectl list-workspaces`.
    ListWorkspaces,
}

/// The single response line for a [`Request`]. `data` carries whatever
/// payload the command produces (a JSON array for the `list-*` commands);
/// `error` is set instead of `data` when `ok` is `false`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Response {
            ok: true,
            data: None,
            error: None,
        }
    }

    pub fn ok_data(data: serde_json::Value) -> Self {
        Response {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Response {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_json() {
        let req = Request::SetLayout { mode: "tile".into() };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"command":"set-layout","mode":"tile"}"#);
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);
    }

    #[test]
    fn unit_variant_serializes_to_bare_command_tag() {
        let json = serde_json::to_string(&Request::ReloadConfig).unwrap();
        assert_eq!(json, r#"{"command":"reload-config"}"#);
    }

    #[test]
    fn focus_workspace_round_trips_through_json() {
        let req = Request::FocusWorkspace { index: 3 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"command":"focus-workspace","index":3}"#);
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);
    }

    #[test]
    fn response_omits_absent_fields() {
        let json = serde_json::to_string(&Response::ok()).unwrap();
        assert_eq!(json, r#"{"ok":true}"#);
    }

    #[test]
    fn error_response_round_trips() {
        let resp = Response::err("something broke");
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }
}
