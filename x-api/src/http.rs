use reqwest::blocking::{Client, ClientBuilder};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[doc = "Application-level proxy selection for HTTP requests."]
pub enum ProxyConfig {
    #[default]
    #[doc = "Use the existing environment and system proxy behavior."]
    Inherit,
    #[doc = "Connect directly, ignoring environment and system proxies."]
    Direct,
    #[doc = "Use an explicit proxy URL, ignoring inherited proxy settings."]
    Url(String),
}

impl ProxyConfig {
    #[doc = "Build a client builder using this proxy selection."]
    pub fn builder(&self) -> Result<ClientBuilder, String> {
        let builder = Client::builder();
        match self {
            Self::Inherit => Ok(builder),
            Self::Direct => Ok(builder.no_proxy()),
            Self::Url(value) => {
                let url = reqwest::Url::parse(value).map_err(|_| "Invalid proxy URL")?;
                if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
                    || url.host_str().is_none()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || !matches!(url.path(), "" | "/")
                {
                    return Err(
                        "Proxy URL must use http, https, socks5 or socks5h and contain only a host and optional port or credentials"
                            .to_string(),
                    );
                }
                let proxy = reqwest::Proxy::all(url).map_err(|_| "Invalid proxy URL")?;
                Ok(builder.no_proxy().proxy(proxy))
            }
        }
    }

    #[doc = "Build an HTTP client using this proxy selection."]
    pub fn client(&self) -> Result<Client, String> {
        self.builder()?.build().map_err(|error| error.to_string())
    }
}
