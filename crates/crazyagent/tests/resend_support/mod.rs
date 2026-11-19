use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerRequest, OwnerResponse};
use std::time::{Duration, Instant};

/// The most time a resend waits for the exchange that answered its request to end.
const SETTLE_MAX: Duration = Duration::from_secs(10);

/// Sends an owner request again until the exchange that answered it has ended. A projects
/// home change settles after its reply while it still holds its claim on the request id, so
/// a resend in that window answers `InProgress` (DEC-159). Inputs: the controller and the
/// request. Output: the first other answer; past `SETTLE_MAX` the exchange never ended.
pub async fn settled(controller: &OwnerController, request: &OwnerRequest) -> OwnerResponse {
    let deadline = Instant::now() + SETTLE_MAX;
    loop {
        let client = controller.owner_client().unwrap();
        let response = client.request(request.clone()).await.unwrap().response;
        match &response {
            OwnerResponse::InProgress { operation_id } => {
                assert_eq!(operation_id, &request.request_id);
            }
            _ => return response,
        }
        assert!(
            Instant::now() < deadline,
            "the exchange that answered {:?} never ended",
            request.request_id
        );
    }
}
