//! `{{name}}` placeholders in a template's message, title and branch.
//!
//! A fixed set of names per subject kind, replaced as text: no loops,
//! conditions or filters. An unknown name is an error when the template is
//! loaded, not when it is used.

use super::SubjectKind;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

/// Names every template can use.
const COMMON: &[&str] = &["project.name"];
/// `me.login` needs the subject's host to resolve, so it comes with one.
const ISSUE: &[&str] = &[
    "me.login",
    "issue.number",
    "issue.title",
    "issue.url",
    "issue.repo",
    "issue.author",
    "issue.labels",
    "issue.slug",
    "issue.body",
];
const PULL_REQUEST: &[&str] = &[
    "me.login",
    "pr.number",
    "pr.title",
    "pr.url",
    "pr.repo",
    "pr.author",
    "pr.labels",
    "pr.slug",
    "pr.body",
    "pr.head",
    "pr.base",
];

/// The placeholders a template of `kind` may use.
pub fn known(kind: SubjectKind) -> Vec<&'static str> {
    let specific: &[&str] = match kind {
        SubjectKind::Issue => ISSUE,
        SubjectKind::PullRequest => PULL_REQUEST,
        SubjectKind::None => &[],
    };
    COMMON.iter().chain(specific).copied().collect()
}

/// The placeholder names used in `text`, in order of appearance.
pub fn names(text: &str) -> Result<Vec<&str>> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            bail!("unclosed `{{{{` placeholder");
        };
        names.push(after[..end].trim());
        rest = &after[end + 2..];
    }
    Ok(names)
}

/// Check that `text` only uses placeholders a `kind` template knows.
pub fn validate(text: &str, kind: SubjectKind) -> Result<()> {
    let known = known(kind);
    for name in names(text)? {
        if !known.contains(&name) {
            bail!(
                "unknown placeholder `{{{{{name}}}}}`; a template with subject `{}` knows: {}",
                kind.as_str(),
                known.join(", ")
            );
        }
    }
    Ok(())
}

/// Replace every placeholder in `text` with its value. A name without a
/// value is an error (validated templates only use known names, and every
/// known name gets a value).
pub fn render(text: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            bail!("unclosed `{{{{` placeholder");
        };
        let name = after[..end].trim();
        let Some(value) = values.get(name) else {
            bail!("no value for placeholder `{{{{{name}}}}}`");
        };
        out.push_str(value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A title as a branch-name fragment: lowercase ASCII words joined by
/// dashes, at most 40 characters, cut at a word boundary.
pub fn slug(title: &str) -> String {
    let mut slug = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = word.to_ascii_lowercase();
        let extra = if slug.is_empty() { 0 } else { 1 };
        if slug.len() + extra + word.len() > 40 {
            if slug.is_empty() {
                slug.push_str(&word[..40]);
            }
            break;
        }
        if extra == 1 {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    slug
}

/// Make a rendered value safe inside a branch name: `git check-ref-format`
/// rejects spaces, `~^:?*[\`, `..` and a few more; replace anything but
/// ASCII alphanumerics, `-`, `_`, `.` and `/` by `-`.
pub fn branch_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut cleaned = cleaned.replace("..", "-");
    while cleaned.contains("//") {
        cleaned = cleaned.replace("//", "/");
    }
    cleaned
        .trim_matches(|c| c == '/' || c == '.' || c == '-')
        .trim_end_matches(".lock")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn placeholders_are_replaced_with_and_without_spaces() {
        let text = "Plan {{issue.url}} for {{ me.login }}.";
        let out = render(
            text,
            &values(&[("issue.url", "https://x/1"), ("me.login", "dk")]),
        )
        .unwrap();
        assert_eq!(out, "Plan https://x/1 for dk.");
    }

    #[test]
    fn unknown_names_are_reported_with_the_known_ones() {
        let err = validate("{{pr.number}}", SubjectKind::Issue)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`{{pr.number}}`"), "{err}");
        assert!(err.contains("issue.number"), "{err}");
        assert!(validate("{{pr.head}}", SubjectKind::PullRequest).is_ok());
        assert!(validate("{{project.name}}", SubjectKind::None).is_ok());
        assert!(validate("{{issue.url}}", SubjectKind::None).is_err());
    }

    #[test]
    fn an_unclosed_placeholder_is_an_error() {
        assert!(validate("Hi {{issue.url", SubjectKind::Issue).is_err());
        assert!(render("Hi {{issue.url", &BTreeMap::new()).is_err());
    }

    #[test]
    fn slugs_are_short_lowercase_words() {
        assert_eq!(slug("Add coupons at Checkout!"), "add-coupons-at-checkout");
        assert_eq!(
            slug("Ünïcode & a very long title that goes on and on and on"),
            "n-code-a-very-long-title-that-goes-on"
        );
        assert_eq!(slug(""), "");
    }

    #[test]
    fn branch_names_lose_what_git_rejects() {
        assert_eq!(branch_safe("dk/42-add coupons"), "dk/42-add-coupons");
        assert_eq!(branch_safe("a..b~c^d:e"), "a-b-c-d-e");
        assert_eq!(branch_safe("/x//y.lock"), "x/y");
    }
}
