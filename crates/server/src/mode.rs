//! Single-user (the default) or multi-user server mode (`user-accounts` spec).
//!
//! Server mode needs the public address people open (`--public-url`): sign-in and invitation
//! links use it, write requests must come from it (`Origin`), and session cookies are `Secure`
//! when it is `https://`. It is configured rather than read from request headers because
//! `X-Forwarded-*` headers can be sent by any client (see `auth.rs`).

use url::Url;

/// `--mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ModeArg {
    /// One user on this computer, signed in with the access token.
    Single,
    /// Accounts and passwords for several people, behind `--public-url`.
    Server,
}

/// How this Open Prism runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Single,
    Server(ServerSettings),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    pub public_url: PublicUrl,
    /// `--insecure-http`: a plain `http://` public URL was allowed, for a test.
    pub insecure_http: bool,
}

impl Mode {
    /// Checks the mode flags together; every combination that cannot work is an error.
    pub fn resolve(
        mode: ModeArg,
        public_url: Option<&str>,
        insecure_http: bool,
    ) -> Result<Self, String> {
        let public_url = public_url.map(str::trim).filter(|url| !url.is_empty());
        match mode {
            ModeArg::Single => {
                if public_url.is_some() {
                    return Err("--public-url only applies with --mode server".into());
                }
                if insecure_http {
                    return Err("--insecure-http only applies with --mode server".into());
                }
                Ok(Self::Single)
            }
            ModeArg::Server => {
                let url = public_url.ok_or(
                    "server mode needs --public-url (OPENPRISM_PUBLIC_URL): the https:// address \
                     people open, such as https://prism.example.org",
                )?;
                let public_url = PublicUrl::parse(url)?;
                if !public_url.is_https() && !insecure_http {
                    return Err(format!(
                        "the public URL {public_url} uses http://, but server mode needs HTTPS: run \
                         Open Prism behind a reverse proxy such as Caddy and use its https:// \
                         address. For a test on a network you trust, add --insecure-http"
                    ));
                }
                Ok(Self::Server(ServerSettings {
                    public_url,
                    insecure_http,
                }))
            }
        }
    }

    pub fn is_server(&self) -> bool {
        matches!(self, Self::Server(_))
    }

    pub fn server(&self) -> Option<&ServerSettings> {
        match self {
            Self::Server(settings) => Some(settings),
            Self::Single => None,
        }
    }
}

/// The address people open in server mode: a scheme, a host and maybe a port, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrl(Url);

impl PublicUrl {
    pub fn parse(value: &str) -> Result<Self, String> {
        let url =
            Url::parse(value).map_err(|err| format!("invalid --public-url {value}: {err}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "--public-url must start with https:// (got {value})"
            ));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(format!("--public-url needs a host name (got {value})"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("--public-url must not contain a user name or password".into());
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(format!(
                "--public-url must be the address of the site itself, without a path, such as \
                 https://prism.example.org (got {value})"
            ));
        }
        Ok(Self(url))
    }

    pub fn is_https(&self) -> bool {
        self.0.scheme() == "https"
    }

    /// `scheme://host[:port]`, as browsers send it in `Origin`.
    pub fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    /// An absolute link to `path` (which starts with `/`) on this site.
    pub fn link(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }
}

impl std::fmt::Display for PublicUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.origin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_is_the_default_and_takes_no_server_flags() {
        assert_eq!(
            Mode::resolve(ModeArg::Single, None, false),
            Ok(Mode::Single)
        );
        assert_eq!(
            Mode::resolve(ModeArg::Single, Some(" "), false),
            Ok(Mode::Single)
        );
        let err = Mode::resolve(ModeArg::Single, Some("https://p.example"), false).unwrap_err();
        assert!(err.contains("--public-url only applies"), "{err}");
        let err = Mode::resolve(ModeArg::Single, None, true).unwrap_err();
        assert!(err.contains("--insecure-http only applies"), "{err}");
    }

    #[test]
    fn server_needs_a_public_url() {
        let err = Mode::resolve(ModeArg::Server, None, false).unwrap_err();
        assert!(err.contains("needs --public-url"), "{err}");
    }

    #[test]
    fn server_needs_https_unless_insecure_http() {
        let err =
            Mode::resolve(ModeArg::Server, Some("http://192.168.1.5:3737"), false).unwrap_err();
        assert!(
            err.contains("needs HTTPS") && err.contains("--insecure-http"),
            "{err}"
        );

        let mode = Mode::resolve(ModeArg::Server, Some("http://192.168.1.5:3737"), true).unwrap();
        let settings = mode.server().unwrap();
        assert!(settings.insecure_http);
        assert!(!settings.public_url.is_https());

        let mode =
            Mode::resolve(ModeArg::Server, Some("https://Prism.Example.org/"), false).unwrap();
        let url = &mode.server().unwrap().public_url;
        assert!(url.is_https());
        assert_eq!(url.origin(), "https://prism.example.org");
        assert_eq!(
            url.link("/setup/abc"),
            "https://prism.example.org/setup/abc"
        );
    }

    #[test]
    fn the_public_url_is_only_a_site_address() {
        assert_eq!(
            PublicUrl::parse("http://127.0.0.1:3798").unwrap().origin(),
            "http://127.0.0.1:3798"
        );
        assert_eq!(
            PublicUrl::parse("https://p.example:443").unwrap().origin(),
            "https://p.example"
        );
        for bad in [
            "prism.example.org",
            "ftp://p.example",
            "https://p.example/prism",
            "https://p.example/?a=1",
            "https://p.example/#x",
            "https://user:pw@p.example",
        ] {
            assert!(PublicUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
