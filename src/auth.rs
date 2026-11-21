//! Cookie handling shared by the native interfaces. Never print a CookieJar.
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Default)]
pub struct CookieJar(BTreeMap<String, String>);

impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieJar")
            .field("names", &self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CookieJar {
    pub fn parse(header: &str) -> Result<Self, String> {
        if header.len() > 64 * 1024 || header.chars().any(|c| c.is_control()) {
            return Err("Cookie contains control characters or exceeds 64 KiB".into());
        }
        let mut jar = Self::default();
        for item in header.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (name, value) = item.split_once('=').ok_or("Invalid Cookie header")?;
            let name = name.trim();
            if jar.0.contains_key(name) {
                return Err(format!("Duplicate Cookie name: {name}"));
            }
            jar.insert(name, value.trim())?;
        }
        Ok(jar)
    }

    pub fn insert(&mut self, name: &str, value: &str) -> Result<(), String> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || value.bytes().any(|c| c < 0x20 || c == 0x7f || c == b';')
        {
            return Err("Invalid cookie name or value".into());
        }
        self.0.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    /// Ingest the first name/value pair only; attributes are not cookies.
    pub fn add_set_cookie(&mut self, header: &str) -> Result<(), String> {
        if header.chars().any(|c| c.is_control()) {
            return Err("Invalid Set-Cookie header".into());
        }
        let pair = header.split(';').next().unwrap_or_default();
        let (name, value) = pair.split_once('=').ok_or("Invalid Set-Cookie header")?;
        self.insert(name.trim(), value.trim())
    }

    pub fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn csrf(&self) -> Result<&str, String> {
        self.get("bili_jct")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Login required: missing bili_jct".into())
    }

    pub fn uid(&self) -> Result<&str, String> {
        self.get("DedeUserID")
            .filter(|s| {
                !s.is_empty()
                    && s.bytes().all(|b| b.is_ascii_digit())
                    && s.parse::<u64>().is_ok_and(|uid| uid > 0)
            })
            .ok_or_else(|| "Login required: missing or invalid DedeUserID".into())
    }

    pub fn validate(&self) -> Result<(), String> {
        self.csrf()?;
        self.uid()?;
        self.get("SESSDATA")
            .filter(|s| !s.is_empty())
            .ok_or("Login required: missing SESSDATA")?;
        Ok(())
    }

    pub fn redacted(&self) -> serde_json::Value {
        serde_json::json!({"uid": self.uid().ok(), "cookie_names": self.0.keys().collect::<Vec<_>>(), "credentials_present": self.validate().is_ok()})
    }

    pub(crate) fn redact_text(&self, text: &str) -> String {
        let mut output = text.to_owned();
        for (key, value) in &self.0 {
            if key != "DedeUserID" && value.len() >= 4 {
                output = output.replace(value, "<redacted>");
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_header_injection_and_ambiguous_identity() {
        assert!(CookieJar::parse("SESSDATA=a\r\nAuthorization: b").is_err());
        assert!(CookieJar::parse("DedeUserID=1; DedeUserID=2").is_err());
        assert!(
            CookieJar::parse("DedeUserID=../2; bili_jct=a; SESSDATA=b")
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn cookie_attributes_are_not_persisted_and_debug_is_redacted() {
        let mut jar = CookieJar::default();
        jar.add_set_cookie("SESSDATA=private-value; Secure; Path=/; HttpOnly")
            .unwrap();
        assert_eq!(jar.header(), "SESSDATA=private-value");
        assert!(!format!("{jar:?}").contains("private-value"));
        assert_eq!(jar.redact_text("failed private-value"), "failed <redacted>");
    }
}
