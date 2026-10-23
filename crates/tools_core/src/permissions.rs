//! Permission mediation for potentially sensitive tool operations.

use crate::spec::{ToolSpec, capabilities};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// When to ask the user for permission before running a tool.
///
/// The tier only decides *whether* to ask; the actual question is routed
/// through the session's [`PermissionMediator`]. Tools may additionally
/// request permission for specific escalations (e.g. `execute_command`
/// asking to bypass the sandbox) independently of the tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionTier {
    /// Never ask; every tool call runs without prompting.
    #[default]
    BypassAll,
    /// A reviewer model ([`ToolCallReviewer`]) judges every call that is not
    /// plainly read-only — the calls [`PermissionTier::WriteTools`] would ask
    /// about, plus outward reads. Calls it finds safe and in line with what
    /// the user asked for run without prompting; the others are put to the
    /// user together with the reviewer's concern. Without a reviewer this
    /// tier asks like [`PermissionTier::WriteTools`].
    Auto,
    /// Ask before running tools whose effects leave the machine — anything
    /// tagged [`capabilities::OUTWARD`]. Local state changes run without
    /// prompting; the tag wins over [`capabilities::READ_ONLY`].
    OutwardTools,
    /// Ask before running tools that may modify state — anything not
    /// tagged [`capabilities::READ_ONLY`].
    WriteTools,
    /// Ask before running any tool.
    AllTools,
}

impl PermissionTier {
    /// Whether invoking a tool with the given spec requires asking first.
    pub fn requires_permission(&self, spec: &ToolSpec) -> bool {
        match self {
            PermissionTier::BypassAll => false,
            PermissionTier::Auto => {
                !spec.has_capability(capabilities::READ_ONLY)
                    || spec.has_capability(capabilities::OUTWARD)
            }
            PermissionTier::OutwardTools => spec.has_capability(capabilities::OUTWARD),
            PermissionTier::WriteTools => !spec.has_capability(capabilities::READ_ONLY),
            PermissionTier::AllTools => true,
        }
    }
}

/// A tool call put to a [`ToolCallReviewer`] under [`PermissionTier::Auto`].
#[derive(Debug)]
pub struct ToolCallReview<'a> {
    pub tool_name: &'a str,
    pub tool_description: &'a str,
    pub params: &'a serde_json::Value,
    /// What the user asked for and what the agent did so far: the user's
    /// messages and the agent's earlier tool calls, oldest first. Tool
    /// results and the agent's own prose are left out on purpose, so neither
    /// injected content nor the agent's rationalizations reach the reviewer.
    pub transcript: String,
}

/// A reviewer's judgement of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    /// Safe and within what the user asked for: run without asking.
    Allow,
    /// Put the call to the user; `concern` says what the reviewer worries
    /// about and is shown with the prompt.
    AskUser { concern: String },
}

/// Decides under [`PermissionTier::Auto`] whether a tool call may run
/// without asking the user — typically a second model reading the
/// conversation. A reviewer only ever takes a question off the user's
/// plate: an error is treated like [`ReviewVerdict::AskUser`].
#[async_trait]
pub trait ToolCallReviewer: Send + Sync {
    async fn review(&self, call: ToolCallReview<'_>) -> Result<ReviewVerdict>;
}

/// Renders the review transcript on demand; only calls that reach the
/// reviewer pay for it.
pub type ReviewTranscript<'a> = &'a (dyn Fn() -> String + Send + Sync);

/// Per-session permission state handed to the agent loop: the active tier
/// plus the tools the user granted for the rest of the session.
///
/// Cloning shares the grant set (it is session-scoped state), so the loop's
/// parallel execution path and sub-agents all see the same grants.
#[derive(Clone, Default)]
pub struct ToolPermissions {
    pub tier: PermissionTier,
    /// Judges calls under [`PermissionTier::Auto`]; with one, session grants
    /// do not bypass the review.
    pub reviewer: Option<Arc<dyn ToolCallReviewer>>,
    granted_tools: Arc<Mutex<HashSet<String>>>,
}

impl ToolPermissions {
    pub fn new(tier: PermissionTier) -> Self {
        Self {
            tier,
            reviewer: None,
            granted_tools: Arc::default(),
        }
    }

