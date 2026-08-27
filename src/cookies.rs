use std::sync::Arc;

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;

use crate::api::ApiError;

/// A ServiceNow-scoped cookie jar whose outbound header is always sensitive.
pub(crate) struct BrowserCookies {
    jar: Arc<SensitiveCookieJar>,
    api_url: reqwest::Url,
    initial_header: String,
}

impl BrowserCookies {
    pub(crate) fn new(site_url: &str, cookie_header: &str) -> Result<Self, ApiError> {
        HeaderValue::from_str(cookie_header).map_err(|_| invalid_cookie())?;
        let api_url = reqwest::Url::parse(&format!("{site_url}/api/now/")).map_err(|error| {
            ApiError::InvalidInput(format!("invalid ServiceNow API URL: {error}"))
        })?;
        let jar = Arc::new(SensitiveCookieJar::default());
        for pair in cookie_header.split(';').map(str::trim) {
            let (name, value) = pair.split_once('=').ok_or_else(invalid_cookie)?;
            if name.is_empty() {
                return Err(invalid_cookie());
            }
            jar.add_cookie_str(&format!("{name}={value}; Path=/"), &api_url);
        }
        let initial_header = cookie_header_from(&jar, &api_url).ok_or_else(invalid_cookie)?;
        Ok(Self {
            jar,
            api_url,
            initial_header,
        })
    }

    pub(crate) fn provider(&self) -> Arc<impl CookieStore + 'static> {
        self.jar.clone()
    }

    pub(crate) fn current_header(&self) -> Option<String> {
        cookie_header_from(&self.jar, &self.api_url)
    }

    pub(crate) fn refreshed_header(&self) -> Option<String> {
        self.current_header()
            .filter(|header| header != &self.initial_header)
    }
}

#[derive(Debug, Default)]
struct SensitiveCookieJar(Jar);

impl SensitiveCookieJar {
    fn add_cookie_str(&self, cookie: &str, url: &reqwest::Url) {
        self.0.add_cookie_str(cookie, url);
    }
}

impl CookieStore for SensitiveCookieJar {
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &HeaderValue>,
        url: &reqwest::Url,
    ) {
        self.0.set_cookies(cookie_headers, url);
    }

    fn cookies(&self, url: &reqwest::Url) -> Option<HeaderValue> {
        self.0.cookies(url).map(|mut value| {
            value.set_sensitive(true);
            value
        })
    }
}

fn cookie_header_from(jar: &SensitiveCookieJar, url: &reqwest::Url) -> Option<String> {
    jar.cookies(url)
        .and_then(|value| value.to_str().ok().map(str::to_string))
}

fn invalid_cookie() -> ApiError {
    ApiError::InvalidInput("stored browser session cookie is invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_values_that_are_not_cookie_headers() {
        assert!(BrowserCookies::new("https://company.service-now.com", "not-a-cookie").is_err());
        assert!(BrowserCookies::new("https://company.service-now.com", "").is_err());
    }

    #[test]
    fn outbound_cookie_headers_remain_sensitive() {
        let cookies = BrowserCookies::new(
            "https://company.service-now.com",
            "JSESSIONID=synthetic-session",
        )
        .unwrap();
        let header = cookies.jar.cookies(&cookies.api_url).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header, "JSESSIONID=synthetic-session");
    }
}
