//! Live check against the real Claude Code session API (ticket #11): does
//! the server keep the custom tag a create sends (`zeron:<first prompt
//! uuid>`), so a create whose answer was lost can be found in the list?
//!
//! Ignored by default, and also needs `ZERON_LIVE_TAG_CHECK=1`. It uses this
//! device's own `claude` login (`~/.claude`) read-only, with Zeron's own
//! identity, and makes exactly one session, which it always archives (even
//! when a check fails or panics, and, when the create's answer is lost, the
//! session it finds by its tag) before it asserts anything. It prints only
//! yes/no findings and status words: never a token, an account, an org, a
//! session id, or an error's text (which could carry one).
//!
//! ```text
//! ZERON_LIVE_TAG_CHECK=1 cargo test -p zeron-harness --test claude_cloud_tag_live -- --ignored --nocapture
//! ```

use futures::FutureExt;
use zeron_harness::claude_cloud::api::SessionInfo;
use zeron_harness::claude_cloud::{ClaudeCloudConfig, ClaudeCloudHarness};
use zeron_harness::provider_cloud::{CreateSession, ProviderCloud, ProviderCloudError};

/// An error's kind, never its text.
fn kind(error: &ProviderCloudError) -> &'static str {
    match error {
        ProviderCloudError::SignInRequired(_) => "sign-in required",
        ProviderCloudError::AccountMismatch(_) => "account mismatch",
        ProviderCloudError::Provider { .. } => "provider refused",
        ProviderCloudError::Disabled(_) => "disabled",
        ProviderCloudError::Uncertain(_) => "outcome unknown",
        ProviderCloudError::Other(text) if text.contains("no cloud environment") => {
            "no cloud environment on the account"
        }
        ProviderCloudError::Other(text) if text.contains("can't read") => {
            "an answer Zeron can't read"
        }
        ProviderCloudError::Other(text) if text.contains("could not be reached") => {
            "not reached"
        }
        ProviderCloudError::Other(_) => "other",
    }
}

#[derive(Default)]
struct Findings {
    single_read: Option<bool>,
    single_read_has_tag: Option<bool>,
    status_before: Option<String>,
    found_by_tag: Option<bool>,
    before: Option<SessionInfo>,
}

#[tokio::test]
#[ignore = "talks to the real Claude Code session API"]
async fn the_server_keeps_zerons_create_tag() {
    if std::env::var("ZERON_LIVE_TAG_CHECK").as_deref() != Ok("1") {
        eprintln!("set ZERON_LIVE_TAG_CHECK=1 to run");
        return;
    }
    let config = ClaudeCloudConfig::device_default().expect("a home folder");
    let harness = ClaudeCloudHarness::new(config).expect("harness builds");
    let prompt_uuid = uuid::Uuid::new_v4().to_string();
    let tag = format!("zeron:{prompt_uuid}");
    let since = chrono::Utc::now() - chrono::Duration::minutes(5);

    let created = harness
        .create_session(CreateSession {
            prompt: "Reply with the single word OK.".into(),
            prompt_uuid: prompt_uuid.clone(),
            model: None,
            title: Some("Zeron tag check (archive me)".into()),
            source: None,
        })
        .await;
    let session = match created {
        Ok(session) => {
            eprintln!("created: yes");
            session
        }
        Err(error) => {
            eprintln!("create failed: {}", kind(&error));
            // A transport failure's text names no credential, account or
            // session (the URL is cut out of it): it says what broke.
            if let ProviderCloudError::Other(text) = &error
                && text.contains("could not be reached")
            {
                eprintln!("transport: {text}");
            }
            // It may still have made the session: find it by its tag and
            // archive it.
            match harness.find_created(&prompt_uuid, since).await {
                Ok(Some(session)) => {
                    eprintln!("found the lost create by its tag: yes; archiving it");
                    let archived = harness.archive_session(&session).await;
                    eprintln!("archived: {}", archived.is_ok());
                }
                Ok(None) => eprintln!("no session with the tag: nothing to archive"),
                Err(error) => eprintln!("tag lookup failed: {}", kind(&error)),
            }
            panic!("the create failed");
        }
    };

    // Steps that may fail or panic: the archive below runs regardless.
    let mut findings = Findings::default();
    let checks = std::panic::AssertUnwindSafe(async {
        let account = harness
            .client()
            .credentials()
            .account()
            .await
            .expect("the login reads");
        let before = harness
            .client()
            .session(&account, &session.session_id)
            .await;
        findings.single_read = Some(before.is_ok());
        if let Ok(info) = &before {
            findings.single_read_has_tag = Some(info.tags.contains(&tag));
            findings.status_before = info.status.clone();
        }
        findings.before = before.ok();
        let found = harness.find_created(&prompt_uuid, since).await;
        findings.found_by_tag =
            Some(matches!(&found, Ok(Some(s)) if s.session_id == session.session_id));
        if let Err(error) = &found {
            eprintln!("list read failed: {}", kind(error));
        }
    })
    .catch_unwind()
    .await;

    // Always archive the one session this test made.
    let archived = harness.archive_session(&session).await;
    eprintln!("archive call worked: {}", archived.is_ok());
    if let Err(error) = &archived {
        eprintln!("archive failed: {}", kind(error));
    }
    let after = match harness.client().credentials().account().await {
        Ok(account) => harness
            .client()
            .session(&account, &session.session_id)
            .await
            .ok(),
        Err(_) => None,
    };
    let archived_now = harness.session_archived(&session).await;

    eprintln!("checks ran to the end: {}", checks.is_ok());
    eprintln!("single-session read worked: {:?}", findings.single_read);
    eprintln!("single-session read has the tag: {:?}", findings.single_read_has_tag);
    eprintln!("status before archive: {:?}", findings.status_before);
    eprintln!("found in the list by its tag: {:?}", findings.found_by_tag);
    if let Some(after) = &after {
        eprintln!("status after archive: {:?}", after.status);
        if let Some(before) = &findings.before {
            eprintln!(
                "archiving moved last_event_at: {}",
                before.last_event_at != after.last_event_at
            );
        }
    }
    eprintln!(
        "session_archived after archive: {:?}",
        archived_now.as_ref().map_err(kind)
    );

    assert!(archived.is_ok(), "the session must be archived");
    assert!(checks.is_ok(), "a check panicked");
    assert_eq!(findings.found_by_tag, Some(true), "the server should keep the zeron: tag");
}
