#![forbid(unsafe_code)]

/// What the project access lists read from the engine that runs them.
pub mod access;
/// The ELF reader: whether a Linux program is loaded by glibc, and so resolves names over
/// TCP when the sandbox asks it to, judged from its program headers alone.
pub mod elf;
/// Why a project sandbox is refused: the one error type every sandbox build and install
/// returns, with the plain reason a refused Windows read root reads as.
pub mod error;
/// The plain refusal sentences, the log row and the owner's private temp folder a grant
/// of extra access reads.
pub mod grants;
/// A project's hidden folders: the bounds, the digest a launch records, the check that git
/// tracks none of them and the list of engine runs that can still open one.
pub mod hidden_paths;
/// What this Linux holds for an agent beyond its Landlock rules: namespaces, signal scope,
/// the call supervisor, root, AppArmor and its own network.
pub mod host;
/// The plain lines and refusal codes a launch shows when a Linux or Windows sandbox cannot
/// hold something, and the check that tells the Windows loopback refusal apart.
pub mod lines;
/// A sandboxed agent's network reach: open, or only the listed loopback and any-host ports.
pub mod net;
/// Path checks and spellings every sandbox root is held to: one spelling per path, the
/// canonical form, the plain-path tests for Unix and Windows, the owner's home and the
/// Seatbelt filter for a path and what is below it.
pub mod paths;
/// A project's agent policy: the access level, network, toolchains, caches, agent files,
/// grants and shared folders, with the views clients show and the hooks the engine passes.
pub mod policy;
/// The lowest port a sandbox bridge may take.
pub mod ports;
/// Boundary qualification: the signed report a release build carries for each sandbox
/// capability, read and checked against the artifacts it names before the engine admits a
/// boundary as qualified.
pub mod qualification;
/// The owner's app-wide sandbox settings: the three Linux options and the stored value with
/// the version of the read that showed it, as the engine saves them and every client reads
/// them.
pub mod settings;
/// The project folders an agent shares: the scope, the access and the picked names and
/// shared-back secrets, with the checks a client's choice is held to.
pub mod shared_folders;
/// The owner's toolchains an agent reads: the tool homes, the toolchain families and the
/// read-only grants a project's choice gives.
pub mod toolchains;
/// The Windows boundary's names: the helper beside the engine and the checks it passes, the
/// plain refusals, the grants folder and the AppContainer name of one agent of one project.
pub mod windows;

#[cfg(test)]
mod tests;
