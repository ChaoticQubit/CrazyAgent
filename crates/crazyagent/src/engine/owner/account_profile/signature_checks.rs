use super::*;
use crate::client_controller::OwnerController;
use crate::engine::owner::transport::OwnerClient;
use std::future::Future;

fn controller_configure<'a>(
    controller: &'a mut OwnerController,
    request: AccountProfileRequest,
    secret: Option<AccountSecret>,
    generation: u64,
) -> impl Future<Output = Result<AccountProfileReply, EngineError>> + 'a {
    controller.configure_account_profile(request, secret, generation)
}

fn controller_read(
    controller: &mut OwnerController,
    request: AccountProfileRequest,
) -> impl Future<Output = Result<AccountProfileReply, EngineError>> + '_ {
    controller.read_account_profile(request)
}

fn controller_capabilities(
    controller: &mut OwnerController,
    request: AccountProfileRequest,
) -> impl Future<Output = Result<AccountAuthCapabilitiesReply, EngineError>> + '_ {
    controller.read_account_auth_capabilities(request)
}

fn client_configure(
    client: &OwnerClient,
    request: AccountProfileRequest,
    secret: Option<AccountSecret>,
) -> impl Future<Output = Result<AccountProfileReply, EngineError>> + '_ {
    client.configure_account_profile(request, secret)
}

fn client_read(
    client: &OwnerClient,
    request: AccountProfileRequest,
) -> impl Future<Output = Result<AccountProfileReply, EngineError>> + '_ {
    client.read_account_profile(request)
}

fn client_capabilities(
    client: &OwnerClient,
    request: AccountProfileRequest,
) -> impl Future<Output = Result<AccountAuthCapabilitiesReply, EngineError>> + '_ {
    client.read_account_auth_capabilities(request)
}

#[test]
fn imp1035_signatures_preserve_async_controller_and_client_contract() {
    let _ = controller_configure;
    let _ = controller_read;
    let _ = controller_capabilities;
    let _ = client_configure;
    let _ = client_read;
    let _ = client_capabilities;
}
