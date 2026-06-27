use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Top-level proxy configuration, loaded from environment variables with defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub dump_dir: Option<PathBuf>,
    pub compression_enabled: bool,
    /// CCR storage backend configuration.
    pub ccr: CcrConfig,
    /// API key authentication configuration.
    pub auth: crate::auth::AuthConfig,
    /// Enable verbose dev-mode logging (request/response details, timing, token usage).
    pub dev_mode: bool,
}

/// CCR (Compress-Cache-Retrieve) storage configuration.
#[derive(Debug, Clone)]
pub struct CcrConfig {
    /// Backend type: "memory" (default) or "sqlite".
    pub backend: String,
    /// Path to SQLite database file (only used when backend = "sqlite").
    pub sqlite_path: Option<String>,
    /// TTL in seconds for stored entries (0 = never expire). Default: 1800 (30 min).
    pub ttl_seconds: u64,
    /// Background purge interval in seconds (SQLite only). Default: 300 (5 min).
    /// 0 disables background purge.
    pub purge_interval_secs: u64,
}

impl Default for CcrConfig {
    fn default() -> Self {
        CcrConfig {
            backend: "memory".to_string(),
            sqlite_path: None,
            ttl_seconds: 1800,
            purge_interval_secs: 300,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub log_level: String,
}

#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    /// Default upstream base URL (e.g., https://api.anthropic.com)
    pub base_url: String,
    /// Optional API key to forward (if the client doesn't provide one)
    pub api_key: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Max connections in the pool
    pub pool_max_connections: usize,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub upstream_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
    pub model_map: HashMap<String, String>,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Environment variables:
    /// - `PROXY_HOST` — bind address (default: 127.0.0.1)
    /// - `PROXY_PORT` — bind port (default: 8787)
    /// - `PROXY_LOG_LEVEL` — log level (default: info)
    /// - `PROXY_UPSTREAM` — default upstream base URL (default: https://api.anthropic.com)
    /// - `PROXY_API_KEY` — optional API key for upstream
    /// - `PROXY_TIMEOUT` — request timeout in seconds (default: 600)
    /// - `PROXY_POOL_MAX` — max connections in pool (default: 20)
    /// - `PROXY_DUMP_DIR` — directory for traffic dump (default: empty = disabled)
    /// - `COMPRESSION_ENABLED` — enable token compression (default: false)
    /// - `PROXY_DEV_MODE` — enable verbose dev logging (default: false)
    /// - `DEEPSEEK_UPSTREAM` — DeepSeek API base URL
    /// - `DEEPSEEK_API_KEY` — DeepSeek API key
    /// - `DEEPSEEK_DEFAULT_MODEL` — default DeepSeek model
    pub fn from_env() -> Result<Self, crate::error::AppError> {
        let host = env_or("PROXY_HOST", "127.0.0.1");
        let port: u16 = env_or("PROXY_PORT", "8787")
            .parse()
            .map_err(|e| crate::error::AppError::ConfigError(format!("invalid PROXY_PORT: {e}")))?;
        let bind_addr = SocketAddr::new(
            host.parse().map_err(|e| {
                crate::error::AppError::ConfigError(format!("invalid PROXY_HOST: {e}"))
            })?,
            port,
        );

        let server = ServerConfig {
            bind_addr,
            log_level: env_or("PROXY_LOG_LEVEL", "info"),
        };

        let upstream = UpstreamConfig {
            base_url: env_or("PROXY_UPSTREAM", "https://api.anthropic.com"),
            api_key: std::env::var("PROXY_API_KEY").ok(),
            timeout_secs: env_or("PROXY_TIMEOUT", "600").parse().unwrap_or(600),
            pool_max_connections: env_or("PROXY_POOL_MAX", "20").parse().unwrap_or(20),
        };

        // Build provider configs from env vars
        let mut providers = HashMap::new();

        // DeepSeek provider config — auto-enabled when either DEEPSEEK_UPSTREAM
        // or DEEPSEEK_API_KEY is set. If only the key is set, upstream defaults
        // to https://api.deepseek.com/anthropic.
        let ds_key = std::env::var("DEEPSEEK_API_KEY").ok();
        let ds_upstream = std::env::var("DEEPSEEK_UPSTREAM").ok();
        if ds_key.is_some() || ds_upstream.is_some() {
            let upstream_url = ds_upstream
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "https://api.deepseek.com/anthropic".to_string());
            providers.insert(
                "deepseek".to_string(),
                ProviderConfig {
                    upstream_url,
                    api_key: ds_key,
                    default_model: env_or("DEEPSEEK_DEFAULT_MODEL", "deepseek-v4-flash"),
                    model_map: parse_model_map("DEEPSEEK_MODEL_MAP"),
                },
            );
        }

        let dump_dir = std::env::var("PROXY_DUMP_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        let compression_enabled = std::env::var("COMPRESSION_ENABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let ccr = CcrConfig {
            backend: env_or("CCR_BACKEND", "memory"),
            sqlite_path: std::env::var("CCR_SQLITE_PATH")
                .ok()
                .filter(|s| !s.is_empty()),
            ttl_seconds: env_or("CCR_TTL_SECONDS", "1800").parse().unwrap_or(1800),
            purge_interval_secs: env_or("CCR_PURGE_INTERVAL_SECONDS", "300").parse().unwrap_or(300),
        };

        // Auth: parse comma-separated API tokens from PROXY_AUTH_TOKENS
        let auth_tokens: Vec<String> = std::env::var("PROXY_AUTH_TOKENS")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let auth = crate::auth::AuthConfig {
            tokens: auth_tokens,
        };
        if auth.is_enabled() {
            tracing::info!(
                "Auth ENABLED ({} token(s) configured)",
                auth.tokens.len()
            );
        }

        let dev_mode = std::env::var("PROXY_DEV_MODE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Ok(Config {
            server,
            upstream,
            providers,
            dump_dir,
            compression_enabled,
            ccr,
            auth,
            dev_mode,
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn parse_model_map(env_key: &str) -> HashMap<String, String> {
    std::env::var(env_key)
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.split(',')
                .filter_map(|pair| {
                    let mut parts = pair.splitn(2, '=');
                    match (parts.next(), parts.next()) {
                        (Some(k), Some(v)) if !k.is_empty() && !v.is_empty() => {
                            Some((k.trim().to_string(), v.trim().to_string()))
                        }
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Use a mutex to serialize env var tests (env vars are global state).
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn with_env_vars<F>(vars: &[(&str, &str)], test: F)
    where
        F: FnOnce(),
    {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Save and remove existing values, then set test values
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();

        // Clean all test keys first
        for (k, _) in vars {
            let _ = std::env::remove_var(k);
        }
        // Also clean keys we might read in from_env
        let all_config_keys = [
            "PROXY_HOST",
            "PROXY_PORT",
            "PROXY_LOG_LEVEL",
            "PROXY_UPSTREAM",
            "PROXY_API_KEY",
            "PROXY_TIMEOUT",
            "PROXY_POOL_MAX",
            "PROXY_DUMP_DIR",
            "COMPRESSION_ENABLED",
            "CCR_BACKEND",
            "CCR_SQLITE_PATH",
            "CCR_TTL_SECONDS",
            "CCR_PURGE_INTERVAL_SECONDS",
            "PROXY_AUTH_TOKENS",
            "DEEPSEEK_UPSTREAM",
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_DEFAULT_MODEL",
            "DEEPSEEK_MODEL_MAP",
        ];
        let saved_all: Vec<_> = all_config_keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in all_config_keys {
            let _ = std::env::remove_var(k);
        }

        // Set our test vars
        for (k, v) in vars {
            std::env::set_var(k, v);
        }

        test();

        // Restore all config keys
        for (k, v) in saved_all {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => {
                    let _ = std::env::remove_var(k);
                }
            }
        }
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => {
                    let _ = std::env::remove_var(k);
                }
            }
        }
    }

    #[test]
    fn test_defaults_when_no_env_vars() {
        // Protect against leftover env vars from other tests or parent shell
        with_env_vars(&[], || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.server.bind_addr.to_string(), "127.0.0.1:8787");
            assert_eq!(config.upstream.base_url, "https://api.anthropic.com");
            assert_eq!(config.upstream.timeout_secs, 600);
            assert_eq!(config.upstream.pool_max_connections, 20);
            assert!(!config.compression_enabled);
            assert!(!config.dev_mode);
            assert!(config.dump_dir.is_none());
            assert!(config.providers.is_empty());
        });
    }

    #[test]
    fn test_custom_host_and_port() {
        with_env_vars(&[("PROXY_HOST", "0.0.0.0"), ("PROXY_PORT", "9999")], || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.server.bind_addr.to_string(), "0.0.0.0:9999");
        });
    }

