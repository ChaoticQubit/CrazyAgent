use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_logged_out_account_reads_as_not_signed_in_for_every_launch() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        run.sign_in(&restart).await;
        assert_eq!(restart.launch(), Ok(()));
        run.log_out(&restart).await;
        assert_eq!(
            restart.launch(),
            Err("master_authentication_unknown".into())
        );
        assert_eq!(run.status().await, AccountStatus::NotSignedIn);
        run.stop().await;
    })
    .await
    .expect("the logout case exceeded 60 seconds");
}
