//! Decoders for the detail tabs: query strings, forms, cookies and Set-Cookie.
//! They read the projected (redacted unless revealed) heads, never raw values.
use zentop_proxy::{Headers, MASK};

pub(super) type Pairs = Vec<(String, String)>;

/// Percent-decodes `value`; forms also decode `+` as a space.
pub(super) fn percent_decode(value: &str, plus_as_space: bool) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    decoded.push((high * 16 + low) as u8);
                    i += 3;
                    continue;
                }
                decoded.push(b'%');
            }
            b'+' if plus_as_space => decoded.push(b' '),
            byte => decoded.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn pairs(text: &str, plus_as_space: bool) -> Pairs {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_decode(name, plus_as_space),
                percent_decode(value, plus_as_space),
            )
        })
        .collect()
}

/// The decoded query of a URL. Masked values stay masked.
pub(super) fn query(url: &str) -> Pairs {
    let url = url.split_once('#').map_or(url, |(url, _)| url);
    url.split_once('?')
        .map_or_else(Vec::new, |(_, query)| pairs(query, true))
}

/// An `application/x-www-form-urlencoded` body, or `None` if it isn't one.
pub(super) fn form(body: &str) -> Option<Pairs> {
    let body = body.trim();
    if body.is_empty() || body.contains(['\n', '\r']) || !body.contains('=') {
        return None;
    }
    Some(pairs(body, true))
}

/// What a cookie tab shows: parsed values, or a note that they're masked.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Parsed<T> {
    Masked,
    Values(Vec<T>),
}

fn values<'a>(headers: &'a Headers, name: &'a str) -> impl Iterator<Item = &'a str> {
    headers
        .iter()
        .filter(move |(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Request cookies from every `Cookie` header.
pub(super) fn cookies(headers: &Headers) -> Parsed<(String, String)> {
    if values(headers, "cookie").any(|value| value == MASK) {
        return Parsed::Masked;
    }
    Parsed::Values(
        values(headers, "cookie")
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| {
                let pair = pair.trim();
                (!pair.is_empty()).then(|| {
                    let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (name.trim().to_owned(), value.trim().to_owned())
                })
            })
            .collect(),
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SetCookie {
    pub name: String,
    pub value: String,
    /// Attributes such as `Path=/`, `HttpOnly` or `SameSite=Lax`, in order.
    pub attributes: Vec<(String, Option<String>)>,
}

/// Response cookies, one per `Set-Cookie` header.
pub(super) fn set_cookies(headers: &Headers) -> Parsed<SetCookie> {
    if values(headers, "set-cookie").any(|value| value == MASK) {
        return Parsed::Masked;
    }
    Parsed::Values(
        values(headers, "set-cookie")
            .filter_map(|value| {
                let mut parts = value.split(';');
                let (name, value) = parts.next()?.trim().split_once('=')?;
                Some(SetCookie {
                    name: name.trim().to_owned(),
                    value: value.trim().to_owned(),
                    attributes: parts
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(|part| match part.split_once('=') {
                            Some((name, value)) => {
                                (name.trim().to_owned(), Some(value.trim().to_owned()))
                            }
                            None => (part.to_owned(), None),
                        })
                        .collect(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn decodes_queries_and_forms() {
        assert_eq!(
            query("http://x/a?q=hello+world&tag=%E2%9C%93&empty&token=[redacted]#frag"),
            vec![
                ("q".into(), "hello world".into()),
                ("tag".into(), "✓".into()),
                ("empty".into(), String::new()),
                ("token".into(), MASK.into()),
            ]
        );
        assert!(query("http://x/a").is_empty());
        assert_eq!(percent_decode("100%", false), "100%");
        assert_eq!(percent_decode("%zz%4", false), "%zz%4");
        assert_eq!(percent_decode("a+b", false), "a+b");
        assert_eq!(
            form("name=Zen+Top&city=K%C3%B6ln"),
            Some(vec![
                ("name".into(), "Zen Top".into()),
                ("city".into(), "Köln".into())
            ])
        );
        assert_eq!(form("{\"json\": true}"), None);
        assert_eq!(form("a=1\nb=2"), None);
    }

    #[test]
    fn parses_cookies_unless_masked() {
        let raw = headers(&[("cookie", "a=1; b=two=2;flag"), ("Cookie", "c=3")]);
        assert_eq!(
            cookies(&raw),
            Parsed::Values(vec![
                ("a".into(), "1".into()),
                ("b".into(), "two=2".into()),
                ("flag".into(), String::new()),
                ("c".into(), "3".into()),
            ])
        );
        assert_eq!(cookies(&headers(&[("cookie", MASK)])), Parsed::Masked);
        assert_eq!(cookies(&headers(&[])), Parsed::Values(Vec::new()));
    }

    #[test]
    fn parses_set_cookie_attributes() {
        let raw = headers(&[
            (
                "set-cookie",
                "sid=abc; Path=/; Expires=Wed, 21 Oct 2026 07:28:00 GMT; HttpOnly; SameSite=Lax",
            ),
            ("set-cookie", "malformed"),
            ("set-cookie", "theme=dark"),
        ]);
        let Parsed::Values(cookies) = set_cookies(&raw) else {
            panic!("values are not masked");
        };
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "sid");
        assert_eq!(cookies[0].value, "abc");
        assert_eq!(
            cookies[0].attributes,
            vec![
                ("Path".into(), Some("/".into())),
                (
                    "Expires".into(),
                    Some("Wed, 21 Oct 2026 07:28:00 GMT".into())
                ),
                ("HttpOnly".into(), None),
                ("SameSite".into(), Some("Lax".into())),
            ]
        );
        assert!(cookies[1].attributes.is_empty());
        assert_eq!(
            set_cookies(&headers(&[("set-cookie", MASK)])),
            Parsed::Masked
        );
    }
}