    pub fn with_reviewer(mut self, reviewer: Arc<dyn ToolCallReviewer>) -> Self {
        self.reviewer = Some(reviewer);
        self
    }

    pub fn is_granted(&self, tool_name: &str) -> bool {
        self.granted_tools.lock().unwrap().contains(tool_name)
    }

    pub fn grant(&self, tool_name: &str) {
        self.granted_tools
            .lock()
            .unwrap()
            .insert(tool_name.to_string());
    }

    /// Tier-based gate run by the agent loop before invoking a tool.
    ///
    /// Returns `Ok(())` when the invocation may proceed. The `Err` message is
    /// routed back to the LLM as the tool result, so it tells the model how
    /// to react to a denial.
    pub async fn check(
        &self,
        handler: Option<&dyn PermissionMediator>,
        spec: &ToolSpec,
        tool_id: Option<&str>,
        params: &serde_json::Value,
        transcript: ReviewTranscript<'_>,
    ) -> Result<()> {
        if !self.tier.requires_permission(spec) {
            return Ok(());
        }
        // Under review, a grant would cover every later call of the tool
        // (any shell command, say), so the reviewer sees granted tools too.
        let reviewed = self.tier == PermissionTier::Auto && self.reviewer.is_some();
        if !reviewed && self.is_granted(&spec.name) {
            return Ok(());
        }
        let concern = match (self.tier, &self.reviewer) {
            (PermissionTier::Auto, Some(reviewer)) => {
                let verdict = reviewer
                    .review(ToolCallReview {
                        tool_name: &spec.name,
                        tool_description: &spec.description,
                        params,
                        transcript: transcript(),
                    })
                    .await;
                match verdict {
                    Ok(ReviewVerdict::Allow) => return Ok(()),
                    Ok(ReviewVerdict::AskUser { concern }) => Some(concern),
                    Err(error) => Some(format!("The auto review failed: {error:#}")),
                }
            }
            _ => None,
        };
        let Some(handler) = handler else {
            match concern {
                Some(concern) => anyhow::bail!(
                    "The auto review did not approve tool '{}' and this frontend cannot ask \
                     the user: {concern} Do not try to reach the same outcome another way.",
                    spec.name
                ),
                None => anyhow::bail!(
                    "Permission is required to run tool '{}', but this frontend cannot ask the \
                     user. The call was denied by policy.",
                    spec.name
                ),
            }
        };
        let decision = handler
            .request_permission(PermissionRequest {
                tool_id,
                tool_name: &spec.name,
                reason: PermissionRequestReason::ToolInvocation {
                    params,
                    concern: concern.as_deref(),
                },
            })
            .await?;
        match decision {
            PermissionDecision::GrantedOnce => Ok(()),
            PermissionDecision::GrantedSession | PermissionDecision::GrantedPersistent => {
                self.grant(&spec.name);
                Ok(())
            }
            PermissionDecision::Denied => anyhow::bail!(
                "The user denied permission to run tool '{}'. Do not simply retry; \
                 ask the user how to proceed or try a different approach.",
                spec.name
            ),
        }
    }
}

/// Context about why permission is being requested.
#[derive(Debug)]
pub enum PermissionRequestReason<'a> {
    ExecuteCommand {
        command_line: &'a str,
        working_dir: Option<&'a Path>,
    },
    /// Tier-based gate before invoking a tool ([`PermissionTier`]).
    ToolInvocation {
        params: &'a serde_json::Value,
        /// Why the auto review ([`PermissionTier::Auto`]) asks the user
        /// instead of approving the call itself.
        concern: Option<&'a str>,
    },
    /// Approve loading a project's local `.mcp.json`, which launches the MCP
    /// servers it defines — i.e. runs project-supplied commands. Not tied to a
    /// tool invocation; raised before a project's registry is built.
    TrustLocalMcp {
        project_dir: &'a Path,
        server_names: Vec<String>,
    },
}

