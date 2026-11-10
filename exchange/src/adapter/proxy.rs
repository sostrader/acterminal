use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ProxyScheme {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyScheme::Http => "http",
            ProxyScheme::Https => "https",
            ProxyScheme::Socks5 => "socks5",
            ProxyScheme::Socks5h => "socks5h",
        }
    }

    pub const ALL: [ProxyScheme; 4] = [
        ProxyScheme::Http,
        ProxyScheme::Https,
        ProxyScheme::Socks5,
        ProxyScheme::Socks5h,
    ];
}

impl std::fmt::Display for ProxyScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProxyAuth {
    username: Username,
    password: Password,
}

impl ProxyAuth {
    pub fn try_new(
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            username: Username::parse(username)?,
            password: Password::parse(password)?,
        })
    }

    pub fn username(&self) -> &str {
        self.username.as_str()
    }

    pub fn password(&self) -> &str {
        self.password.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Proxy {
    scheme: ProxyScheme,
    host: String,
    port: u16,
    auth: Option<ProxyAuth>,
}

impl Proxy {
    pub fn new(
        scheme: ProxyScheme,
        host: impl Into<String>,
        port: u16,
        auth: Option<ProxyAuth>,
    ) -> Result<Self, String> {
        let host_raw = host.into();
        let host = host_raw.trim();

        if host.is_empty() {
            return Err("Proxy host missing".to_string());
        }
        if port == 0 {
            return Err("Proxy port must be in range 1-65535".to_string());
        }

        let proxy = Self {
            scheme,
            host: host.to_string(),
            port,
            auth,
        };

        proxy.try_to_url_string()?;
        Ok(proxy)
    }

    pub fn scheme(&self) -> ProxyScheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn auth(&self) -> Option<&ProxyAuth> {
        self.auth.as_ref()
    }

    pub fn set_auth(&mut self, auth: Option<ProxyAuth>) {
        self.auth = auth;
    }

    pub fn without_auth(mut self) -> Self {
        self.auth = None;
        self
    }

    pub fn try_from_str_strict(s: &str) -> Result<Self, String> {
        let s = s.trim();

        if s.is_empty() {
            return Err("Proxy URL is empty".to_string());
        }
        if !s.contains("://") {
            return Err(format!(
                "Invalid proxy value (missing scheme): {s:?}. Expected e.g. http://127.0.0.1:8080 or socks5h://127.0.0.1:1080."
            ));
        }

        let url = url::Url::parse(s).map_err(|e| format!("Invalid proxy URL: {e}"))?;
        Self::try_from_url(&url)
    }

    pub fn try_from_url(url: &url::Url) -> Result<Self, String> {
        let scheme_str = url.scheme().to_ascii_lowercase();
        let scheme = match scheme_str.as_str() {
            "http" => ProxyScheme::Http,
            "https" => ProxyScheme::Https,
            "socks5" => ProxyScheme::Socks5,
            "socks5h" => ProxyScheme::Socks5h,
            _ => {
                return Err(format!(
                    "Unsupported proxy scheme: {scheme_str} (use http://, https://, socks5://, socks5h://)"
                ));
            }
        };

        let host = url
            .host_str()
            .ok_or_else(|| "Proxy host missing".to_string())?
            .to_string();

        let port = url
            .port_or_known_default()
            .ok_or_else(|| "Proxy port missing".to_string())?;

        let username = (!url.username().is_empty()).then(|| url.username().to_string());
        let password = url.password().map(|s| s.to_string());

        let auth = match (username, password) {
            (None, None) => None,
            (Some(username), Some(password)) => Some(ProxyAuth::try_new(username, password)?),
            _ => return Err("Proxy auth requires both username and password".to_string()),
        };

        Self::new(scheme, host, port, auth)
    }

    fn host_with_ipv6_brackets(host: &str) -> String {
        // If `host` looks like an IPv6 literal and is not already bracketed,
        // add brackets to produce a valid URL authority / socket address.
        let h = host.trim();
        if h.contains(':') && !h.starts_with('[') && !h.ends_with(']') {
            format!("[{h}]")
        } else {
            h.to_string()
        }
    }

    fn host_for_url_authority(&self) -> String {
        // url::Url::host_str() returns IPv6 without brackets (e.g. "2001:db8::1"),
        // but URL authority form requires brackets: "http://[2001:db8::1]:8080".
        Self::host_with_ipv6_brackets(&self.host)
    }

    pub fn try_to_url_string(&self) -> Result<String, String> {
        let host = self.host_for_url_authority();

        let mut url = url::Url::parse(&format!(
            "{}://{}:{}/",
            self.scheme.as_str(),
            host,
            self.port
        ))
        .map_err(|e| format!("Invalid proxy components: {e}"))?;

        if let Some(auth) = &self.auth {
            url.set_username(auth.username())
                .map_err(|_| "Invalid proxy username".to_string())?;
            url.set_password(Some(auth.password()))
                .map_err(|_| "Invalid proxy password".to_string())?;
        }

        let mut out = url.to_string();
        if out.ends_with('/') {
            out.pop();
        }
        Ok(out)
    }

    pub fn to_url_string(&self) -> String {
        match self.try_to_url_string() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Proxy::to_url_string fallback: {}", e);
                self.to_url_string_no_auth()
            }
        }
    }

    pub fn to_url_string_no_auth(&self) -> String {
        let host = self.host_for_url_authority();
        format!("{}://{}:{}", self.scheme.as_str(), host, self.port)
    }

    /// Safe for logs/telemetry: never includes username or password.
    pub fn to_log_string(&self) -> String {
        let host = self.host_for_url_authority();
        if self.auth.is_some() {
            format!("{}://***:***@{}:{}", self.scheme.as_str(), host, self.port)
        } else {
            self.to_url_string_no_auth()
        }
    }

    /// Safe for UI display: may include username, never includes password.
    pub fn to_ui_string(&self) -> String {
        let host = self.host_for_url_authority();
        match self.auth.as_ref() {
            Some(auth) => format!(
                "{}://{}@{}:{}",
                self.scheme.as_str(),
                auth.username(),
                host,
                self.port
            ),
            None => self.to_url_string_no_auth(),
        }
    }
}

impl std::fmt::Display for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_log_string())
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
struct Username(String);

impl Username {
    fn parse(value: impl Into<String>) -> Result<Self, String> {
        value.into().try_into()
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("Proxy username cannot be empty".to_string());
        }
        if value.contains(':') {
            return Err("Proxy username cannot contain ':'".to_string());
        }
        if value.as_bytes().iter().any(|b| *b == b'\r' || *b == b'\n') {
            return Err("Proxy username cannot contain CR or LF characters".to_string());
        }
        Ok(())
    }
}

impl std::fmt::Debug for Username {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Username").field(&self.0).finish()
    }
}

impl TryFrom<String> for Username {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.0
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
struct Password(String);

impl Password {
    fn parse(value: impl Into<String>) -> Result<Self, String> {
        value.into().try_into()
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("Proxy password cannot be empty".to_string());
        }
        if value.as_bytes().iter().any(|b| *b == b'\r' || *b == b'\n') {
            return Err("Proxy password cannot contain CR or LF characters".to_string());
        }
        Ok(())
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(**redacted**)")
    }
}

impl TryFrom<String> for Password {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<Password> for String {
    fn from(value: Password) -> Self {
        value.0
    }
}