    #[test]
    fn test_custom_upstream_and_api_key() {
        with_env_vars(
            &[
                ("PROXY_UPSTREAM", "https://custom.api.com"),
                ("PROXY_API_KEY", "sk-test-key"),
                ("PROXY_TIMEOUT", "300"),
                ("PROXY_POOL_MAX", "10"),
            ],
            || {
                let config = Config::from_env().unwrap();
                assert_eq!(config.upstream.base_url, "https://custom.api.com");
                assert_eq!(config.upstream.api_key.as_deref(), Some("sk-test-key"));
                assert_eq!(config.upstream.timeout_secs, 300);
                assert_eq!(config.upstream.pool_max_connections, 10);
            },
        );
    }

    #[test]
    fn test_deepseek_auto_enabled_by_api_key_only() {
        with_env_vars(&[("DEEPSEEK_API_KEY", "sk-ds-key-only")], || {
            let config = Config::from_env().unwrap();
            assert_eq!(
                config.providers.len(),
                1,
                "DeepSeek should auto-enable with just API key"
            );
            let ds = config.providers.get("deepseek").unwrap();
            assert_eq!(
                ds.upstream_url, "https://api.deepseek.com/anthropic",
                "upstream should default when only API key is set"
            );
        });
    }

    #[test]
    fn test_deepseek_provider_enabled() {
        with_env_vars(
            &[
                ("DEEPSEEK_UPSTREAM", "https://api.deepseek.com/anthropic"),
                ("DEEPSEEK_API_KEY", "sk-ds-key"),
                ("DEEPSEEK_DEFAULT_MODEL", "deepseek-v4-pro"),
            ],
            || {
                let config = Config::from_env().unwrap();
                assert_eq!(config.providers.len(), 1);
                let ds = config.providers.get("deepseek").unwrap();
                assert_eq!(ds.upstream_url, "https://api.deepseek.com/anthropic");
                assert_eq!(ds.api_key.as_deref(), Some("sk-ds-key"));
                assert_eq!(ds.default_model, "deepseek-v4-pro");
            },
        );
    }