/// Request payload passed to a [`PermissionMediator`].
#[derive(Debug)]
pub struct PermissionRequest<'a> {
    pub tool_id: Option<&'a str>,
    pub tool_name: &'a str,
    pub reason: PermissionRequestReason<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Allow this single invocation; ask again next time.
    GrantedOnce,
    /// Allow for the rest of this session (in-memory grant).
    GrantedSession,
    /// Allow and remember durably — the embedder persists the approval so it
    /// survives across sessions and restarts (e.g. trusting a project's local
    /// `.mcp.json`). Ordinary tier-based tool prompts do not offer this; a
    /// tool that receives it degrades to a session grant, since there is no
    /// durable per-tool grant store.
    GrantedPersistent,
    Denied,
}

#[async_trait]
pub trait PermissionMediator: Send + Sync {
    async fn request_permission(
        &self,
        request: PermissionRequest<'_>,
    ) -> Result<PermissionDecision>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mediator returning a fixed decision and counting how often it is asked.
    struct ScriptedMediator {
        decision: PermissionDecision,
        calls: AtomicUsize,
    }

    impl ScriptedMediator {
        fn new(decision: PermissionDecision) -> Self {
            Self {
                decision,
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl PermissionMediator for ScriptedMediator {
        async fn request_permission(
            &self,
            _request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.decision)
        }
    }

    fn spec_with(tags: &'static [&'static str]) -> ToolSpec {
        ToolSpec {
            name: "test_tool".into(),
            description: "test".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
            annotations: None,
            capabilities: ToolSpec::capabilities(tags),
            multiline_params: &[],
            hidden: false,
            title_template: None,
        }
    }

    #[test]
    fn bypass_all_never_requires_permission() {
        let tier = PermissionTier::BypassAll;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(!tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(!tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn write_tools_requires_permission_unless_read_only() {
        let tier = PermissionTier::WriteTools;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        // Untagged tools (e.g. MCP tools without a read-only hint) count as writes.
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn outward_tools_requires_permission_only_for_outward_tagged_tools() {
        let tier = PermissionTier::OutwardTools;
        assert!(tier.requires_permission(&spec_with(&[capabilities::OUTWARD])));
        // Outward wins over read-only: reading via an outward service still
        // leaks the request to a third party.
        assert!(tier.requires_permission(&spec_with(&[
            capabilities::READ_ONLY,
            capabilities::OUTWARD
        ])));
        assert!(!tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(!tier.requires_permission(&spec_with(&[])));
    }

    #[test]
    fn outward_tools_tier_serializes_as_kebab_case() {
        assert_eq!(
            serde_json::to_string(&PermissionTier::OutwardTools).unwrap(),
            "\"outward-tools\""
        );
        let parsed: PermissionTier = serde_json::from_str("\"outward-tools\"").unwrap();
        assert_eq!(parsed, PermissionTier::OutwardTools);
    }

    #[test]
    fn all_tools_always_requires_permission() {
        let tier = PermissionTier::AllTools;
        assert!(tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[tokio::test]
    async fn bypass_tier_does_not_consult_the_mediator() {
        let permissions = ToolPermissions::new(PermissionTier::BypassAll);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[capabilities::EDITS_FILES]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 0);
    }

    #[tokio::test]
    async fn write_tier_skips_read_only_tools() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[capabilities::READ_ONLY]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 0);
    }

    #[tokio::test]
    async fn denied_decision_becomes_an_error() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        let err = permissions
            .check(
                Some(&mediator),
                &spec_with(&[]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied permission"));
        assert_eq!(mediator.call_count(), 1);
    }

    #[tokio::test]
    async fn granted_session_is_remembered_per_tool() {
        let permissions = ToolPermissions::new(PermissionTier::AllTools);
        let mediator = ScriptedMediator::new(PermissionDecision::GrantedSession);
        let params = serde_json::json!({});
        let spec = spec_with(&[capabilities::READ_ONLY]);
        permissions
            .check(Some(&mediator), &spec, None, &params, &String::new)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params, &String::new)
            .await
            .unwrap();
        // Second call is served from the session grant, and clones share it.
        assert_eq!(mediator.call_count(), 1);
        assert!(permissions.clone().is_granted("test_tool"));
    }

    #[tokio::test]
    async fn granted_once_asks_again_next_time() {
        let permissions = ToolPermissions::new(PermissionTier::AllTools);
        let mediator = ScriptedMediator::new(PermissionDecision::GrantedOnce);
        let params = serde_json::json!({});
        let spec = spec_with(&[]);
        permissions
            .check(Some(&mediator), &spec, None, &params, &String::new)
            .await
            .unwrap();
        permissions
            .check(Some(&mediator), &spec, None, &params, &String::new)
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 2);
    }

    #[tokio::test]
    async fn missing_handler_denies_when_tier_requires_asking() {
        let permissions = ToolPermissions::new(PermissionTier::WriteTools);
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params, &String::new)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot ask the user"));
    }

    /// Reviewer returning a fixed verdict (or failing) and recording the
    /// transcript it was shown.
    struct ScriptedReviewer {
        verdict: Option<ReviewVerdict>,
        transcripts: Mutex<Vec<String>>,
    }

    impl ScriptedReviewer {
        fn new(verdict: Option<ReviewVerdict>) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                transcripts: Mutex::default(),
            })
        }

