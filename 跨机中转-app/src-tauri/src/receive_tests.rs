use super::*;

fn session(id: &str) -> ReceiveSession {
    ReceiveSession {
        session_id: id.into(),
        alias: "test".into(),
        sender: SenderTarget {
            host: "127.0.0.1".into(),
            port: 53317,
            protocol: ProtocolType::Http,
            fingerprint: "test".into(),
        },
        files: HashMap::new(),
        saved_paths: HashMap::new(),
        total_bytes: 0,
        finished_files: 0,
        failed_files: 0,
        finalized_bytes: 0,
        in_progress: HashMap::new(),
        started: Instant::now(),
        last_activity: Instant::now(),
        ended: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_publishes_session_before_upload_can_observe_it() {
    let receive = Arc::new(AsyncMutex::new(None::<ReceiveSession>));
    let (decision_tx, decision_rx) = oneshot::channel();
    let receive_peer = receive.clone();
    let upload = tokio::spawn(async move {
        assert!(matches!(decision_rx.await.unwrap(), PrepareUploadDecisionV2::Accept(_)));
        let guard = receive_peer.lock().await;
        assert_eq!(guard.as_ref().unwrap().session_id, "new");
    });
    accept_receive_session(&receive, session("new"), decision_tx).await.unwrap();
    upload.await.unwrap();
}

#[tokio::test]
async fn expired_acceptance_restores_previous_session() {
    let receive = AsyncMutex::new(Some(session("previous")));
    let (tx, rx) = oneshot::channel();
    drop(rx);
    assert!(accept_receive_session(&receive, session("expired"), tx).await.is_err());
    assert_eq!(receive.lock().await.as_ref().unwrap().session_id, "previous");
}

#[test]
fn idle_session_becomes_stale_after_timeout() {
    let mut s = session("old");
    s.last_activity = Instant::now() - RECEIVE_SESSION_IDLE_TIMEOUT - Duration::from_secs(1);
    assert!(receive_session_is_stale(&s, Instant::now()));
}

#[test]
fn recently_active_session_is_not_stale() {
    let s = session("active");
    assert!(!receive_session_is_stale(&s, Instant::now()));
}

#[tokio::test]
async fn stale_session_is_cleared_before_a_new_upload() {
    let mut s = session("old");
    s.last_activity = Instant::now() - RECEIVE_SESSION_IDLE_TIMEOUT - Duration::from_secs(1);
    let receive = AsyncMutex::new(Some(s));
    assert_eq!(clear_stale_receive(&receive).await.as_deref(), Some("old"));
    assert!(receive.lock().await.is_none());
}

#[test]
fn status_error_preserves_windows_paths_without_debug_escaping() {
    let reason = r#"Failed to create C:\Users\xiaofeng\Downloads\裂隙灯课题.zip: Access is denied (os error 5)"#;
    let err = ClientError::StatusCode(localsend::http::StatusCodeError {
        status: 500,
        message: Some(reason.into()),
    });
    assert_eq!(reqwest_error_detail(&err), reason);
}