    #[test]
    fn test_model_map_parsing() {
        with_env_vars(
            &[
                ("DEEPSEEK_UPSTREAM", "https://api.deepseek.com/anthropic"),
                (
                    "DEEPSEEK_MODEL_MAP",
                    "sonnet=deepseek-v4-pro, opus=deepseek-v4-pro, haiku=deepseek-v4-flash",
                ),
            ],
            || {
                let config = Config::from_env().unwrap();
                let ds = config.providers.get("deepseek").unwrap();
                assert_eq!(ds.model_map.len(), 3);
                assert_eq!(
                    ds.model_map.get("sonnet").map(|s| s.as_str()),
                    Some("deepseek-v4-pro")
                );
                assert_eq!(
                    ds.model_map.get("opus").map(|s| s.as_str()),
                    Some("deepseek-v4-pro")
                );
                assert_eq!(
                    ds.model_map.get("haiku").map(|s| s.as_str()),
                    Some("deepseek-v4-flash")
                );
            },
        );
    }

    #[test]
    fn test_compression_enabled() {
        with_env_vars(&[("COMPRESSION_ENABLED", "true")], || {
            let config = Config::from_env().unwrap();
            assert!(config.compression_enabled);
        });

        with_env_vars(&[("COMPRESSION_ENABLED", "1")], || {
            let config = Config::from_env().unwrap();
            assert!(config.compression_enabled);
        });

        with_env_vars(&[("COMPRESSION_ENABLED", "false")], || {
            let config = Config::from_env().unwrap();
            assert!(!config.compression_enabled);
        });
    }

    #[test]
    fn test_dump_dir() {
        with_env_vars(&[("PROXY_DUMP_DIR", "/tmp/dumps")], || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.dump_dir, Some(PathBuf::from("/tmp/dumps")));
        });
    }

    #[test]
    fn test_empty_dump_dir_disabled() {
        with_env_vars(&[("PROXY_DUMP_DIR", "")], || {
            let config = Config::from_env().unwrap();
            assert!(config.dump_dir.is_none());
        });
    }

    #[test]
    fn test_parse_model_map_empty() {
        with_env_vars(
            &[
                ("DEEPSEEK_UPSTREAM", "https://api.deepseek.com/anthropic"),
                ("DEEPSEEK_MODEL_MAP", ""),
            ],
            || {
                let config = Config::from_env().unwrap();
                let ds = config.providers.get("deepseek").unwrap();
                assert!(ds.model_map.is_empty());
            },
        );
    }

    #[test]
    fn test_parse_model_map_with_spaces() {
        with_env_vars(
            &[
                ("DEEPSEEK_UPSTREAM", "https://api.deepseek.com/anthropic"),
                ("DEEPSEEK_MODEL_MAP", "a = b , c = d"),
            ],
            || {
                let config = Config::from_env().unwrap();
                let ds = config.providers.get("deepseek").unwrap();
                assert_eq!(ds.model_map.get("a").map(|s| s.as_str()), Some("b"));
                assert_eq!(ds.model_map.get("c").map(|s| s.as_str()), Some("d"));
            },
        );
    }
}