        fn review_count(&self) -> usize {
            self.transcripts.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ToolCallReviewer for ScriptedReviewer {
        async fn review(&self, call: ToolCallReview<'_>) -> Result<ReviewVerdict> {
            self.transcripts.lock().unwrap().push(call.transcript);
            self.verdict
                .clone()
                .ok_or_else(|| anyhow::anyhow!("reviewer offline"))
        }
    }

    /// Mediator recording the concern it was asked with.
    struct ConcernRecorder {
        concerns: Mutex<Vec<Option<String>>>,
    }

    #[async_trait]
    impl PermissionMediator for ConcernRecorder {
        async fn request_permission(
            &self,
            request: PermissionRequest<'_>,
        ) -> Result<PermissionDecision> {
            if let PermissionRequestReason::ToolInvocation { concern, .. } = request.reason {
                self.concerns
                    .lock()
                    .unwrap()
                    .push(concern.map(str::to_string));
            }
            Ok(PermissionDecision::GrantedOnce)
        }
    }

    fn auto_permissions(reviewer: Arc<ScriptedReviewer>) -> ToolPermissions {
        ToolPermissions::new(PermissionTier::Auto).with_reviewer(reviewer)
    }

    #[test]
    fn auto_tier_reviews_writes_and_outward_reads() {
        let tier = PermissionTier::Auto;
        assert!(!tier.requires_permission(&spec_with(&[capabilities::READ_ONLY])));
        assert!(tier.requires_permission(&spec_with(&[
            capabilities::READ_ONLY,
            capabilities::OUTWARD
        ])));
        assert!(tier.requires_permission(&spec_with(&[capabilities::EDITS_FILES])));
        assert!(tier.requires_permission(&spec_with(&[])));
    }

    #[tokio::test]
    async fn auto_tier_runs_approved_calls_without_asking() {
        let reviewer = ScriptedReviewer::new(Some(ReviewVerdict::Allow));
        let permissions = auto_permissions(reviewer.clone());
        let mediator = ScriptedMediator::new(PermissionDecision::Denied);
        let params = serde_json::json!({});
        permissions
            .check(Some(&mediator), &spec_with(&[]), None, &params, &|| {
                "User: fix the build".to_string()
            })
            .await
            .unwrap();
        assert_eq!(mediator.call_count(), 0);
        assert_eq!(
            *reviewer.transcripts.lock().unwrap(),
            vec!["User: fix the build".to_string()]
        );
    }

    #[tokio::test]
    async fn auto_tier_skips_the_reviewer_for_read_only_tools() {
        let reviewer = ScriptedReviewer::new(Some(ReviewVerdict::Allow));
        let permissions = auto_permissions(reviewer.clone());
        let params = serde_json::json!({});
        permissions
            .check(
                None,
                &spec_with(&[capabilities::READ_ONLY]),
                None,
                &params,
                &|| panic!("no transcript needed"),
            )
            .await
            .unwrap();
        assert_eq!(reviewer.review_count(), 0);
    }

    #[tokio::test]
    async fn auto_tier_asks_the_user_with_the_reviewers_concern() {
        let reviewer = ScriptedReviewer::new(Some(ReviewVerdict::AskUser {
            concern: "Deletes the remote branch.".into(),
        }));
        let permissions = auto_permissions(reviewer);
        let mediator = ConcernRecorder {
            concerns: Mutex::default(),
        };
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap();
        assert_eq!(
            *mediator.concerns.lock().unwrap(),
            vec![Some("Deletes the remote branch.".to_string())]
        );
    }

    #[tokio::test]
    async fn auto_tier_asks_the_user_when_the_review_fails() {
        let permissions = auto_permissions(ScriptedReviewer::new(None));
        let mediator = ConcernRecorder {
            concerns: Mutex::default(),
        };
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap();
        let concerns = mediator.concerns.lock().unwrap();
        assert!(concerns[0].as_deref().unwrap().contains("reviewer offline"));
    }

    #[tokio::test]
    async fn auto_tier_denies_unapproved_calls_without_a_mediator() {
        let permissions = auto_permissions(ScriptedReviewer::new(Some(ReviewVerdict::AskUser {
            concern: "Uploads the .env file.".into(),
        })));
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params, &String::new)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Uploads the .env file."));
    }

