//! An issue or pull request named by its web URL.

use crate::session_templates::SubjectKind;
use github::RepoRef;

/// `https://host/owner/name/(issues|pull)/<number>`, anything after the
/// number (a fragment, a tab) ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectUrl {
    pub repo: RepoRef,
    pub kind: SubjectKind,
    pub number: u64,
}

impl SubjectUrl {
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("https://")?;
        let mut parts = rest.split(['/', '#', '?']);
        let (host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
        // A web link names no user; the remote parser would drop one.
        if host.contains('@') {
            return None;
        }
        let kind = match parts.next()? {
            "issues" => SubjectKind::Issue,
            "pull" => SubjectKind::PullRequest,
            _ => return None,
        };
        let number: u64 = parts.next()?.parse().ok().filter(|n| *n > 0)?;
        // The remote parser checks host, owner and name as the client wants
        // them before a token is sent anywhere.
        let repo = RepoRef::from_remote_url(&format!("https://{host}/{owner}/{name}"))?;
        Some(Self { repo, kind, number })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_pull_request_urls() {
        let issue = SubjectUrl::parse("https://github.com/acme/shop/issues/42").unwrap();
        assert_eq!(issue.repo, RepoRef::new("github.com", "acme", "shop"));
        assert_eq!(issue.kind, SubjectKind::Issue);
        assert_eq!(issue.number, 42);
        let pr = SubjectUrl::parse("https://GHE.corp/acme/shop/pull/7/files#diff").unwrap();
        assert_eq!(pr.repo.host, "ghe.corp");
        assert_eq!(pr.kind, SubjectKind::PullRequest);
    }

    #[test]
    fn other_urls_are_refused() {
        for url in [
            "https://github.com/acme/shop/wiki/7",
            "https://github.com/acme/shop/issues/0",
            "http://github.com/acme/shop/issues/1",
            "https://evil.com@x/acme/shop/issues/1",
            "https://github.com/acme/../issues/1",
        ] {
            assert_eq!(SubjectUrl::parse(url), None, "{url}");
        }
    }
}
