// Git fetch, push, and push-with-retry logic (§6.5).
// US-011 implements this module.

use std::time::Duration;

use super::{GitError, RepoManager, PUSH_BACKOFF_SECS};

// ---------------------------------------------------------------------------
// SSH credentials helper
// ---------------------------------------------------------------------------

/// Build a [`git2::RemoteCallbacks`] that handles SSH and HTTPS authentication.
///
/// Strategy (tried in order):
/// 1. SSH agent - works when `ssh-agent` is running and has the key loaded,
///    which is the normal case on macOS (Keychain) and Linux (ssh-agent).
/// 2. Default key files - `~/.ssh/id_ed25519`, `~/.ssh/id_ecdsa`,
///    `~/.ssh/id_rsa` (tried in order, first existing file wins).
/// 3. HTTPS remotes - delegates to the system git credential helper.
///
/// libgit2 does NOT read `~/.ssh/config` or use the system SSH binary, so
/// this callback is required for any SSH remote URL.
///
/// The `called` flag prevents the infinite-retry loop that libgit2 triggers
/// when credentials are accepted by the callback but rejected by the server -
/// returning an error on the second invocation surfaces the real failure.
fn make_auth_callbacks<'cb>() -> git2::RemoteCallbacks<'cb> {
    let mut callbacks = git2::RemoteCallbacks::new();
    let mut called = false;

    callbacks.credentials(move |url, username_from_url, allowed_types| {
        if called {
            return Err(git2::Error::from_str(
                "SSH authentication failed (credentials rejected by server)",
            ));
        }
        called = true;

        let username = username_from_url.unwrap_or("git");

        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            // 1. Try SSH agent first.
            if let Ok(cred) = git2::Cred::ssh_key_from_agent(username) {
                return Ok(cred);
            }

            // 2. Fall back to well-known key files.
            if let Some(home) = dirs::home_dir() {
                let ssh_dir = home.join(".ssh");
                for key_name in &["id_ed25519", "id_ecdsa", "id_rsa"] {
                    let private_key = ssh_dir.join(key_name);
                    if private_key.exists() {
                        let public_key = ssh_dir.join(format!("{}.pub", key_name));
                        let pub_opt = public_key.exists().then_some(public_key.as_path());
                        return git2::Cred::ssh_key(username, pub_opt, &private_key, None);
                    }
                }
            }
        }

        // 3. HTTPS remotes: delegate to the system git credential helper.
        if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Ok(cfg) = git2::Config::open_default() {
                return git2::Cred::credential_helper(&cfg, url, username_from_url);
            }
        }

        Err(git2::Error::from_str(
            "no credentials available: ensure ssh-agent is running with your \
             key loaded, or a plain (unencrypted) key exists at \
             ~/.ssh/id_ed25519, ~/.ssh/id_ecdsa, or ~/.ssh/id_rsa. \
             Note: passphrase-protected keys must be loaded into ssh-agent \
             first — Chronicle cannot prompt for a passphrase",
        ))
    });

    callbacks
}

// ---------------------------------------------------------------------------
// Network-error classification
// ---------------------------------------------------------------------------

