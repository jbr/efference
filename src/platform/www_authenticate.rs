//! `WWW-Authenticate` challenge parsing (RFC 9110 §11.6.1).
//!
//! Deliberately free of both OAuth and MCP. A `401` from a connected provider
//! carries the challenge that starts the authorization dance (docs/tools.md),
//! but nothing in this module knows that — it turns a header value into
//! scheme/parameter structure and stops. The intended home is a standalone
//! crate alongside `trillium-basic-auth`, which hand-rolls the emit half of
//! this same grammar; it lives here until a second consumer shows what the API
//! should be.
//!
//! ## Why this is not `split(',')`
//!
//! The header is a comma-separated list of challenges, and each challenge's
//! parameters are *also* comma-separated, with no delimiter distinguishing the
//! two levels:
//!
//! ```text
//! WWW-Authenticate: Bearer realm="a, b", scope="x", Basic realm="c"
//! ```
//!
//! That is two challenges, the first with two parameters — and the first
//! `realm` value contains a comma of its own. Three rules resolve it:
//!
//! - Commas inside a quoted-string are data, not separators, and a
//!   quoted-string may contain `\"` escapes.
//! - After a comma, a token followed by `=` continues the current challenge;
//!   a bare token starts a new one.
//! - Immediately after a scheme, a bare token is a `token68` blob (the
//!   `Negotiate a87421bf` shape), not a new scheme — the same syntax means
//!   different things at the two positions.
//!
//! Parsing is lenient and total: there is no error type, because a client that
//! refuses to read a slightly-malformed challenge simply fails to
//! authenticate. Junk is dropped and whatever parsed is returned.

/// One challenge: an auth-scheme plus either a `token68` blob or a list of
/// auth-params. The grammar permits exactly one of the two, and a scheme with
/// neither is legal (`WWW-Authenticate: Basic`).
///
/// Parameters are held as an ordered `Vec` rather than a map: RFC 9110 forbids
/// a repeated parameter name within one challenge, but this is a parser, and
/// what a server sent is more useful than what it should have sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Challenge {
    scheme: String,
    token68: Option<String>,
    params: Vec<(String, String)>,
}

impl Challenge {
    fn new(scheme: &str) -> Self {
        Self {
            scheme: scheme.to_string(),
            ..Self::default()
        }
    }

    /// The auth-scheme as it was written. Compare with [`Challenge::is_scheme`]
    /// rather than `==`; scheme names are case-insensitive.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Whether this is the named scheme, compared case-insensitively per
    /// RFC 9110 §11.1.
    pub fn is_scheme(&self, name: &str) -> bool {
        self.scheme.eq_ignore_ascii_case(name)
    }

    /// The `token68` blob, for schemes that carry one instead of parameters.
    pub fn token68(&self) -> Option<&str> {
        self.token68.as_deref()
    }

