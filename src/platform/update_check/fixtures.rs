//! Signed `latest.json` fixtures and an in-process server for them, shared by the
//! update check's tests and the host's (docs/update-check.md).

use super::*;

// Fixtures signed with test keys made for these tests (`minisign -G -W`), never the
// release key. Each signature is `minisign -S -m <document> -t "<trusted comment>"`
// over the document beside it, byte for byte: a document has no trailing newline.

/// The public half of the test key every valid fixture is signed with.
pub(crate) const TEST_KEY: &str = "RWQLGCCZk4R9Z/JWr5nK9pRYVJeTsk3zhwIqLPue5t8V63Gn6vLcJKMQ";

pub(crate) const NEWER: &str =
    r#"{"version":"9.0.0","tag":"v9.0.0","url":"https://efference.net/releases/v9.0.0"}"#;

/// Over [`NEWER`], for `efference v9.0.0 latest.json`.
pub(crate) const NEWER_SIG: &str = "untrusted comment: signature from minisign secret key
RUQLGCCZk4R9Z1S+csXsefAFzbECu4zR58mmaUhfEA8/tG8JcbBLgY43fe4E4q5xoKSCaSd6ZO1tTtT49P9Ps0xVw0CUGUFi1gE=
trusted comment: efference v9.0.0 latest.json
KUf7eTChWykGaYU+CUPHC+HduOETeXSdWhqmwuqWjZrU/KCekwSFFPmX/zEVSfin5ham5X6TVkc5TLwRZCJVDg==
";

/// Over [`NEWER`], for another release's tag.
pub(crate) const NEWER_SIG_OLD_TAG: &str = "untrusted comment: signature from minisign secret key
RUQLGCCZk4R9Z1S+csXsefAFzbECu4zR58mmaUhfEA8/tG8JcbBLgY43fe4E4q5xoKSCaSd6ZO1tTtT49P9Ps0xVw0CUGUFi1gE=
trusted comment: efference v0.0.1 latest.json
ZVuNLtTKk0HXqf2ZLZRjYRCcLS7WYdTPXhDkhxdFQI1QIg2UNHXzJAsXvLz8xHDDViI5aKHrZ1DOBkFZ8Jv0Cg==
";

/// Over [`NEWER`], for another file of the same release.
pub(crate) const NEWER_SIG_OTHER_FILE: &str =
    "untrusted comment: signature from minisign secret key
RUQLGCCZk4R9Z1S+csXsefAFzbECu4zR58mmaUhfEA8/tG8JcbBLgY43fe4E4q5xoKSCaSd6ZO1tTtT49P9Ps0xVw0CUGUFi1gE=
trusted comment: efference v9.0.0 install.sh
ygfukojaKd3rjswfO/+D81Sv5QijdKa0ihQDKOWrZv1Zz1IvAdCUMas4f+kdU8yEfchg3W1ytRlwdb8xrL9XCQ==
";

/// Over [`NEWER`], with the right comment, by a key that is not [`TEST_KEY`].
pub(crate) const NEWER_SIG_OTHER_KEY: &str = "untrusted comment: signature from minisign secret key
RUTr136xowSZAxNhb7dx/EkQqJOV0BjJexf/Vbi9lhfp3gC5tBI6G67BpSI13mK4desL0XLrvAlmonfLQnDQzgbJwEXE3Ye+aQU=
trusted comment: efference v9.0.0 latest.json
lNo6eJFNbvtCoUiatPF9+ysZf/5Eh0oZgd/mSVX5UbrWCyFKBR3ZrjbE27gGdxddBsJjH8BviY51C6YlcbbSCw==
";

pub(crate) const OLDER: &str =
    r#"{"version":"0.0.1","tag":"v0.0.1","url":"https://efference.net/releases/v0.0.1"}"#;

/// Over [`OLDER`], for `efference v0.0.1 latest.json`.
pub(crate) const OLDER_SIG: &str = "untrusted comment: signature from minisign secret key
RUQLGCCZk4R9Z6dxJgN+89panvYA7fRidssW1382orjuKQuKOxd3RmhXTyk77Pn25pyN3tO8lhEdEeg8YI23Vb8d5lE2SejGxQ0=
trusted comment: efference v0.0.1 latest.json
ri/PFTPU8HFcHL9O6kCvaEdZR7SqVYcr7jMSbZGYw5c0/muCHKrJQ3NLef3SHec5oFcUi2oPbeBE4mIZhW23Bg==
";

/// A server answering `path` with `body`, a `404` for anything else; the check
/// reaches it in-process.
pub(crate) fn serving(files: &'static [(&'static str, &'static str)]) -> Client {
    let server = move |conn: trillium::Conn| async move {
        match files.iter().find(|(path, _)| *path == conn.path()) {
            Some((_, body)) => conn.ok(*body),
            None => conn.with_status(Status::NotFound).halt(),
        }
    };
    Client::new(trillium_testing::ServerConnector::new(server))
}

pub(crate) const URL: &str = "http://releases.test/releases/latest.json";

pub(crate) fn check(files: &'static [(&'static str, &'static str)]) -> UpdateCheck {
    UpdateCheck::at(serving(files), URL, TEST_KEY).unwrap()
}
