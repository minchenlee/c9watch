//! CLI shortcuts use the public protocol and the same engine admission path.
use clap::Subcommand;
use serde_json::{json, Value};

#[derive(Subcommand)]
pub enum Action {
    /// Upsert a key; value is tagged JSON, e.g. '{"type":"number","v":42}'
    Set {
        session_key: String,
        key: String,
        value: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        unit: Option<String>,
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        ttl_seconds: Option<u64>,
    },
    /// Clear one namespace/name key
    Delete { session_key: String, key: String },
    /// List annotations, optionally filtered by target and namespace
    List {
        #[arg(long)]
        session_key: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
    },
}
fn request(action: Action) -> Result<(&'static str, Value), String> {
    let target = |key: String| json!({"kind":"session","sessionKey":key});
    Ok(match action {
        Action::Set {
            session_key,
            key,
            value,
            label,
            unit,
            format,
            ttl_seconds,
        } => {
            let value: crate::engine_proto::wire::AnnotationValue = serde_json::from_str(&value)
                .map_err(|e| format!("invalid tagged annotation value: {e}"))?;
            (
                "annotation.set",
                json!({"target":target(session_key),"key":key,"value":value,"label":label,"unit":unit,"format":format,"ttlSeconds":ttl_seconds}),
            )
        }
        Action::Delete { session_key, key } => (
            "annotation.delete",
            json!({"target":target(session_key),"key":key}),
        ),
        Action::List {
            session_key,
            namespace,
        } => (
            "annotation.list",
            json!({"target":session_key.map(target),"namespace":namespace}),
        ),
    })
}
pub fn run(action: Action, pretty: bool) -> Result<(), String> {
    use super::engine_client::{
        current_exe, ensure_engine, error_line, process_env, refuse_agent_caller, ConnectError,
        EngineConn,
    };
    use crate::engine::home::{engine_home, sock_path};

    let (method, params) = request(action)?;
    refuse_agent_caller(&process_env)?;
    let home = engine_home()?;
    ensure_engine(&home, &current_exe()?)?;
    let mut conn = EngineConn::connect(&sock_path(&home)).map_err(ConnectError::into_msg)?;
    let response = conn.call(method, params)?;
    if !response.ok {
        return Err(error_line(&response));
    }
    super::print_json(&response.result.unwrap_or_default(), pretty)
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn all_annotation_commands_route_to_cli_and_build_public_requests() {
        for args in [
            vec![
                "c9watch",
                "annotate",
                "set",
                "claudeCode:s",
                "ci/tests",
                r#"{"type":"number","v":42}"#,
            ],
            vec!["c9watch", "annotate", "delete", "claudeCode:s", "ci/tests"],
            vec![
                "c9watch",
                "annotate",
                "list",
                "--session-key",
                "claudeCode:s",
                "--namespace",
                "ci",
            ],
        ] {
            assert!(super::super::is_cli_invocation("annotate"));
            let cli = super::super::Cli::try_parse_from(args).unwrap();
            let super::super::Commands::Annotate { action } = cli.command else {
                panic!("annotate parsed as another command")
            };
            let (method, params) = request(action).unwrap();
            assert!(method.starts_with("annotation."));
            assert_eq!(params["target"]["sessionKey"], "claudeCode:s");
        }
    }
}