    /// The first parameter with this name, matched case-insensitively per
    /// RFC 9110 §11.6.1. The *value* is returned as written, with
    /// quoted-string escapes already resolved — values are case-sensitive.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every parameter, in the order the server sent them.
    pub fn params(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// The challenges from one or more `WWW-Authenticate` header values, in the
/// order they were offered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Challenges(Vec<Challenge>);

impl Challenges {
    /// Parse one header value.
    pub fn parse(input: &str) -> Self {
        let mut challenges: Vec<Challenge> = Vec::new();
        Self::extend_from(&mut challenges, input);
        Self(challenges)
    }

    /// Parse several header values as one list — a header that appears more
    /// than once is equivalent to one header with the values comma-joined
    /// (RFC 9110 §5.3), which is how `Headers::get_values` hands them over.
    pub fn parse_all<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut challenges: Vec<Challenge> = Vec::new();
        for value in values {
            Self::extend_from(&mut challenges, value);
        }
        Self(challenges)
    }

    /// The first challenge offering this scheme, matched case-insensitively.
    pub fn scheme(&self, name: &str) -> Option<&Challenge> {
        self.0.iter().find(|c| c.is_scheme(name))
    }

    /// Every challenge, in offer order.
    pub fn iter(&self) -> impl Iterator<Item = &Challenge> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Append the challenges in `input` to `challenges`. Continuation
    /// parameters attach to whatever challenge is currently last, which is why
    /// this takes the accumulator rather than returning a fresh list — a
    /// challenge split across two header lines still assembles correctly.
    fn extend_from(challenges: &mut Vec<Challenge>, input: &str) {
        for element in split_elements(input) {
            // `\r\n` as well as spaces: the deprecated obs-fold continuation
            // can leave a line break inside a value handed to us as one string.
            let element = element.trim_matches([' ', '\t', '\r', '\n']);
            // The `#rule` list production permits empty elements; skip them.
            if element.is_empty() {
                continue;
            }
            match classify(element) {
                Some(Element::Scheme(scheme)) => challenges.push(Challenge::new(scheme)),
                Some(Element::SchemeToken68(scheme, token68)) => {
                    let mut challenge = Challenge::new(scheme);
                    challenge.token68 = Some(token68.to_string());
                    challenges.push(challenge);
                }
                Some(Element::SchemeParam(scheme, param)) => {
                    let mut challenge = Challenge::new(scheme);
                    challenge.params.push(param);
                    challenges.push(challenge);
                }
                // A parameter continues the challenge already in progress. One
                // with nothing to attach to is junk from a malformed header.
                Some(Element::Param(param)) => {
                    if let Some(last) = challenges.last_mut() {
                        last.params.push(param);
                    }
                }
                None => {}
            }
        }
    }
}

impl<'a> IntoIterator for &'a Challenges {
    type Item = &'a Challenge;
    type IntoIter = std::slice::Iter<'a, Challenge>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// What one comma-separated element turned out to be. The first three start a
/// new challenge; [`Element::Param`] continues the previous one.
enum Element<'a> {
    /// `Basic` — a scheme with nothing after it in this element.
    Scheme(&'a str),
    /// `Bearer realm="x"` — a scheme and its first parameter.
    SchemeParam(&'a str, (String, String)),
    /// `Negotiate a87421bf` — a scheme and its `token68` blob.
    SchemeToken68(&'a str, &'a str),
    /// `scope="x"` — a parameter belonging to the preceding challenge.
    Param((String, String)),
}

/// Split on the commas that separate list elements, leaving those inside a
/// quoted-string alone. Slicing is byte-indexed but only ever at `,` `"` and
/// `\`, which are ASCII, so every boundary is a char boundary.
fn split_elements(input: &str) -> Vec<&str> {
    let mut elements = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, byte) in input.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                elements.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    elements.push(&input[start..]);
    elements
}

/// Decide what a single trimmed, non-empty element is.
///
/// The leading token is a parameter name if `=` follows it and a scheme
/// otherwise. This is unambiguous *at the start of an element* because a
/// `token68` never appears there: it has no commas of its own, so it always
/// shares an element with the scheme it follows.
fn classify(element: &str) -> Option<Element<'_>> {
    let (word, rest) = take_token(element);
    if word.is_empty() {
        return None;
    }
    let rest = trim_bws(rest);
    if let Some(after_eq) = rest.strip_prefix('=') {
        return Some(Element::Param((
            word.to_string(),
            parse_value(trim_bws(after_eq)),
        )));
    }
    if rest.is_empty() {
        return Some(Element::Scheme(word));
    }
    // `word` is a scheme and `rest` is its single following item: either the
    // first auth-param, or a token68.
    Some(match classify_after_scheme(rest) {
        AfterScheme::Param(param) => Element::SchemeParam(word, param),
        AfterScheme::Token68(token68) => Element::SchemeToken68(word, token68),
    })
}

enum AfterScheme<'a> {
    Param((String, String)),
    Token68(&'a str),
}

/// Disambiguate the item following a scheme.
///
/// Both productions can start with the same characters, and both can contain
/// `=`: `realm="x"` is a parameter, `dGVzdA==` is a token68 whose trailing `=`
/// is base64 padding. The distinguishing question is what follows the `=`
/// signs — padding is the end of the item, a parameter value is not.
fn classify_after_scheme(rest: &str) -> AfterScheme<'_> {
    let (name, after_name) = take_token(rest);
    let after_name = trim_bws(after_name);
    let equals = after_name.len() - after_name.trim_start_matches('=').len();
    if equals == 1 {
        let value = trim_bws(&after_name[1..]);
        // A lone `=` at the end of the item is padding, not an empty value.
        if !value.is_empty() {
            return AfterScheme::Param((name.to_string(), parse_value(value)));
        }
    }
    // Everything else — bare `Negotiate abc`, base64 padding, characters that
    // are not `tchar` at all (`/` and `+` appear in token68 but `/` is not a
    // token character) — is the blob, taken whole.
    AfterScheme::Token68(rest.trim_end_matches([' ', '\t']))
}

/// A parameter value: a quoted-string with its escapes resolved, or a bare
/// token.
fn parse_value(input: &str) -> String {
    match input.strip_prefix('"') {
        Some(quoted) => {
            let mut value = String::new();
            let mut escaped = false;
            for c in quoted.chars() {
                match c {
                    _ if escaped => {
                        value.push(c);
                        escaped = false;
                    }
                    '\\' => escaped = true,
                    '"' => break,
                    _ => value.push(c),
                }
            }
            // An unterminated quoted-string yields what was there; the
            // alternative is discarding a value the server plainly meant.
            value
        }
        None => take_token(input).0.to_string(),
    }
}

/// Split off the leading run of `tchar` (RFC 9110 §5.6.2).
fn take_token(input: &str) -> (&str, &str) {
    let end = input.find(|c: char| !is_tchar(c)).unwrap_or(input.len());
    input.split_at(end)
}

fn is_tchar(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

/// Drop leading "bad whitespace" — the optional space the grammar tolerates
/// around the `=` in an auth-param.
fn trim_bws(input: &str) -> &str {
    input.trim_start_matches([' ', '\t'])
}

#[cfg(test)]
mod test;
