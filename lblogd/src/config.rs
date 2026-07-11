//! The `lblogd` config file: one TOML file drives both the NomadNet node and
//! the web server.
//!
//! ```toml
//! data_dir  = "/var/lib/lblogd"
//! posts_dir = "/var/lib/lblogd/posts"
//!
//! [node]
//! instance_name          = "leviculum"
//! display_name           = "leviculum.network dev blog"
//! announce_interval_secs = 21600        # optional
//!
//! [web]
//! domains            = ["leviculum.network"]
//! acme_contact_email = "you@example.org"
//! acme_staging       = true
//! http_bind          = "0.0.0.0:80"     # optional, this is the default
//! https_bind         = "0.0.0.0:443"    # optional, this is the default
//! ```
//!
//! [`Config::blog_node_config`] and [`Config::web_config`] map the file onto
//! the two component configs; the ACME cache directory is derived as
//! `<data_dir>/acme` rather than configured separately.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::node::BlogNodeConfig;
use crate::web::WebConfig;

/// Errors from loading the config file.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Reading the file failed (missing file, permissions).
    #[error("reading config {path}: {source}")]
    Read {
        /// The config file path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file is not valid TOML or is missing a required field.
    #[error("config {path}: {source}")]
    Parse {
        /// The config file path.
        path: String,
        /// The TOML error, which names the offending field or line.
        source: toml::de::Error,
    },
}

/// The parsed config file.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Base data directory: identities, node storage, and the ACME cache
    /// live under it.
    pub data_dir: PathBuf,
    /// Directory of Markdown posts, served by both the node and the web
    /// server.
    pub posts_dir: PathBuf,
    /// NomadNet node settings.
    pub node: NodeSection,
    /// Web server settings.
    pub web: WebSection,
}

/// The `[node]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeSection {
    /// Shared instance name of the running `lnsd` daemon; must match the
    /// daemon's `instance_name`.
    pub instance_name: String,
    /// Display name announced over Reticulum.
    pub display_name: String,
    /// Re-announce cadence in seconds; defaults to
    /// [`BlogNodeConfig::default_announce_interval`].
    pub announce_interval_secs: Option<u64>,
}

/// The `[web]` section.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    /// Domains the HTTPS certificate covers.
    pub domains: Vec<String>,
    /// Contact email for the ACME account.
    pub acme_contact_email: String,
    /// Use the Let's Encrypt staging directory (untrusted test certificates,
    /// generous rate limits). Required so production is a deliberate choice.
    pub acme_staging: bool,
    /// Plain HTTP listen address, redirect only.
    #[serde(default = "default_http_bind")]
    pub http_bind: SocketAddr,
    /// HTTPS listen address.
    #[serde(default = "default_https_bind")]
    pub https_bind: SocketAddr,
}

fn default_http_bind() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 80))
}

fn default_https_bind() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 443))
}

impl Config {
    /// Load and parse the config file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// The NomadNet node config this file describes.
    pub fn blog_node_config(&self) -> BlogNodeConfig {
        BlogNodeConfig {
            instance_name: self.node.instance_name.clone(),
            data_dir: self.data_dir.clone(),
            posts_dir: self.posts_dir.clone(),
            display_name: self.node.display_name.clone(),
            announce_interval: self
                .node
                .announce_interval_secs
                .map(Duration::from_secs)
                .unwrap_or_else(BlogNodeConfig::default_announce_interval),
        }
    }

