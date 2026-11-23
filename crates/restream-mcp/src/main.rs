//! Standalone MCP server bootstrap for `restream`.
//! This binary owns CLI/env-driven startup concerns such as transport mode,
//! compatibility policy, auth wiring, and backend selection before handing
//! control to the shared MCP transport/runtime layers.

#[cfg(feature = "mcp-server")]
use std::sync::Arc;

#[cfg(feature = "mcp-server")]
fn usage() -> &'static str {
    "usage: restream-mcp [--print-tools] [--stdio] [--bind <addr>]"
}

#[cfg(feature = "mcp-server")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompatibilityMode {
    Strict,
    Warn,
    Off,
}

#[cfg(feature = "mcp-server")]
fn compatibility_mode_from_env() -> Result<CompatibilityMode, String> {
    match std::env::var("RESTREAM_MCP_VERSION_CHECK") {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" | "strict" => Ok(CompatibilityMode::Strict),
            "warn" => Ok(CompatibilityMode::Warn),
            "off" => Ok(CompatibilityMode::Off),
            other => Err(format!(
                "invalid RESTREAM_MCP_VERSION_CHECK='{other}' (expected strict, warn, or off)"
            )),
        },
        Err(_) => Ok(CompatibilityMode::Strict),
    }
}

#[cfg(all(feature = "mcp-server", feature = "mcp-http-backend"))]
fn auth_from_env() -> restream_mcp::http::HttpAgentAuth {
    use restream_mcp::http::HttpAgentAuth;

    if let Ok(cookie) = std::env::var("RESTREAM_AGENT_SESSION_COOKIE") {
        let cookie = cookie.trim();
        return if cookie.is_empty() {
            HttpAgentAuth::None
        } else {
            HttpAgentAuth::SessionCookie(cookie.to_owned())
        };
    }
    if let Ok(token) = std::env::var("RESTREAM_AGENT_BEARER_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return HttpAgentAuth::BearerToken(token.to_owned());
        }
    }
    HttpAgentAuth::None
}

#[cfg(not(feature = "mcp-server"))]
fn main() {
    eprintln!(
        "restream-mcp was built without the 'mcp-server' feature. Rebuild with --features mcp-server."
    );
    std::process::exit(1);
}

#[cfg(feature = "mcp-server")]
fn main() {
    let mut args = std::env::args().skip(1);
    let mut print_tools = false;
    let mut stdio = false;
    let mut bind_addr = "127.0.0.1:4040".to_string();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--print-tools" => print_tools = true,
            "--stdio" => stdio = true,
            "--bind" => {
                let Some(value) = args.next() else {
                    eprintln!("{}", usage());
                    std::process::exit(2);
                };
                bind_addr = value;
            }
            _ => {
                eprintln!("{}", usage());
                std::process::exit(2);
            }
        }
    }

    if print_tools {
        for tool in restream_mcp::tool_catalog() {
            println!(
                "{}\tfeature={}\tcompiledIn={}",
                tool.name, tool.requires_feature, tool.compiled_in
            );
        }
        return;
    }

    #[cfg(not(feature = "mcp-http-backend"))]
    {
        eprintln!("restream-mcp requires the 'mcp-http-backend' feature in this scaffold to run.");
        std::process::exit(1);
    }

    #[cfg(feature = "mcp-http-backend")]
    {
        let compatibility_mode = match compatibility_mode_from_env() {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        let base_url = std::env::var("RESTREAM_AGENT_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:3030".to_string());
        let auth = auth_from_env();
        let allowed_origins = std::env::var("RESTREAM_MCP_ALLOWED_ORIGINS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let backend = match restream_mcp::http::HttpAgentBackend::new(base_url, auth) {
            Ok(backend) => backend,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        };
        let config = restream_mcp::TransportConfig {
            mode: if stdio {
                restream_mcp::TransportMode::Stdio
            } else {
                restream_mcp::TransportMode::StreamableHttp
            },
            bind_addr,
            allowed_origins,
        };

        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("failed to build the tokio runtime: {error}");
                std::process::exit(1);
            }
        };
        check_target_compatibility(&runtime, &backend, compatibility_mode);
        let result = runtime.block_on(restream_mcp::run_server(config, Arc::new(backend)));
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(all(feature = "mcp-server", feature = "mcp-http-backend"))]
fn check_target_compatibility(
    runtime: &tokio::runtime::Runtime,
    backend: &restream_mcp::http::HttpAgentBackend,
    mode: CompatibilityMode,
) {
    if mode == CompatibilityMode::Off {
        return;
    }
    if let Err(error) = runtime.block_on(backend.verify_target_compatibility()) {
        if mode == CompatibilityMode::Warn {
            eprintln!("warning: {error}");
        } else {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
