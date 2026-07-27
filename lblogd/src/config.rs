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
//! For a local development run, set `acme = false` in `[web]`. The HTTP
//! listener then serves the blog directly and no HTTPS listener is opened,
//! which is the only way to run the server on a machine without a publicly
//! reachable domain:
//!
//! ```toml
//! [web]
//! acme      = false
//! http_bind = "127.0.0.1:8080"
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
use crate::web::{AcmeSettings, WebConfig};

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
    /// The file parses but the field combination is not usable, e.g. ACME is
    /// enabled without the fields it needs.
    #[error("config {path}: {message}")]
    Invalid {
        /// The config file path.
        path: String,
        /// What is wrong, naming the offending field.
        message: String,
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
///
/// The three ACME fields are optional at the TOML level but required once
/// [`acme`](Self::acme) is on; [`Config::validate`] enforces that. Keeping
/// them out of serde's required set is what lets a development config omit
/// them entirely instead of filling in meaningless placeholders.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    /// Obtain HTTPS certificates from Let's Encrypt. Defaults to `true`, so
    /// existing deployment configs keep working unchanged; set it to `false`
    /// for a local development run on a machine with no publicly reachable
    /// domain.
    #[serde(default = "default_acme")]
    pub acme: bool,
    /// Domains the HTTPS certificate covers. Required when `acme` is on.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Contact email for the ACME account. Required when `acme` is on.
    pub acme_contact_email: Option<String>,
    /// Use the Let's Encrypt staging directory (untrusted test certificates,
    /// generous rate limits). Required when `acme` is on, so production stays
    /// a deliberate choice.
    pub acme_staging: Option<bool>,
    /// Plain HTTP listen address. Redirect-only when `acme` is on, the blog
    /// itself when it is off.
    #[serde(default = "default_http_bind")]
    pub http_bind: SocketAddr,
    /// HTTPS listen address. Unused, and never bound, when `acme` is off.
    #[serde(default = "default_https_bind")]
    pub https_bind: SocketAddr,
}

fn default_acme() -> bool {
    true
}

fn default_http_bind() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 80))
}

fn default_https_bind() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 443))
}

impl Config {
    /// Load, parse and validate the config file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        config.validate(path)?;
        Ok(config)
    }

    /// Reject field combinations serde cannot express: with ACME on, the
    /// three certificate fields are mandatory. Without it they are ignored,
    /// so their absence is not an error.
    fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        if !self.web.acme {
            return Ok(());
        }
        let invalid = |message: &str| ConfigError::Invalid {
            path: path.display().to_string(),
            message: message.to_string(),
        };
        if self.web.domains.is_empty() {
            return Err(invalid(
                "web.domains must list at least one domain when web.acme is true",
            ));
        }
        if self.web.acme_contact_email.is_none() {
            return Err(invalid(
                "web.acme_contact_email is required when web.acme is true",
            ));
        }
        if self.web.acme_staging.is_none() {
            return Err(invalid(
                "web.acme_staging is required when web.acme is true: \
                 set it to true for Let's Encrypt staging, false for production",
            ));
        }
        Ok(())
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

    /// The web server config this file describes. `acme: None` selects the
    /// plaintext development mode.
    ///
    /// The fallbacks on the two ACME options are unreachable for a config
    /// that came through [`Config::load`], which rejects `acme = true`
    /// without them.
    pub fn web_config(&self) -> WebConfig {
        WebConfig {
            acme: self.web.acme.then(|| AcmeSettings {
                domains: self.web.domains.clone(),
                cache_dir: self.data_dir.join("acme"),
                contact_email: self.web.acme_contact_email.clone().unwrap_or_default(),
                staging: self.web.acme_staging.unwrap_or(true),
            }),
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

    /// A development config: no ACME, so none of the certificate fields.
    const DEV_SAMPLE: &str = r#"
        data_dir  = "/tmp/lblogd-dev"
        posts_dir = "/tmp/lblogd-dev/posts"

        [node]
        instance_name = "lblogd-dev"
        display_name  = "dev blog"

        [web]
        acme      = false
        http_bind = "127.0.0.1:8080"
    "#;

    /// Write `text` to a temp file and run it through the real [`Config::load`],
    /// which is what applies [`Config::validate`].
    fn load_from_str(text: &str) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lblogd.toml");
        std::fs::write(&path, text).unwrap();
        Config::load(&path)
    }

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
        assert_eq!(
            config.web.acme_contact_email.as_deref(),
            Some("ops@example.org")
        );
        assert_eq!(config.web.acme_staging, Some(true));
        assert_eq!(config.web.http_bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.web.https_bind, "127.0.0.1:8443".parse().unwrap());
    }

    #[test]
    fn acme_defaults_to_on_so_deployment_configs_are_unchanged() {
        // SAMPLE carries no `acme` key, exactly like a config written before
        // the field existed.
        assert!(!SAMPLE.contains("acme ="));
        let config = load_from_str(SAMPLE).unwrap();
        assert!(config.web.acme);
        assert!(config.web_config().acme.is_some());
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
        let acme = web.acme.expect("SAMPLE enables acme");
        assert_eq!(
            acme.domains,
            vec!["leviculum.network", "www.leviculum.network"]
        );
        assert_eq!(acme.cache_dir, PathBuf::from("/var/lib/lblogd/acme"));
        assert_eq!(acme.contact_email, "ops@example.org");
        assert!(acme.staging);
        assert_eq!(web.http_bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(web.https_bind, "127.0.0.1:8443".parse().unwrap());
        assert_eq!(web.posts_dir, PathBuf::from("/var/lib/lblogd/posts"));
    }

    #[test]
    fn dev_config_needs_no_acme_fields() {
        let config = load_from_str(DEV_SAMPLE).unwrap();
        assert!(!config.web.acme);
        assert!(config.web.domains.is_empty());
        assert_eq!(config.web.acme_contact_email, None);
        assert_eq!(config.web.acme_staging, None);
        assert_eq!(config.web.http_bind, "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn web_config_without_acme_selects_plaintext_mode() {
        let web = load_from_str(DEV_SAMPLE).unwrap().web_config();
        assert!(web.acme.is_none());
        assert_eq!(web.http_bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(web.posts_dir, PathBuf::from("/tmp/lblogd-dev/posts"));
    }

    #[test]
    fn acme_without_domains_is_invalid() {
        let sample = SAMPLE.replace(
            "domains            = [\"leviculum.network\", \"www.leviculum.network\"]",
            "domains = []",
        );
        let err = load_from_str(&sample).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("web.domains"), "{err}");
    }

    #[test]
    fn acme_without_contact_email_is_invalid() {
        let sample = SAMPLE.replace("acme_contact_email = \"ops@example.org\"", "");
        let err = load_from_str(&sample).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("web.acme_contact_email"), "{err}");
    }

    #[test]
    fn acme_without_staging_choice_is_invalid() {
        // Staging vs production must stay a deliberate decision, so dropping
        // the key is an error rather than silently defaulting.
        let sample = SAMPLE.replace("acme_staging       = true", "");
        let err = load_from_str(&sample).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("web.acme_staging"), "{err}");
    }

    #[test]
    fn acme_off_ignores_missing_certificate_fields() {
        // The same config that is invalid with acme on loads cleanly with it
        // off: the fields are meaningless there, not merely optional.
        let sample = SAMPLE
            .replace("acme_contact_email = \"ops@example.org\"", "")
            .replace("acme_staging       = true", "")
            .replace("[web]", "[web]\n        acme = false");
        let config = load_from_str(&sample).unwrap();
        assert!(config.web_config().acme.is_none());
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
