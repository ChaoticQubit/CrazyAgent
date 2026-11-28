/** What the window says when a project's extra grant (a socket, a port or the temp folder) is
 * refused, full or removed, one sentence per code, the same on every page. Input: none.
 * Output: the sentence for each code. */
export const GRANT_COPY: Record<string, string> = {
  grant_socket_needs_macos: "A socket can be allowed only on macOS in this version.",
  grant_socket_not_plain: "The socket path must be a full path of letters, digits, slashes, dots, dashes and underscores, at most 103 bytes, in a folder that exists.",
  grant_socket_secret: "That socket sits with your keys or credentials, so it is never allowed.",
  grant_socket_engine: "That socket belongs to CrazyAgent itself, so it is never allowed.",
  grant_socket_agent: "That socket controls a terminal, an editor or an SSH connection (tmux, screen, dtach, Zellij, kitty, WezTerm, Alacritty, iTerm2, Emacs, Neovim, Kakoune, VS Code, Cursor and the like) or hands out your SSH keys, which can run anything as you, so it is never allowed.",
  grant_socket_host: "That socket controls containers or virtual machines (Docker, containerd, Podman, Colima, Lima, OrbStack and the like), which can reach your whole computer, so it is never allowed.",
  grant_socket_keys: "That socket belongs to a key or password agent (gpg-agent, 1Password, SSH or another agent), so it is never allowed.",
  grant_socket_owner: "That socket belongs to the system or another user, has a second name, or sits in a folder other users can change, so it is never allowed.",
  grant_socket_missing: "There is no socket at that path now. Start the program that makes it, then allow it again.",
  grant_socket_refused: "That socket cannot be allowed. A socket can be allowed only when it is there now, belongs to the owner with one name, sits in a folder only the owner can change, and is no socket of CrazyAgent itself, of keys or credentials, of containers or virtual machines, or of a terminal, editor, SSH connection or key agent. Check the path and that the program making it runs, then ask again.",
  grant_port_reserved: "That port belongs to your sign-in, your local model or CrazyAgent itself (its IDE or model proxy), or controls Docker, a browser or a debugger (Node, Java, Python or Go), so it is never allowed.",
  grant_port_dynamic: "Ports from 49152 up are lent by the system to CrazyAgent's IDE and other apps for a while, so they are never allowed. Pick a fixed port below 49152.",
  agent_grants_invalid: "A project allows at most 2 sockets, 4 ports and your temp folder, each once.",
  grant_full: "This project already allows as many extras of that kind as it can.",
  grant_requests_full: "Four access requests of this project are still waiting for the owner.",
  grant_reason_invalid: "Give a reason in plain words.",
  agent_grant_revoked: "An allowed extra this agent held (a socket, a port or your temp folder) was removed, so the agent was stopped. Its next start goes without it.",
};