    #[tokio::test]
    async fn auto_tier_without_reviewer_asks_like_write_tools() {
        let permissions = ToolPermissions::new(PermissionTier::Auto);
        let mediator = ConcernRecorder {
            concerns: Mutex::default(),
        };
        let params = serde_json::json!({});
        permissions
            .check(
                Some(&mediator),
                &spec_with(&[]),
                None,
                &params,
                &String::new,
            )
            .await
            .unwrap();
        assert_eq!(*mediator.concerns.lock().unwrap(), vec![None]);
    }

    #[tokio::test]
    async fn auto_tier_reviews_granted_tools_too() {
        let reviewer = ScriptedReviewer::new(Some(ReviewVerdict::AskUser {
            concern: "Wipes the home directory.".into(),
        }));
        let permissions = auto_permissions(reviewer.clone());
        permissions.grant("test_tool");
        let params = serde_json::json!({});
        let err = permissions
            .check(None, &spec_with(&[]), None, &params, &String::new)
            .await
            .unwrap_err();
        assert_eq!(reviewer.review_count(), 1);
        assert!(err.to_string().contains("Wipes the home directory."));
    }

    #[test]
    fn tier_serializes_as_kebab_case_and_defaults_to_bypass_all() {
        assert_eq!(PermissionTier::default(), PermissionTier::BypassAll);
        assert_eq!(
            serde_json::to_string(&PermissionTier::WriteTools).unwrap(),
            "\"write-tools\""
        );
        let parsed: PermissionTier = serde_json::from_str("\"all-tools\"").unwrap();
        assert_eq!(parsed, PermissionTier::AllTools);
        let parsed: PermissionTier = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(parsed, PermissionTier::Auto);
    }
}