    /// The web server config this file describes.
    pub fn web_config(&self) -> WebConfig {
        WebConfig {
            domains: self.web.domains.clone(),
            acme_cache_dir: self.data_dir.join("acme"),
            acme_contact_email: self.web.acme_contact_email.clone(),
            acme_staging: self.web.acme_staging,
            http_bind: self.web.http_bind,
            https_bind: self.web.https_bind,
            posts_dir: self.posts_dir.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        data_dir  = "/var/lib/lblogd"
        posts_dir = "/var/lib/lblogd/posts"

        [node]
        instance_name          = "leviculum"
        display_name           = "leviculum.network dev blog"
        announce_interval_secs = 3600

        [web]
        domains            = ["leviculum.network", "www.leviculum.network"]
        acme_contact_email = "ops@example.org"
        acme_staging       = true
        http_bind          = "127.0.0.1:8080"
        https_bind         = "127.0.0.1:8443"
    "#;

    #[test]
    fn sample_parses() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/lblogd"));
        assert_eq!(config.posts_dir, PathBuf::from("/var/lib/lblogd/posts"));
        assert_eq!(config.node.instance_name, "leviculum");
        assert_eq!(config.node.display_name, "leviculum.network dev blog");
        assert_eq!(config.node.announce_interval_secs, Some(3600));
        assert_eq!(
            config.web.domains,
            vec!["leviculum.network", "www.leviculum.network"]
        );
        assert_eq!(config.web.acme_contact_email, "ops@example.org");
        assert!(config.web.acme_staging);
        assert_eq!(config.web.http_bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.web.https_bind, "127.0.0.1:8443".parse().unwrap());
    }

    #[test]
    fn blog_node_config_maps_fields() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        let node = config.blog_node_config();
        assert_eq!(node.instance_name, "leviculum");
        assert_eq!(node.data_dir, PathBuf::from("/var/lib/lblogd"));
        assert_eq!(node.posts_dir, PathBuf::from("/var/lib/lblogd/posts"));
        assert_eq!(node.display_name, "leviculum.network dev blog");
        assert_eq!(node.announce_interval, Duration::from_secs(3600));
    }

    #[test]
    fn announce_interval_defaults_when_omitted() {
        let sample = SAMPLE.replace("announce_interval_secs = 3600", "");
        let config: Config = toml::from_str(&sample).unwrap();
        assert_eq!(config.node.announce_interval_secs, None);
        assert_eq!(
            config.blog_node_config().announce_interval,
            BlogNodeConfig::default_announce_interval()
        );
    }

    #[test]
    fn web_config_maps_fields_and_derives_acme_cache_dir() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        let web = config.web_config();
        assert_eq!(
            web.domains,
            vec!["leviculum.network", "www.leviculum.network"]
        );
        assert_eq!(web.acme_cache_dir, PathBuf::from("/var/lib/lblogd/acme"));
        assert_eq!(web.acme_contact_email, "ops@example.org");
        assert!(web.acme_staging);
        assert_eq!(web.http_bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(web.https_bind, "127.0.0.1:8443".parse().unwrap());
        assert_eq!(web.posts_dir, PathBuf::from("/var/lib/lblogd/posts"));
    }

    #[test]
    fn binds_default_to_80_and_443() {
        let sample = SAMPLE
            .replace("http_bind          = \"127.0.0.1:8080\"", "")
            .replace("https_bind         = \"127.0.0.1:8443\"", "");
        let config: Config = toml::from_str(&sample).unwrap();
        assert_eq!(config.web.http_bind, "0.0.0.0:80".parse().unwrap());
        assert_eq!(config.web.https_bind, "0.0.0.0:443".parse().unwrap());
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
        assert!(err.to_string().contains("does-not-exist.toml"), "{err}");
    }

    #[test]
    fn invalid_toml_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "data_dir = [unclosed").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        assert!(err.to_string().contains("bad.toml"), "{err}");
    }

    #[test]
    fn missing_required_field_names_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.toml");
        let sample = SAMPLE.replace("instance_name          = \"leviculum\"", "");
        std::fs::write(&path, sample).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        assert!(err.to_string().contains("instance_name"), "{err}");
    }

    #[test]
    fn unknown_field_is_a_parse_error() {
        let sample = format!("{SAMPLE}\ntypo_field = 1\n");
        let err = toml::from_str::<Config>(&sample).unwrap_err();
        assert!(err.to_string().contains("typo_field"), "{err}");
    }
}