/// Returns `true` if `err` represents a transient network failure.
///
/// Network errors should be logged and the current sync cycle skipped;
/// the cron interval provides the next retry opportunity (§11.3).
#[must_use]
pub fn is_network_error(err: &GitError) -> bool {
    match err {
        GitError::Git2(e) => matches!(
            e.class(),
            git2::ErrorClass::Net
                | git2::ErrorClass::Http
                | git2::ErrorClass::Ssl
                | git2::ErrorClass::Ssh
        ),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// RepoManager - fetch / push
// ---------------------------------------------------------------------------

impl RepoManager {
    /// Fetch from the named remote.
    ///
    /// Updates `refs/remotes/<remote>/*` to mirror the server's
    /// `refs/heads/*`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Git2`] on any failure.  Use [`is_network_error`]
    /// to distinguish transient from fatal failures.
    pub fn fetch(&self, remote_name: &str) -> Result<(), GitError> {
        let mut remote = self.repo.find_remote(remote_name)?;
        let refspec = format!("+refs/heads/*:refs/remotes/{}/*", remote_name);
        let mut opts = git2::FetchOptions::new();
        opts.remote_callbacks(make_auth_callbacks());
        remote.fetch(&[refspec.as_str()], Some(&mut opts), None)?;
        Ok(())
    }

    /// Attempt a **single** push of the current HEAD branch to `remote_name`.
    ///
    /// Returns [`GitError::PushRejected`] when the remote rejects the update
    /// because it has advanced past our local tip.  All other git2 errors are
    /// returned as [`GitError::Git2`].
    ///
    /// # Errors
    ///
    /// See [`GitError`].
    pub(crate) fn push_head(&self, remote_name: &str) -> Result<(), GitError> {
        let mut remote = self.repo.find_remote(remote_name)?;

        // Use the configured branch name — never derive from HEAD, which can
        // vary by machine depending on the system's init.defaultBranch setting.
        let branch_ref = format!("refs/heads/{}", self.branch);
        let refspec = format!("{}:{}", branch_ref, branch_ref);

        // The push_update_reference callback fires once per pushed ref.
        // `status` is Some(error-message) on rejection, None on success.
        let mut rejection: Option<(String, String)> = None;
        {
            let rej = &mut rejection;
            let mut callbacks = make_auth_callbacks();
            callbacks.push_update_reference(move |refname, status| {
                if let Some(msg) = status {
                    *rej = Some((refname.to_owned(), msg.to_owned()));
                }
                Ok(())
            });
            let mut push_opts = git2::PushOptions::new();
            push_opts.remote_callbacks(callbacks);
            remote.push(&[refspec.as_str()], Some(&mut push_opts))?;
        }

        if let Some((refname, message)) = rejection {
            return Err(GitError::PushRejected { refname, message });
        }
        Ok(())
    }

    /// Push the current HEAD to `remote_name` with exponential-backoff retry.
    ///
    /// On push rejection, `on_rejection` is called to perform the
    /// fetch → re-merge → re-commit cycle; then the push is retried after
    /// the delay defined in [`PUSH_BACKOFF_SECS`].
    ///
    /// `sleep_fn` receives the duration to wait before each retry.  Pass
    /// `|d| std::thread::sleep(d)` in production and a no-op in tests.
    ///
    /// # Errors
    ///
    /// - [`GitError::PushExhausted`] - all retries failed
    /// - [`GitError::Git2`] - network or other hard failure (returned immediately)
    pub fn push_with_retry<F, S>(
        &self,
        remote_name: &str,
        mut on_rejection: F,
        sleep_fn: S,
    ) -> Result<(), GitError>
    where
        F: FnMut() -> Result<(), GitError>,
        S: Fn(Duration),
    {
        let rn = remote_name.to_owned();
        let mut try_push = || self.push_head(&rn);
        let backoff: Vec<Duration> = PUSH_BACKOFF_SECS
            .iter()
            .map(|&s| Duration::from_secs(s))
            .collect();
        run_push_retry(&mut try_push, &mut on_rejection, &sleep_fn, &backoff)
    }
}

// ---------------------------------------------------------------------------
// Core retry loop (pub(crate) for unit testing with mock closures)
// ---------------------------------------------------------------------------

/// Execute push with exponential-backoff retry (§6.5).
///
/// Algorithm:
/// 1. Call `try_push()` immediately (no initial delay).
/// 2. On [`GitError::PushRejected`]: sleep `backoff[i]`, call `on_rejection()`
///    (fetch → re-merge → re-commit), then retry.
/// 3. After all `backoff` entries are exhausted: return
///    [`GitError::PushExhausted`].
/// 4. On a network error (`is_network_error` is `true`): return immediately
///    without retrying.
/// 5. Any other error from `try_push` or `on_rejection` is returned as-is.
///
/// Total push attempts = 1 initial + `backoff.len()` retries.
///
/// # Errors
///
/// See [`GitError`].
pub(crate) fn run_push_retry<PushFn, RejectFn, SleepFn>(
    try_push: &mut PushFn,
    on_rejection: &mut RejectFn,
    sleep_fn: &SleepFn,
    backoff: &[Duration],
) -> Result<(), GitError>
where
    PushFn: FnMut() -> Result<(), GitError>,
    RejectFn: FnMut() -> Result<(), GitError>,
    SleepFn: Fn(Duration),
{
    // Initial attempt - no sleep.
    match try_push() {
        Ok(()) => return Ok(()),
        Err(e) if is_network_error(&e) => {
            tracing::error!("network error on initial push attempt: {}", e);
            return Err(e);
        }
        Err(e @ GitError::PushRejected { .. }) => {
            tracing::warn!("push rejected on initial attempt ({}); will retry", e);
        }
        Err(e) => return Err(e),
    }

    // Retry loop - up to `backoff.len()` retries.
    for (i, &delay) in backoff.iter().enumerate() {
        tracing::info!(
            attempt = i + 1,
            max_retries = backoff.len(),
            delay_secs = delay.as_secs(),
            "sleeping before push retry"
        );
        sleep_fn(delay);

        // Re-merge cycle: fetch → merge → commit (caller responsibility).
        on_rejection()?;

        match try_push() {
            Ok(()) => return Ok(()),
            Err(e) if is_network_error(&e) => {
                tracing::error!("network error on push retry {}: {}", i + 1, e);
                return Err(e);
            }
            Err(e @ GitError::PushRejected { .. }) => {
                tracing::warn!("push rejected on retry {}/{}: {}", i + 1, backoff.len(), e);
                // Continue to next backoff step.
            }
            Err(e) => return Err(e),
        }
    }

    let total_attempts = backoff.len() + 1; // initial + retries
    tracing::error!(attempts = total_attempts, "push exhausted all retries");
    Err(GitError::PushExhausted {
        attempts: total_attempts,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn push_rejected() -> GitError {
        GitError::PushRejected {
            refname: "refs/heads/main".to_owned(),
            message: "non-fast-forward".to_owned(),
        }
    }

    fn push_fatal() -> GitError {
        // A non-network, non-rejection error.
        GitError::Manifest("fatal error for testing".to_owned())
    }

    fn network_git_error() -> GitError {
        GitError::Git2(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::Net,
            "simulated network failure",
        ))
    }

    fn http_git_error() -> GitError {
        GitError::Git2(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::Http,
            "simulated HTTP failure",
        ))
    }

    const BACKOFF: [Duration; 3] = [
        Duration::ZERO,
        Duration::from_secs(5),
        Duration::from_secs(25),
    ];

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    /// Create an initial commit in `repo` so that HEAD is valid for push.
    fn create_initial_commit(repo: &git2::Repository) {
        let workdir = repo.workdir().expect("non-bare repo");
        std::fs::write(workdir.join("README.md"), b"# chronicle\n").expect("write README");
        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new("README.md"))
            .expect("add README");
        index.write().expect("write index");
        let tree_oid = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_oid).expect("find tree");
        let sig = git2::Signature::now("Test", "test@example.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .expect("commit");
    }

    // -----------------------------------------------------------------------
    // is_network_error
    // -----------------------------------------------------------------------

    #[test]
    fn is_network_error_true_for_net_class() {
        assert!(is_network_error(&network_git_error()));
    }

    #[test]
    fn is_network_error_true_for_http_class() {
        assert!(is_network_error(&http_git_error()));
    }

    #[test]
    fn is_network_error_true_for_ssl_class() {
        let e = GitError::Git2(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::Ssl,
            "ssl error",
        ));
        assert!(is_network_error(&e));
    }

    #[test]
    fn is_network_error_true_for_ssh_class() {
        let e = GitError::Git2(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::Ssh,
            "ssh error",
        ));
        assert!(is_network_error(&e));
    }

    #[test]
    fn is_network_error_false_for_push_rejected() {
        assert!(!is_network_error(&push_rejected()));
    }

    #[test]
    fn is_network_error_false_for_push_exhausted() {
        assert!(!is_network_error(&GitError::PushExhausted { attempts: 4 }));
    }

    #[test]
    fn is_network_error_false_for_manifest_error() {
        assert!(!is_network_error(&push_fatal()));
    }

    #[test]
    fn is_network_error_false_for_generic_git2_error() {
        let e = GitError::Git2(git2::Error::new(
            git2::ErrorCode::GenericError,
            git2::ErrorClass::None,
            "generic error",
        ));
        assert!(!is_network_error(&e));
    }

    // -----------------------------------------------------------------------
    // run_push_retry - success paths
    // -----------------------------------------------------------------------

    #[test]
    fn push_succeeds_on_first_attempt_no_retry() {
        let push_calls = RefCell::new(0u32);
        let sleep_calls: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || {
                *push_calls.borrow_mut() += 1;
                Ok(())
            },
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|d| sleep_calls.borrow_mut().push(d),
            &BACKOFF,
        );

        assert!(result.is_ok());
        assert_eq!(*push_calls.borrow(), 1, "only one push attempt");
        assert!(sleep_calls.borrow().is_empty(), "no sleep");
        assert_eq!(*reject_calls.borrow(), 0, "on_rejection not called");
    }

    #[test]
    fn push_rejected_once_retries_with_zero_delay() {
        let attempts = RefCell::new(0u32);
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || {
                let n = *attempts.borrow();
                *attempts.borrow_mut() += 1;
                if n == 0 {
                    Err(push_rejected())
                } else {
                    Ok(())
                }
            },
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|d| sleeps.borrow_mut().push(d),
            &BACKOFF,
        );

        assert!(result.is_ok());
        assert_eq!(*attempts.borrow(), 2);
        assert_eq!(*sleeps.borrow(), vec![Duration::ZERO]);
        assert_eq!(*reject_calls.borrow(), 1);
    }

    #[test]
    fn push_rejected_twice_uses_zero_then_five_second_backoff() {
        let attempts = RefCell::new(0u32);
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

        let result = run_push_retry(
            &mut || {
                let n = *attempts.borrow();
                *attempts.borrow_mut() += 1;
                if n < 2 {
                    Err(push_rejected())
                } else {
                    Ok(())
                }
            },
            &mut || Ok(()),
            &|d| sleeps.borrow_mut().push(d),
            &BACKOFF,
        );

        assert!(result.is_ok());
        assert_eq!(*attempts.borrow(), 3);
        assert_eq!(
            *sleeps.borrow(),
            vec![Duration::ZERO, Duration::from_secs(5)]
        );
    }

    // -----------------------------------------------------------------------
    // run_push_retry - exhaustion
    // -----------------------------------------------------------------------

    #[test]
    fn all_retries_exhausted_returns_push_exhausted() {
        let result = run_push_retry(
            &mut || Err(push_rejected()),
            &mut || Ok(()),
            &|_| {},
            &BACKOFF,
        );

        assert!(
            matches!(result, Err(GitError::PushExhausted { .. })),
            "expected PushExhausted, got {:?}",
            result
        );
    }

    #[test]
    fn push_exhausted_attempt_count_equals_initial_plus_retries() {
        let result = run_push_retry(
            &mut || Err(push_rejected()),
            &mut || Ok(()),
            &|_| {},
            &BACKOFF,
        );

        let Err(GitError::PushExhausted { attempts }) = result else {
            panic!("expected PushExhausted");
        };
        // initial(1) + backoff.len()(3) = 4
        assert_eq!(attempts, 4);
    }

    #[test]
    fn all_retries_exhausted_correct_sleep_and_reject_counts() {
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let reject_calls = RefCell::new(0u32);

        let _ = run_push_retry(
            &mut || Err(push_rejected()),
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|d| sleeps.borrow_mut().push(d),
            &BACKOFF,
        );

        assert_eq!(
            *sleeps.borrow(),
            vec![
                Duration::ZERO,
                Duration::from_secs(5),
                Duration::from_secs(25)
            ],
            "backoff timing must be 0s, 5s, 25s"
        );
        assert_eq!(
            *reject_calls.borrow(),
            3,
            "on_rejection called once per retry"
        );
    }

    // -----------------------------------------------------------------------
    // run_push_retry - error propagation
    // -----------------------------------------------------------------------

    #[test]
    fn on_rejection_failure_propagates_and_stops_retries() {
        let push_calls = RefCell::new(0u32);
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || {
                *push_calls.borrow_mut() += 1;
                Err(push_rejected())
            },
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Err(push_fatal())
            },
            &|_| {},
            &BACKOFF,
        );

        // on_rejection fails on first attempt → stop immediately
        assert!(result.is_err());
        assert!(
            !matches!(result, Err(GitError::PushRejected { .. })),
            "should not return PushRejected"
        );
        assert_eq!(
            *push_calls.borrow(),
            1,
            "only initial push before on_rejection fails"
        );
        assert_eq!(
            *reject_calls.borrow(),
            1,
            "on_rejection called exactly once"
        );
    }

    #[test]
    fn fatal_error_on_initial_push_returns_immediately() {
        let sleep_calls = RefCell::new(0u32);
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || Err(push_fatal()),
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|_| *sleep_calls.borrow_mut() += 1,
            &BACKOFF,
        );

        assert!(result.is_err());
        assert_eq!(*sleep_calls.borrow(), 0, "no sleep on fatal error");
        assert_eq!(
            *reject_calls.borrow(),
            0,
            "on_rejection not called on fatal error"
        );
    }

    #[test]
    fn network_error_on_initial_push_returns_immediately_without_retry() {
        let sleep_calls = RefCell::new(0u32);
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || Err(network_git_error()),
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|_| *sleep_calls.borrow_mut() += 1,
            &BACKOFF,
        );

        assert!(result.is_err());
        assert!(is_network_error(result.as_ref().unwrap_err()));
        assert_eq!(*sleep_calls.borrow(), 0, "no sleep on network error");
        assert_eq!(
            *reject_calls.borrow(),
            0,
            "on_rejection not called on network error"
        );
    }

    #[test]
    fn network_error_on_retry_stops_further_retries() {
        let attempts = RefCell::new(0u32);
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let reject_calls = RefCell::new(0u32);

        let result = run_push_retry(
            &mut || {
                let n = *attempts.borrow();
                *attempts.borrow_mut() += 1;
                if n == 0 {
                    Err(push_rejected()) // initial: rejected → retry
                } else {
                    Err(network_git_error()) // retry 1: network error → stop
                }
            },
            &mut || {
                *reject_calls.borrow_mut() += 1;
                Ok(())
            },
            &|d| sleeps.borrow_mut().push(d),
            &BACKOFF,
        );

        assert!(result.is_err());
        assert!(is_network_error(result.as_ref().unwrap_err()));
        assert_eq!(*attempts.borrow(), 2);
        // slept once (before retry 1), then network error stopped further retries
        assert_eq!(*sleeps.borrow(), vec![Duration::ZERO]);
        assert_eq!(*reject_calls.borrow(), 1);
    }

    // -----------------------------------------------------------------------
    // push_head + fetch smoke tests (actual git2 operations)
    // -----------------------------------------------------------------------

    fn make_local_repo(dir: &TempDir) -> crate::git::RepoManager {
        crate::git::RepoManager::init_or_open(dir.path(), None, "main").expect("init local repo")
    }

    fn make_bare_remote(dir: &TempDir) -> git2::Repository {
        git2::Repository::init_bare(dir.path()).expect("init bare repo")
    }

    #[test]
    fn push_head_to_bare_remote_succeeds() {
        let local_dir = tmp();
        let remote_dir = tmp();

        let manager = make_local_repo(&local_dir);
        make_bare_remote(&remote_dir);

        // Add remote and create an initial commit.
        manager
            .repository()
            .remote("origin", remote_dir.path().to_str().unwrap())
            .expect("add remote");
        create_initial_commit(manager.repository());

        manager
            .push_head("origin")
            .expect("push_head should succeed");

        // Verify the bare repo received the commit.
        let bare = git2::Repository::open_bare(remote_dir.path()).expect("open bare");
        assert!(bare.head().is_ok(), "remote HEAD must exist after push");
    }

    #[test]
    fn fetch_from_remote_after_push_succeeds() {
        let src_dir = tmp();
        let bare_dir = tmp();
        let dst_dir = tmp();

        // Source repo: commit + push to bare.
        let src = make_local_repo(&src_dir);
        make_bare_remote(&bare_dir);
        src.repository()
            .remote("origin", bare_dir.path().to_str().unwrap())
            .expect("add remote to src");
        create_initial_commit(src.repository());
        src.push_head("origin").expect("initial push");

        // Destination repo: fetch from bare.
        let dst = make_local_repo(&dst_dir);
        dst.repository()
            .remote("origin", bare_dir.path().to_str().unwrap())
            .expect("add remote to dst");

        dst.fetch("origin").expect("fetch should succeed");

        // Verify the fetch updated at least one remote-tracking ref.
        let has_tracking_ref = dst
            .repository()
            .references()
            .expect("list refs")
            .flatten()
            .any(|r| {
                r.name()
                    .map(|n| n.starts_with("refs/remotes/origin/"))
                    .unwrap_or(false)
            });
        assert!(
            has_tracking_ref,
            "remote-tracking ref must exist after fetch"
        );
    }
}
