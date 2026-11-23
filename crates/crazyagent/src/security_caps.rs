use kernel_security::error::StoreError;
use kernel_security::net as kernel_net;
use kernel_security::secrets::Secrets;
use sdk::generated::{service, update};
use sdk::secrets::Secret;
use sdk::settings::QuitSettings;
use sdk::{Refusal, net, process, secrets, settings};

const _: () = assert!(
    kernel_security::net::REPLY_BYTES_MAX == sdk::BYTES_MAX
        && kernel_security::net::REPLY_TEXT_BYTES_MAX == sdk::TEXT_BYTES_MAX
        && kernel_security::net::REPLY_LIST_MAX == sdk::LIST_ITEMS_MAX
);

/// The store codes a plugin's input or handle caused; any other code is a store failure.
const REJECTED_CODES: [&str; 10] = [
    "credential_name_invalid",
    "secret_scope_missing",
    "secret_owner_entry_missing",
    "secret_handle_refused",
    "secret_handles_full",
    "credential_reference_invalid",
    "net_url_invalid",
    "net_header_name_invalid",
    "net_request_invalid",
    "net_host_refused",
];

/// The refusal a plugin sees for a store error, its code unchanged: `Rejected` for a
/// refused input or handle, `Failed` for everything else. Input: the error. Output: the
/// refusal.
fn refusal(error: StoreError) -> Refusal {
    if REJECTED_CODES.contains(&error.0.as_str()) {
        Refusal::Rejected { code: error.0 }
    } else {
        Refusal::Failed { code: error.0 }
    }
}

/// The secrets capabilities a plugin calls, backed by the kernel secrets module of one
/// owner request; ids cross as SDK handles and never as values.
pub struct SecretsCaps<'r>(pub Secrets<'r>);

impl secrets::Provide for SecretsCaps<'_> {
    fn put_from_owner(&self, caller: &'static str, name: String) -> Result<Secret, Refusal> {
        self.0
            .put_from_owner(caller, &name)
            .map(Secret::from_kernel)
            .map_err(refusal)
    }

    fn find(&self, caller: &'static str, name: String) -> Result<Option<Secret>, Refusal> {
        self.0
            .find(caller, &name)
            .map(|found| found.map(Secret::from_kernel))
            .map_err(refusal)
    }

    fn forget(&self, caller: &'static str, secret: Secret) -> Result<(), Refusal> {
        self.0.forget(caller, secret.kernel_id()).map_err(refusal)
    }
}

/// The `net.request` capability a plugin calls, backed by the kernel request path of one
/// owner request; handles cross as kernel ids both ways and never as values.
pub struct NetCaps<'r>(pub kernel_net::Requests<'r>);

/// The kernel method of an SDK one. Input: the method. Output: the kernel method.
fn method(method: net::Method) -> kernel_net::Method {
    match method {
        net::Method::Get => kernel_net::Method::Get,
        net::Method::Post => kernel_net::Method::Post,
        net::Method::Put => kernel_net::Method::Put,
        net::Method::Patch => kernel_net::Method::Patch,
        net::Method::Delete => kernel_net::Method::Delete,
    }
}

/// The kernel request of an SDK one. Input: the request. Output: the kernel request.
fn kernel_request(request: net::Request) -> kernel_net::Request {
    let field = |field: net::Field| kernel_net::Field {
        name: field.name,
        value: field.value,
    };
    kernel_net::Request {
        method: method(request.method),
        url: request.url,
        headers: request.headers.into_iter().map(field).collect(),
        bearer: request.bearer.map(Secret::kernel_id),
        form: request.form.into_iter().map(field).collect(),
        secret_form: request
            .secret_form
            .into_iter()
            .map(|found| kernel_net::SecretField {
                name: found.name,
                secret: found.secret.kernel_id(),
            })
            .collect(),
        json: request.json,
        reply_headers: request.reply_headers,
        capture: request
            .capture
            .into_iter()
            .map(|capture| kernel_net::Capture {
                field: capture.field,
                keep_as: capture.keep_as,
            })
            .collect(),
    }
}

/// The SDK reply of a kernel one. Input: the reply. Output: the SDK reply.
fn sdk_reply(reply: kernel_net::Reply) -> net::Reply {
    net::Reply {
        status: reply.status,
        body: reply.body,
        headers: reply
            .headers
            .into_iter()
            .map(|field| net::Field {
                name: field.name,
                value: field.value,
            })
            .collect(),
        captured: reply
            .captured
            .into_iter()
            .map(|found| net::Captured {
                field: found.field,
                secret: Secret::from_kernel(found.secret),
            })
            .collect(),
    }
}

impl net::Provide for NetCaps<'_> {
    fn request(&self, caller: &'static str, request: net::Request) -> Result<net::Reply, Refusal> {
        self.0
            .request(caller, kernel_request(request))
            .map(sdk_reply)
            .map_err(refusal)
    }
}

/// The provider of a gateway built for one capability only: every other call fails with
/// `cap_provider_absent` and opens nothing, so the quit save never reaches the OS store.
pub struct Absent;

/// The failure every `Absent` call answers. Output: the refusal.
fn absent() -> Refusal {
    Refusal::Failed {
        code: "cap_provider_absent".to_owned(),
    }
}

impl secrets::Provide for Absent {
    fn put_from_owner(&self, _: &'static str, _: String) -> Result<Secret, Refusal> {
        Err(absent())
    }

    fn find(&self, _: &'static str, _: String) -> Result<Option<Secret>, Refusal> {
        Err(absent())
    }

    fn forget(&self, _: &'static str, _: Secret) -> Result<(), Refusal> {
        Err(absent())
    }
}

impl net::Provide for Absent {
    fn request(&self, _: &'static str, _: net::Request) -> Result<net::Reply, Refusal> {
        Err(absent())
    }
}

impl process::Provide for Absent {
    fn install(
        &self,
        _: &'static str,
        _: process::InstallRecipe,
    ) -> Result<process::InstallResult, Refusal> {
        Err(absent())
    }
}

impl settings::Provide for Absent {
    fn save_quit(&self, _: &'static str, _: QuitSettings) -> Result<(), Refusal> {
        Err(absent())
    }
}

impl service::Provide for Absent {
    fn status(&self, _: &'static str) -> Result<service::ServiceState, Refusal> {
        Err(absent())
    }

    fn repair(&self, _: &'static str) -> Result<(), Refusal> {
        Err(absent())
    }

    fn install(&self, _: &'static str) -> Result<(), Refusal> {
        Err(absent())
    }

    fn uninstall(&self, _: &'static str) -> Result<(), Refusal> {
        Err(absent())
    }
}

impl update::Provide for Absent {
    fn set_channel(&self, _: &'static str, _: update::UpdateChannel) -> Result<(), Refusal> {
        Err(absent())
    }
}

#[cfg(test)]
mod tests;
