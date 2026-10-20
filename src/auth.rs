use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use md5::{Digest as _, Md5};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Credentials {
    pub username: Zeroizing<String>,
    pub password: Zeroizing<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Challenge {
    Basic,
    Digest {
        realm: String,
        nonce: String,
        opaque: Option<String>,
        qop_auth: bool,
    },
}

fn md5_hex(data: &str) -> String {
    hex::encode(Md5::digest(data.as_bytes()))
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Parses `name=value` / `name="quoted, value"` items, splitting on commas outside quotes.
fn split_items(header: &str) -> Vec<String> {
    let mut items = vec![];
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for c in header.chars() {
        if escaped {
            current.push(c);
            escaped = false;
        } else if in_quotes && c == '\\' {
            escaped = true;
        } else if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
        } else if c == ',' && !in_quotes {
            items.push(core::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    items.push(current);
    items
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

/// Picks the best supported challenge from a (possibly comma-joined, multi-scheme)
/// `WWW-Authenticate` header: Digest with MD5 first, then Basic.
pub fn parse_challenge(header: &str) -> Option<Challenge> {
    let mut schemes: Vec<(String, Vec<(String, String)>)> = vec![];
    for item in split_items(header) {
        // A new scheme starts with a bare token, optionally followed by its first parameter.
        let (scheme, param) = match item.split_once(char::is_whitespace) {
            Some((token, rest)) if !token.contains('=') => (Some(token), rest.trim()),
            _ if !item.contains('=') => (Some(item.as_str()), ""),
            _ => (None, item.as_str()),
        };
        if let Some(scheme) = scheme {
            schemes.push((scheme.to_ascii_lowercase(), vec![]));
        }
        if let (Some((name, value)), Some((_, params))) =
            (param.split_once('='), schemes.last_mut())
        {
            params.push((name.trim().to_ascii_lowercase(), unquote(value.trim())));
        }
    }

    let digest = schemes.iter().find_map(|(scheme, params)| {
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        let algorithm_ok = get("algorithm").is_none_or(|a| a.eq_ignore_ascii_case("MD5"));
        (scheme == "digest" && algorithm_ok).then(|| {
            Some(Challenge::Digest {
                realm: get("realm")?,
                nonce: get("nonce")?,
                opaque: get("opaque"),
                qop_auth: get("qop").is_some_and(|q| q.split(',').any(|o| o.trim() == "auth")),
            })
        })?
    });
    digest.or_else(|| {
        schemes
            .iter()
            .any(|(s, _)| s == "basic")
            .then_some(Challenge::Basic)
    })
}

/// The `Authorization` header value answering `challenge`.
pub fn authorization(
    credentials: &Credentials,
    challenge: &Challenge,
    method: &str,
    uri: &str,
    nonce_count: u32,
    cnonce: &str,
) -> Zeroizing<String> {
    match challenge {
        Challenge::Basic => {
            let pair = Zeroizing::new(format!(
                "{}:{}",
                *credentials.username, *credentials.password
            ));
            Zeroizing::new(format!("Basic {}", BASE64.encode(pair.as_bytes())))
        }
        Challenge::Digest {
            realm,
            nonce,
            opaque,
            qop_auth,
        } => {
            let ha1 = Zeroizing::new(md5_hex(&format!(
                "{}:{realm}:{}",
                *credentials.username, *credentials.password
            )));
            let ha2 = md5_hex(&format!("{method}:{uri}"));
            let nc = format!("{nonce_count:08x}");
            let mut header = format!(
                "Digest username={}, realm={}, nonce={}, uri={}, algorithm=MD5",
                quote(&credentials.username),
                quote(realm),
                quote(nonce),
                quote(uri),
            );
            let response = if *qop_auth {
                header.push_str(&format!(", qop=auth, nc={nc}, cnonce={}", quote(cnonce)));
                md5_hex(&format!("{}:{nonce}:{nc}:{cnonce}:auth:{ha2}", *ha1))
            } else {
                md5_hex(&format!("{}:{nonce}:{ha2}", *ha1))
            };
            header.push_str(&format!(", response={}", quote(&response)));
            if let Some(opaque) = opaque {
                header.push_str(&format!(", opaque={}", quote(opaque)));
            }
            Zeroizing::new(header)
        }
    }
}

/// Path (plus query) of an absolute URL, as used in the Digest `uri` field.
pub fn request_path(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme.find('/').map_or("/", |i| &after_scheme[i..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(username: &str, password: &str) -> Credentials {
        Credentials {
            username: Zeroizing::new(username.into()),
            password: Zeroizing::new(password.into()),
        }
    }

    #[test]
    fn rfc2617_example() {
        let challenge = parse_challenge(
      r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
    )
    .unwrap();
        let header = authorization(
            &creds("Mufasa", "Circle Of Life"),
            &challenge,
            "GET",
            "/dir/index.html",
            1,
            "0a4f113b",
        );
        assert!(
            header.contains(r#"response="6629fae49393a05397450978507c4ef1""#),
            "{}",
            *header
        );
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
        assert!(header.contains("nc=00000001"));
    }

    #[test]
    fn monerod_joined_headers_prefer_md5() {
        let header = r#"Digest qop="auth",algorithm=MD5-sess,realm="monero-rpc",nonce="abc",stale=false, Digest qop="auth",algorithm=MD5,realm="monero-rpc",nonce="xyz",stale=false"#;
        assert_eq!(
            parse_challenge(header),
            Some(Challenge::Digest {
                realm: "monero-rpc".into(),
                nonce: "xyz".into(),
                opaque: None,
                qop_auth: true
            })
        );
    }

    #[test]
    fn basic_fallback() {
        assert_eq!(
            parse_challenge(r#"Basic realm="proxy""#),
            Some(Challenge::Basic)
        );
        let header = authorization(
            &creds("user", "pass"),
            &Challenge::Basic,
            "POST",
            "/",
            1,
            "",
        );
        assert_eq!(*header, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn request_paths() {
        assert_eq!(request_path("http://127.0.0.1:18081/json_rpc"), "/json_rpc");
        assert_eq!(
            request_path("https://api.example.com/monero/get_blocks.bin"),
            "/monero/get_blocks.bin"
        );
        assert_eq!(request_path("http://host"), "/");
    }
}
