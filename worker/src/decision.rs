//! Moderation results → on-chain action (design §7.5). Pure + exhaustively tested.
//! Fail closed: a check that could not run (`Unavailable`) never leads to an approval, and
//! never to a reject either — unless another check already decides the reject on its own.

use core::num::NonZeroU32;

use crate::{submission::Kind, tx::Action};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Check {
    Pass,
    /// reason bits (`tx::flags`) — never zero, so every on-chain reject carries a reason
    Fail {
        flags: NonZeroU32,
        detail: Vec<String>,
    },
    /// the check could not run (API down, timeout, missing key, billing …)
    Unavailable(String),
}

impl Check {
    pub fn fail(flag: u32, detail: impl Into<String>) -> Check {
        Check::Fail {
            flags: NonZeroU32::new(flag).expect("reason flag must be non-zero"),
            detail: vec![detail.into()],
        }
    }
    fn fail_flags(&self) -> u32 {
        match self {
            Check::Fail { flags, .. } => flags.get(),
            _ => 0,
        }
    }
}

/// Approved content for the tx (already validated + normalized).
#[derive(Debug, Clone)]
pub struct Parts {
    pub image_url: String,
    pub image_sha256: [u8; 32],
    pub text: String,
    pub click_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Outcome {
    Act(Action),
    /// nothing is sent on chain; the client may re-sign and retry
    Retry {
        unavailable: Vec<String>,
    },
}

fn unavailable(checks: &[&Check]) -> Vec<String> {
    checks
        .iter()
        .filter_map(|c| match c {
            Check::Unavailable(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// `text` / `url` are only consulted for `Kind::Full` (an image-only resubmission keeps the
/// text and click_url approved earlier — they are locked on chain).
pub fn decide(kind: Kind, image: &Check, text: &Check, url: &Check, p: &Parts) -> Outcome {
    match kind {
        Kind::ImageOnly => match image {
            Check::Pass => Outcome::Act(Action::ApproveImage {
                image_url: p.image_url.clone(),
                image_sha256: p.image_sha256,
            }),
            Check::Fail { flags, .. } => Outcome::Act(Action::Reject { flags: flags.get() }),
            Check::Unavailable(_) => Outcome::Retry {
                unavailable: unavailable(&[image]),
            },
        },
        Kind::Full => {
            // text or click_url failing decides on its own (both are mandatory — §11 Q3)
            let hard = text.fail_flags() | url.fail_flags();
            if hard != 0 {
                return Outcome::Act(Action::Reject {
                    flags: hard | image.fail_flags(),
                });
            }
            if matches!(text, Check::Unavailable(_)) || matches!(url, Check::Unavailable(_)) {
                return Outcome::Retry {
                    unavailable: unavailable(&[image, text, url]),
                };
            }
            match image {
                Check::Pass => Outcome::Act(Action::ApproveFull {
                    image_url: p.image_url.clone(),
                    image_sha256: p.image_sha256,
                    text: p.text.clone(),
                    click_url: p.click_url.clone(),
                }),
                // A13: show the text until a new image passes (one tx, reasons recorded)
                Check::Fail { flags, .. } => Outcome::Act(Action::ApproveTextOnly {
                    text: p.text.clone(),
                    click_url: p.click_url.clone(),
                    image_reject_flags: flags.get(),
                }),
                Check::Unavailable(_) => Outcome::Retry {
                    unavailable: unavailable(&[image]),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::flags::*;

    fn parts() -> Parts {
        Parts {
            image_url: "https://w/ads/a.png".into(),
            image_sha256: [1; 32],
            text: "t".into(),
            click_url: "https://x.example.org/".into(),
        }
    }
    fn all() -> Vec<Check> {
        vec![
            Check::Pass,
            Check::fail(IMAGE_MODERATION, "i"),
            Check::Unavailable("down".into()),
        ]
    }

    #[test]
    fn full_table() {
        let p = parts();
        let pass = Check::Pass;
        let img_bad = Check::fail(IMAGE_MODERATION, "sexual");
        let txt_bad = Check::fail(TEXT_MODERATION, "hate");
        let url_bad = Check::fail(URL_LAYER2, "MALWARE");
        let down = Check::Unavailable("openai http_429".into());

        assert!(matches!(
            decide(Kind::Full, &pass, &pass, &pass, &p),
            Outcome::Act(Action::ApproveFull { .. })
        ));
        assert_eq!(
            decide(Kind::Full, &img_bad, &pass, &pass, &p),
            Outcome::Act(Action::ApproveTextOnly {
                text: "t".into(),
                click_url: "https://x.example.org/".into(),
                image_reject_flags: IMAGE_MODERATION
            })
        );
        assert_eq!(
            decide(Kind::Full, &img_bad, &txt_bad, &url_bad, &p),
            Outcome::Act(Action::Reject {
                flags: IMAGE_MODERATION | TEXT_MODERATION | URL_LAYER2
            })
        );
        // a reject decided by the text does not wait for an unavailable URL check
        assert_eq!(
            decide(Kind::Full, &pass, &txt_bad, &down, &p),
            Outcome::Act(Action::Reject {
                flags: TEXT_MODERATION
            })
        );
        // never approve without every answer
        for (i, t, u) in [
            (&down, &pass, &pass),
            (&pass, &down, &pass),
            (&pass, &pass, &down),
            (&img_bad, &pass, &down),
        ] {
            assert!(
                matches!(decide(Kind::Full, i, t, u, &p), Outcome::Retry { .. }),
                "{i:?} {t:?} {u:?}"
            );
        }
    }

    #[test]
    fn image_only_table_ignores_text_and_url() {
        let p = parts();
        for t in all() {
            for u in all() {
                assert!(matches!(
                    decide(Kind::ImageOnly, &Check::Pass, &t, &u, &p),
                    Outcome::Act(Action::ApproveImage { .. })
                ));
                assert_eq!(
                    decide(Kind::ImageOnly, &Check::fail(FILE, "x"), &t, &u, &p),
                    Outcome::Act(Action::Reject { flags: FILE })
                );
                assert!(matches!(
                    decide(Kind::ImageOnly, &Check::Unavailable("x".into()), &t, &u, &p),
                    Outcome::Retry { .. }
                ));
            }
        }
    }

    #[test]
    fn every_reject_has_a_reason_and_no_approval_has_an_unavailable_input() {
        let p = parts();
        let checks = || {
            vec![
                Check::Pass,
                Check::fail(IMAGE_MODERATION, "a"),
                Check::fail(URL_LAYER1, "b"),
                Check::Unavailable("c".into()),
            ]
        };
        for kind in [Kind::Full, Kind::ImageOnly] {
            for i in checks() {
                for t in checks() {
                    for u in checks() {
                        match decide(kind, &i, &t, &u, &p) {
                            Outcome::Act(Action::Reject { flags }) => assert_ne!(flags, 0),
                            Outcome::Act(_) => {
                                let used: Vec<&Check> = if kind == Kind::Full {
                                    vec![&i, &t, &u]
                                } else {
                                    vec![&i]
                                };
                                assert!(
                                    used.iter().all(|c| !matches!(c, Check::Unavailable(_))),
                                    "{kind:?} {i:?} {t:?} {u:?}"
                                );
                            }
                            Outcome::Retry { unavailable } => assert!(!unavailable.is_empty()),
                        }
                    }
                }
            }
        }
    }
}
